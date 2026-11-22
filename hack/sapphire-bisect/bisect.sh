#!/usr/bin/env bash
# Study, not a gate. EKS live A-022 (2026-11-22) measured vLLM CPU v0.34.0
# 1.45x slower at steady state than v0.11.2 on one Intel Xeon Platinum 8488C
# (Sapphire Rapids) node, A/B/A with the same pod and probe; on Xeon 6975P
# (Granite Rapids, CI) the gap is at most ~10 %. This finds the release where
# the step happens, on the CPU where it reproduces, outside Kubernetes.
#
# For each release it runs ../steady-study/study.sh: a fresh eager server on
# cpu0 and its SMT sibling (what the pod on a 2-vCPU m7i saw: request 1 CPU,
# no limit), the operator v0.17.0 warmup, then 40 requests of the game day
# probe. Order: v0.11.2 and v0.34.0 first; if v0.34.0 is not >= 1.3x slower
# it stops (nothing to bisect). Otherwise a binary search on images.txt with
# the midpoint of the two p50s as the threshold, then v0.11.2 again as the
# drift control. Every result line starts with "BISECT " so it can be read
# from the EC2 console output.
#
# Usage: bisect.sh  (env: REPS=2, OUT=out, MIN_RATIO=1.3, MODEL, MODEL_REVISION,
# HF_CACHE). MIN_RATIO lower than 1.3 only to exercise the search in a dry run.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
reps=${REPS:-2} out=${OUT:-out} min_ratio=${MIN_RATIO:-1.3}
mkdir -p "$out"
mapfile -t rel < <(grep -v '^#' "$here/images.txt" | awk '{print $1}')
mapfile -t img < <(grep -v '^#' "$here/images.txt" | awk '{print $2}')
last=$((${#rel[@]} - 1))
declare -A p50

say() { echo "BISECT $*" >&2; }

# measure INDEX [TAG]: median-of-reps steady p50 of release INDEX, cached.
measure() {
  local i=$1 tag=${2:-} r d v vals=()
  if [ -z "$tag" ] && [ -n "${p50[$i]:-}" ]; then return; fi
  for r in $(seq 1 "$reps"); do
    d="$out/${rel[$i]}${tag}-$r"
    if bash "$here/../steady-study/study.sh" "${img[$i]}" eager core "$d" > "$d.log" 2>&1; then
      v=$(python3 "$here/../steady-study/summarize.py" "$d" | python3 -c 'import json,sys; r=json.load(sys.stdin); print(r["p50_s"], r["p95_s"], r["first_s"], r["ready_s"], r["n"], r["failed"])')
      say "run ${rel[$i]}${tag} rep $r: p50 p95 first ready n failed = $v"
      vals+=("${v%% *}")
    else
      say "run ${rel[$i]}${tag} rep $r: FAILED (tail of log follows)"; tail -5 "$d.log" | sed 's/^/BISECT   /'
    fi
  done
  docker rmi -f "${img[$i]}" > /dev/null 2>&1 || true
  [ ${#vals[@]} -gt 0 ] || { say "no result for ${rel[$i]}"; return 1; }
  v=$(printf '%s\n' "${vals[@]}" | sort -n | awk '{a[NR]=$1} END{print (NR%2 ? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2)}')
  [ -n "$tag" ] && { say "${rel[$i]}${tag} p50 $v"; echo "$v"; return; }
  p50[$i]=$v
  say "${rel[$i]} p50 $v"
}

say "cpu: $(sed -n 's/^model name[[:space:]]*: //p' /proc/cpuinfo | head -1), $(nproc) vcpus, $(grep -o -w -E 'amx_bf16|avx512f' /proc/cpuinfo | sort -u | paste -sd, - || true)"
say "releases: ${rel[*]} (reps $reps)"
measure 0; measure "$last"
a=${p50[0]} b=${p50[$last]}
ratio=$(awk -v a="$a" -v b="$b" 'BEGIN{printf "%.3f", b/a}')
say "endpoints: ${rel[0]} $a s, ${rel[$last]} $b s, ratio $ratio"
if awk -v r="$ratio" -v m="$min_ratio" 'BEGIN{exit !(r < m)}'; then
  say "RESULT no step >= ${min_ratio}x on this CPU: nothing to bisect"
else
  thr=$(awk -v a="$a" -v b="$b" 'BEGIN{printf "%.3f", (a+b)/2}')
  say "threshold $thr s (midpoint)"
  lo=0 hi=$last
  while [ $((hi - lo)) -gt 1 ]; do
    mid=$(((lo + hi) / 2))
    measure "$mid"
    if awk -v m="${p50[$mid]}" -v t="$thr" 'BEGIN{exit !(m > t)}'; then hi=$mid; else lo=$mid; fi
    say "step: ${rel[$mid]} ${p50[$mid]} s -> fast up to ${rel[$lo]}, slow from ${rel[$hi]}"
  done
  say "RESULT first slow release ${rel[$hi]} (${p50[$hi]} s), last fast ${rel[$lo]} (${p50[$lo]} s)"
fi
c=$(measure 0 -control | tail -1)
say "control ${rel[0]} again: $c s (first $a s, drift $(awk -v a="$a" -v c="$c" 'BEGIN{printf "%+.1f%%", (c-a)/a*100}'))"
say "measured: $(for i in "${!p50[@]}"; do echo "$i ${rel[$i]}=${p50[$i]}"; done | sort -n | cut -d' ' -f2 | paste -sd' ' -)"
say "DONE"
