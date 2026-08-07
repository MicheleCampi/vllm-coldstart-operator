#!/usr/bin/env python3
"""Soak the operator on kind and record what drifts.

ADR-0008 lists "no soak" among the non-goals, and a soak that reports
"nothing went wrong" proves nothing: absence of evidence. So three
hypotheses, stated before the run and falsifiable from the series this
writes.

  H1  The reconciler does not leak. Operator RSS stays within a declared
      band of its value once the first reconciles have settled. A watch
      that accumulates is the classic controller defect and it does not
      show in a ten-minute test.

  H2  Reconcile count grows linearly in wall-clock, not faster. A requeue
      that feeds itself produces a hot loop measurable only over hours.
      Falsified if the rate over the last quarter of the run exceeds the
      rate over the first quarter by more than a declared factor.

  H3  No child is recreated and no placement is lost. Child UIDs and the
      fleet's placement list are constant for the whole run.

The three are reported separately. H2 failing while H1 holds means a busy
loop that allocates nothing; H1 failing while H2 holds means a leak per
unit time rather than per reconcile. Collapsing them into one verdict
would lose that.

Nothing here interprets: the sampler writes a JSONL series and the
analysis is a separate pass over it, so a run interrupted at hour 30 is
still readable and a threshold changed afterwards cannot quietly rewrite
what was observed.
"""
import argparse
import json
import subprocess
import sys
import time
import urllib.request
from pathlib import Path


def scrape_metrics(url, timeout=5.0):
    """Parse the operator's own OpenMetrics endpoint.

    Returns None on failure rather than zeros: a missed scrape is a gap in
    the series, not an operator that stopped reconciling, and the two must
    not read alike in the analysis.
    """
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            body = r.read().decode("utf-8", "replace")
    except Exception:
        return None
    out = {}
    for line in body.splitlines():
        if line.startswith("#") or not line.strip():
            continue
        try:
            key, val = line.rsplit(None, 1)
        except ValueError:
            continue
        if key.startswith("vcso_reconcile_runs"):
            out["reconcile_runs"] = float(val)
        elif key.startswith("vcso_reconcile_failures"):
            out["reconcile_failures"] = out.get("reconcile_failures", 0.0) + float(val)
        elif key.startswith("vcso_reconcile_duration_sum"):
            out["reconcile_duration_sum"] = float(val)
    return out or None


def proc_rss_kb(pid):
    """RSS from /proc, in kB. None if the process is gone — which is itself
    the observation, and the analysis reports it as a restart rather than
    carrying the last value forward."""
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except Exception:
        return None
    return None


def kubectl_json(args, timeout=20.0):
    """Run kubectl and parse JSON. None on any failure, same reasoning as
    the scrape: a transient API error is a gap, not a state."""
    try:
        r = subprocess.run(["kubectl"] + args, capture_output=True,
                           text=True, timeout=timeout)
        if r.returncode != 0:
            return None
        return json.loads(r.stdout)
    except Exception:
        return None


def cluster_snapshot(fleet):
    """The H3 observables: which children exist, with which UIDs, and what
    the fleet says its placements are."""
    out = {"children": None, "placements": None, "fleet_phase": None}
    vs = kubectl_json(["get", "vllmservice", "-o", "json"])
    if vs is not None:
        out["children"] = sorted(
            (i["metadata"]["name"], i["metadata"]["uid"])
            for i in vs.get("items", [])
        )
    fs = kubectl_json(["get", "fleetservice", fleet, "-o", "json"])
    if fs is not None:
        st = fs.get("status") or {}
        out["fleet_phase"] = st.get("phase")
        out["placements"] = sorted(
            (p.get("vllmServiceRef"), p.get("nodeRef"), p.get("phase"))
            for p in (st.get("placements") or [])
        )
    return out


def sample_loop(a, out_path):
    """Append one JSONL record per interval until the duration elapses.

    Appended, not accumulated in memory and written at the end: a soak
    that loses its series when the box reboots at hour 40 has measured
    nothing, and the failure it would hide is exactly the kind a soak
    exists to catch.
    """
    deadline = time.time() + a.duration_hours * 3600
    n = 0
    with open(out_path, "a", buffering=1) as f:
        while time.time() < deadline:
            rec = {
                "t_unix": time.time(),
                "rss_kb": proc_rss_kb(a.pid),
                "metrics": scrape_metrics(a.metrics_url),
                "cluster": cluster_snapshot(a.fleet),
            }
            f.write(json.dumps(rec) + "\n")
            n += 1
            if n % 12 == 0:
                m = rec["metrics"] or {}
                print(f"[{n}] rss={rec['rss_kb']}kB "
                      f"reconciles={m.get('reconcile_runs')} "
                      f"phase={(rec['cluster'] or {}).get('fleet_phase')}",
                      flush=True)
            time.sleep(a.interval_s)
    print(f"soak finished: {n} samples -> {out_path}")
    return 0


def analyse(path, rss_band_pct, rate_factor, settle_frac):
    """Read the series and report H1, H2, H3 separately.

    Thresholds are parameters with declared defaults rather than constants
    chosen after looking: rss_band_pct and rate_factor are printed in the
    verdict so a reader knows what the pass was measured against.
    """
    recs = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    recs.append(json.loads(line))
                except json.JSONDecodeError:
                    pass  # a torn last line if the run was killed mid-write
    if len(recs) < 8:
        return {"error": f"only {len(recs)} samples, too few to say anything"}

    t0, t1 = recs[0]["t_unix"], recs[-1]["t_unix"]
    hours = (t1 - t0) / 3600.0
    res = {"samples": len(recs), "hours": round(hours, 2),
           "gaps_metrics": sum(1 for r in recs if r["metrics"] is None),
           "gaps_rss": sum(1 for r in recs if r["rss_kb"] is None),
           "thresholds": {"rss_band_pct": rss_band_pct,
                          "rate_factor": rate_factor,
                          "settle_frac": settle_frac}}

    # H1 — RSS after the settling window
    settled = recs[int(len(recs) * settle_frac):]
    rss = [r["rss_kb"] for r in settled if r["rss_kb"] is not None]
    if not rss:
        res["H1"] = "NO DATA: RSS never read after settling"
    else:
        base, hi, lo = rss[0], max(rss), min(rss)
        drift = 100.0 * (rss[-1] - base) / base
        res["rss_first_kb"], res["rss_last_kb"] = base, rss[-1]
        res["rss_min_kb"], res["rss_max_kb"] = lo, hi
        res["rss_drift_pct"] = round(drift, 2)
        res["H1"] = ("HOLDS" if abs(drift) <= rss_band_pct
                     else f"FALSIFIED: RSS drifted {drift:+.1f}% over {hours:.1f}h")

    # H2 — reconcile rate, first quarter against last quarter
    def rate(chunk):
        pts = [(r["t_unix"], r["metrics"]["reconcile_runs"])
               for r in chunk if r["metrics"] and "reconcile_runs" in r["metrics"]]
        if len(pts) < 2 or pts[-1][0] == pts[0][0]:
            return None
        return (pts[-1][1] - pts[0][1]) / (pts[-1][0] - pts[0][0])
    q = max(2, len(recs) // 4)
    r_first, r_last = rate(recs[:q]), rate(recs[-q:])
    res["reconcile_rate_first_per_s"] = r_first
    res["reconcile_rate_last_per_s"] = r_last
    if r_first is None or r_last is None or r_first == 0:
        res["H2"] = "NO DATA: reconcile counter not readable at both ends"
    else:
        ratio = r_last / r_first
        res["reconcile_rate_ratio"] = round(ratio, 3)
        res["H2"] = ("HOLDS" if ratio <= rate_factor
                     else f"FALSIFIED: rate grew {ratio:.2f}x, over the {rate_factor}x bound")

    # H3 — children and placements constant
    seen_children = {tuple(map(tuple, r["cluster"]["children"]))
                     for r in recs if r["cluster"] and r["cluster"]["children"] is not None}
    seen_places = {tuple(map(tuple, r["cluster"]["placements"]))
                   for r in recs if r["cluster"] and r["cluster"]["placements"] is not None}
    res["distinct_child_sets"] = len(seen_children)
    res["distinct_placement_sets"] = len(seen_places)
    if not seen_children:
        res["H3"] = "NO DATA: cluster never read"
    elif len(seen_children) == 1 and len(seen_places) <= 1:
        res["H3"] = "HOLDS"
    else:
        res["H3"] = (f"FALSIFIED: {len(seen_children)} distinct child sets, "
                     f"{len(seen_places)} distinct placement sets — a child was "
                     f"recreated or a placement moved")
    fails = sum(1 for r in recs if r["metrics"] and r["metrics"].get("reconcile_failures"))
    res["samples_with_failures"] = fails
    return res


def main():
    p = argparse.ArgumentParser(
        description="Soak the operator and report H1/H2/H3 (ADR-0008 non-goal).")
    p.add_argument("--out", required=True, help="JSONL series, appended")
    p.add_argument("--pid", type=int, help="operator PID, for RSS. Required unless --analyse-only")
    p.add_argument("--fleet", default="ci-fleet", help="FleetService to watch")
    p.add_argument("--metrics-url", default="http://127.0.0.1:8080/metrics",
                   help="the operator's own OpenMetrics endpoint (src/main.rs http::serve)")
    p.add_argument("--duration-hours", type=float, default=48.0)
    p.add_argument("--interval-s", type=float, default=300.0,
                   help="sampling cadence. 5 min over 48h is ~576 samples: "
                        "enough to see a trend, few enough that the sampler "
                        "is not itself load on the API server")
    p.add_argument("--rss-band-pct", type=float, default=15.0,
                   help="H1 threshold: RSS drift tolerated after settling. "
                        "Declared here so the verdict names what it passed "
                        "against")
    p.add_argument("--rate-factor", type=float, default=1.5,
                   help="H2 threshold: how much the reconcile rate may grow "
                        "from the first quarter to the last")
    p.add_argument("--settle-frac", type=float, default=0.1,
                   help="fraction of the run discarded before judging H1: "
                        "the first reconciles allocate caches that are not a leak")
    p.add_argument("--analyse-only", action="store_true")
    a = p.parse_args()

    if a.analyse_only:
        res = analyse(a.out, a.rss_band_pct, a.rate_factor, a.settle_frac)
        print(json.dumps(res, indent=1))
        return 0 if "error" not in res else 1

    if a.pid is None:
        sys.exit("--pid is required unless --analyse-only")
    if proc_rss_kb(a.pid) is None:
        sys.exit(f"pid {a.pid} is not readable in /proc — wrong pid, or the "
                 f"operator is not running")
    if scrape_metrics(a.metrics_url) is None:
        sys.exit(f"no metrics at {a.metrics_url} — the operator serves them on "
                 f"0.0.0.0:8080; without them H2 has no observable and the "
                 f"soak would run for hours to say nothing")
    print(f"soaking for {a.duration_hours}h, sampling every {a.interval_s}s "
          f"-> {a.out}")
    rc = sample_loop(a, Path(a.out))
    print(json.dumps(analyse(a.out, a.rss_band_pct, a.rate_factor, a.settle_frac), indent=1))
    return rc


if __name__ == "__main__":
    sys.exit(main())
