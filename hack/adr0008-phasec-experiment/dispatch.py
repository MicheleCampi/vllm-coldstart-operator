#!/usr/bin/env python3
"""Read the published placement and say where the workload should go.

ADR-0008 D1: this is measurement instrumentation, not a product component.
It ships with the experiment because the experiment is what needs it — the
operator reports where it placed and does not choose endpoints, and putting
this in the operator would give it a concern ADR-0007 D5 deliberately left
to the router layer.

What it does is small on purpose: resolve a FleetService slot to the
in-cluster address of the child serving it, and refuse clearly when the
placement is not ready to receive traffic. The replay driver takes the
address from here and is otherwise unchanged, which is what makes the
level-3 defect — both arms measuring the same fixed endpoint — impossible
to repeat by accident.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys


class NotPlaced(RuntimeError):
    """The fleet has no address to give for this slot, and says why.

    Distinct from a transport error: a caller polling until a placement is
    Ready must tell "not yet" from "the cluster is unreachable", and a bare
    RuntimeError would collapse the two.
    """


def _kubectl(args: list[str]) -> str:
    r = subprocess.run(
        ["kubectl", *args], capture_output=True, text=True, timeout=30
    )
    if r.returncode != 0:
        raise RuntimeError(f"kubectl {' '.join(args)} failed: {r.stderr.strip()}")
    return r.stdout


def resolve(fleet: str, namespace: str, slot: int, port: int = 8000) -> dict:
    """Return the address of the child holding `slot`, with its provenance.

    The provenance travels with the address because a measurement that
    cannot say which placement produced it is the level-3 outcome again:
    numbers that look like a strategy comparison and are not.
    """
    raw = _kubectl(["get", "fleetservice", fleet, "-n", namespace, "-o", "json"])
    status = json.loads(raw).get("status") or {}
    placements = status.get("placements") or []
    if not placements:
        raise NotPlaced(f"{fleet} has published no placements yet")

    if slot >= len(placements):
        raise NotPlaced(
            f"{fleet} has {len(placements)} placement(s); slot {slot} does not exist"
        )
    p = placements[slot]

    phase = p.get("phase")
    if phase != "Ready":
        # Deliberately an error rather than a wait: how long to wait is the
        # caller's policy, and a dispatcher that blocks would hide a fleet
        # that is never going to become Ready.
        raise NotPlaced(
            f"{fleet} slot {slot} is {phase}, not Ready — "
            f"child={p.get('vllmServiceRef')} node={p.get('nodeRef')}"
        )

    child = p.get("vllmServiceRef")
    if not child:
        raise NotPlaced(f"{fleet} slot {slot} is Ready but names no child")

    return {
        "url": f"http://{child}.{namespace}.svc.cluster.local:{port}",
        "child": child,
        "node": p.get("nodeRef"),
        "slot": slot,
        # ADR-0008 D1: what the planner ranked on. None when the operator
        # predates the field or the strategy ranks on nothing — reported as
        # absent rather than defaulted, so a run cannot silently claim inputs
        # it never had.
        "decidedOn": p.get("decidedOn"),
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        prog="dispatch",
        description="Resolve a FleetService slot to the address of the placed replica.",
    )
    ap.add_argument("--fleet", required=True)
    ap.add_argument("--namespace", default="default")
    ap.add_argument("--slot", type=int, default=0)
    ap.add_argument("--port", type=int, default=8000)
    ap.add_argument(
        "--url-only",
        action="store_true",
        help="print just the URL, for use in a shell pipeline",
    )
    a = ap.parse_args(argv)

    try:
        r = resolve(a.fleet, a.namespace, a.slot, a.port)
    except NotPlaced as e:
        print(f"not placed: {e}", file=sys.stderr)
        return 2
    except RuntimeError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1

    if a.url_only:
        print(r["url"])
    else:
        json.dump(r, sys.stdout, indent=2)
        print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
