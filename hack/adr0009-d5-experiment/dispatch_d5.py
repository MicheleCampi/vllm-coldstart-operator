"""Multi-target workload dispatcher for the ADR-0009 D5 experiment.
Stdlib only, deriving its pacing and per-request accounting from
hack/gpu-session/adr0007-ea-experiment/replay.py.

Why a separate program rather than a flag on replay.py: replay.py takes a
single --url, and here the set of destinations is not fixed for the duration
of the run. It is exactly what the consumer varies, so it is the thing under
test. Replay.py's output is committed evidence of the ADR-0007 experiment;
it is left byte-identical.

Target discovery is the consumer side of ADR-0008 D1: the operator publishes
placements in FleetService.status, and this dispatcher — instrumentation-only,
living in hack/ — reads them. Endpoints are derived, not guessed:
build_service() in src/main.rs names the Service after the child VllmService
(the string carried in placement.vllmServiceRef) and exposes port 8000, so
`<vllmServiceRef>:8000` is a deterministic consequence of the operator's own
naming, not a coincidence of this cluster.

Reachability is via `kubectl proxy` on localhost: child Services are
ClusterIP, and a single long-lived proxy keeps the dispatcher on the host
next to consumer.py with no image to build, no RBAC to grant, and no
per-target port-forward to die mid-run. Targets vary only in the request
path. The latency the proxy adds does not touch the primary metric, which is
peak replicas.

The send schedule is open-loop: arrival times come from the frozen trace and
nothing the cluster does feeds back into them. With zero Ready targets a
request fails immediately (status -1, error no_ready_target) rather than
waiting for one, because waiting would make the load generator's timing a
function of the arm under test.
"""
import argparse
import concurrent.futures
import itertools
import json
import sys
import threading
import time
import urllib.error
import urllib.request

POLL_INTERVAL_S = 2.0
READY_PHASE = "Ready"
CHILD_PORT = 8000


class TargetSet:
    """Ready placements, refreshed in the background from FleetService.status.

    Round-robin over whatever is currently Ready. The selection policy is
    identical in both arms; only the set differs, and the set is the SUT.
    """

    def __init__(self, proxy, namespace, fleet):
        self._url = (
            f"{proxy}/apis/inference.michelecampi.dev/v1alpha1/namespaces/"
            f"{namespace}/fleetservices/{fleet}"
        )
        self._proxy = proxy
        self._namespace = namespace
        self._lock = threading.Lock()
        self._ready = []
        self._cycle = itertools.count()
        self._stop = threading.Event()
        self._polls = 0
        self._poll_errors = 0

    def endpoint(self, child):
        return (
            f"{self._proxy}/api/v1/namespaces/{self._namespace}/services/"
            f"{child}:{CHILD_PORT}/proxy/v1/completions"
        )

    def refresh(self):
        try:
            with urllib.request.urlopen(self._url, timeout=10) as resp:
                obj = json.loads(resp.read())
        except Exception:
            with self._lock:
                self._poll_errors += 1
            return
        placements = (obj.get("status") or {}).get("placements") or []
        ready = [
            p["vllmServiceRef"]
            for p in placements
            if p.get("phase") == READY_PHASE and p.get("vllmServiceRef")
        ]
        ready.sort()
        with self._lock:
            self._ready = ready
            self._polls += 1

    def run(self):
        while not self._stop.is_set():
            self.refresh()
            self._stop.wait(POLL_INTERVAL_S)

    def stop(self):
        self._stop.set()

    def pick(self):
        """Return (child_name, n_ready) or (None, 0) if nothing is Ready."""
        with self._lock:
            ready = self._ready
            if not ready:
                return None, 0
            return ready[next(self._cycle) % len(ready)], len(ready)

    def stats(self):
        with self._lock:
            return {"polls": self._polls, "poll_errors": self._poll_errors}


def do_request(url, item, timeout):
    t0 = time.monotonic()
    try:
        body = json.dumps(
            {
                "model": item["model"],
                "prompt": item["prompt"],
                "max_tokens": item["max_tokens"],
                "temperature": 0.0,
            }
        ).encode()
        req = urllib.request.Request(
            url, data=body, headers={"Content-Type": "application/json"}
        )
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            ttfb = (time.monotonic() - t0) * 1000
            resp.read()
            total = (time.monotonic() - t0) * 1000
            return {
                "status": resp.status,
                "ttfb_ms": round(ttfb, 1),
                "total_ms": round(total, 1),
            }
    except urllib.error.HTTPError as e:
        return {
            "status": e.code,
            "ttfb_ms": None,
            "total_ms": round((time.monotonic() - t0) * 1000, 1),
        }
    except Exception as e:
        return {
            "status": -1,
            "error": type(e).__name__,
            "ttfb_ms": None,
            "total_ms": round((time.monotonic() - t0) * 1000, 1),
        }


def send(targets, item, timeout):
    child, n_ready = targets.pick()
    if child is None:
        return {
            "status": -1,
            "error": "no_ready_target",
            "ttfb_ms": None,
            "total_ms": 0.0,
            "target": None,
            "n_ready_at_send": 0,
        }
    r = do_request(targets.endpoint(child), item, timeout)
    r["target"] = child
    r["n_ready_at_send"] = n_ready
    return r


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--workload", required=True)
    ap.add_argument("--fleet", required=True)
    ap.add_argument("--namespace", default="default")
    ap.add_argument("--proxy", default="http://127.0.0.1:8001")
    ap.add_argument("--model", required=True)
    ap.add_argument("--timeout-s", type=float, default=60.0)
    ap.add_argument("--out", required=True)
    ap.add_argument("--max-workers", type=int, default=256)
    args = ap.parse_args()

    items = [json.loads(line) for line in open(args.workload)]
    for item in items:
        item.setdefault("model", args.model)

    targets = TargetSet(args.proxy, args.namespace, args.fleet)
    targets.refresh()
    if not targets.pick()[0]:
        print(
            "fatal: no Ready placement at start; is the fleet up and the "
            "proxy running?",
            file=sys.stderr,
        )
        return 2
    poller = threading.Thread(target=targets.run, daemon=True)
    poller.start()

    results = [None] * len(items)
    t_start = time.monotonic()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.max_workers) as ex:
        futures = {}
        for i, item in enumerate(items):
            delay = item["t_offset_s"] - (time.monotonic() - t_start)
            if delay > 0:
                time.sleep(delay)
            t_send = time.monotonic() - t_start
            fut = ex.submit(send, targets, item, args.timeout_s)
            futures[fut] = (i, t_send)
        for fut in concurrent.futures.as_completed(futures):
            i, t_send = futures[fut]
            r = fut.result()
            r.update({"i": i, "t_send_s": round(t_send, 3)})
            results[i] = r
    targets.stop()

    with open(args.out, "w") as f:
        for r in results:
            f.write(json.dumps(r) + "\n")

    ok = sum(1 for r in results if r and r["status"] == 200)
    starved = sum(1 for r in results if r and r.get("error") == "no_ready_target")
    per_target = {}
    for r in results:
        if r and r.get("target"):
            per_target[r["target"]] = per_target.get(r["target"], 0) + 1
    wall = time.monotonic() - t_start
    st = targets.stats()
    print(
        f"dispatched={len(items)} ok={ok} fail={len(items) - ok} "
        f"starved={starved} wall_s={wall:.1f} polls={st['polls']} "
        f"poll_errors={st['poll_errors']}",
        file=sys.stderr,
    )
    print(f"per_target={json.dumps(per_target, sort_keys=True)}", file=sys.stderr)
    return 0 if ok == len(items) else 1


if __name__ == "__main__":
    sys.exit(main())
