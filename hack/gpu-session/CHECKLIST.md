# Item-4 GPU session — go/no-go checklist

Budget: EUR 50 hard cap. Estimate $12-18, 2.5-3h wall clock.
Topology: 3x single-GPU Lambda (A10 preferred ~$1.29/hr; fallback A100 40GB
~$1.99/hr — verify price/availability in dashboard at launch).
Claim under test: service continuity + bounded recovery + zero cascade
(delta-based vs baseline, NOT absolute-zero). 3 repetitions.

## Phase 0 — before spending (optim-dev, zero cost)
- [ ] `origin/main` green, working tree clean
- [ ] musl binary rebuilt from HEAD: `ldd` says "statically linked"
- [ ] `hack/loadgen/loadgen.py`, `hack/rehearsal/analyze.py` present
- [ ] this directory's scripts pass `bash -n`

## Phase 1 — launch (dashboard)
- [ ] 3 instances, same region; note $/hr actually charged
- [ ] fill `00-env.sh` (IPs, SSH key), `source hack/gpu-session/00-env.sh`
- [ ] sanity per node: `ssh_X nvidia-smi` shows the GPU;
      `ssh_X nvidia-ctk --version` (toolkit preinstalled by Lambda Stack)
- ABORT if: no capacity in one region for 3 nodes (try 1 other region,
  then stop — do not shop regions for an hour at 3x $/hr)

## Phase 2 — cluster (target: <=25 min from launch)
- [ ] `01-server.sh`, then `02-agent.sh`
- [ ] 3 Ready nodes; `nvidia.com/gpu: 1` allocatable on each
- [ ] `03-prepull.sh` (parallel; ~10 min: image ~10GB + model ~15GB per node)
- ABORT if: GPU not allocatable after device-plugin restart + 15 min of
  debugging. Teardown, take notes, retry another day — do not burn the
  budget on cluster plumbing.

## Phase 3 — deploy + smoke (target: <=20 min)
- [ ] `04-deploy.sh`; operator systemd unit active, logs clean
- [ ] map A/B/C roles to real node names; seed warmth (A,B Warm 0.0;
      C Warm 0.3; all `isSpotInstance: true`)
- [ ] `kubectl apply -f hack/gpu-session/fleet-gpu.yaml`
- [ ] both children Ready; `curl <node>:30800/v1/models` answers on both
      NodePorts (any node IP works, kube-proxy routes)
- [ ] warmth check: placements landed on A and B (C stayed spare)
- ABORT if: vLLM pods CrashLoop twice with the same error after one
  targeted fix attempt (likely --gpu-memory-utilization or driver issue;
  one retry with 0.85, then stop)

## Phase 4 — measured runs (3 repetitions)
Per repetition:
- [ ] start loadgen against both NodePorts (closed-loop, JSONL out)
- [ ] steady state >=60s on both services
- [ ] inject notice on B:
      `kubectl patch nodestate <B> --subresource=status --type=merge
       -p '{"status":{"spot":{"preemptionNoticeDetected":true,
       "preemptionNoticeTime":"<RFC3339 now>"}}}'`
- [ ] wait for surge-first completion: replacement Ready on C BEFORE
      drain of B (operator logs + placement timeline)
- [ ] keep loadgen running >=60s post-recovery, stop, save JSONL + logs
- [ ] `analyze.py` on the run; sanity: unaffected service error count == 0
- [ ] reset between reps: notice=false, wait fleet stable, re-seed warmth
      (B back to Warm/0.0 spare or rotate roles B<->C, note the choice)
- NOTE: detection path out of scope (notice injected via status patch),
  disclosed in PROVENANCE — do not oversell in writeup.

## Phase 5 — evidence off-box BEFORE teardown
- [ ] scp all JSONL + operator logs + `kubectl get events` dump to
      optim-dev `hack/gpu-session/runs/<date>/`
- [ ] `kubectl get vllmservice,fleetservice,nodestate -o yaml` snapshot
- [ ] nvidia-smi + k3s version + image digest recorded (PROVENANCE)
- [ ] only then: terminate all 3 instances in dashboard
- [ ] verify billing stopped (dashboard shows terminated)

## Abort = teardown + notes. The rehearsal already proves the mechanics;
## a failed GPU session costs money but no claims. Never leave nodes
## running while debugging on optim-dev.
