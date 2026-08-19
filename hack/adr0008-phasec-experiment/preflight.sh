#!/usr/bin/env bash
# ADR-0008 phase C pre-flight. Three checks with declared abort criteria,
# because discovering any of them mid-run wastes the session.
#
# Run this after the cluster is up and before the first measured rep. It
# spends about a minute and can save an hour.
set -uo pipefail
FAIL=0
ok(){ echo "  PASS: $*"; }
bad(){ echo "  ABORT: $*"; FAIL=$((FAIL+1)); }

NODES="${NODES:?set NODES to the space-separated node list}"
HORIZON="${HORIZON:-600}"
DRAIN="${DRAIN:-120}"

echo "=== 1. the cap can be set on every candidate node ==="
# Gate 3 tested one instance. A cap that works on one node and not another
# would silently turn the asymmetry off for some reps.
for n in $NODES; do
  out="$(ssh "$n" 'CUR=$(nvidia-smi --query-gpu=power.limit --format=csv,noheader,nounits | tr -d " ");
                   sudo nvidia-smi -pl 100 >/dev/null 2>&1 && \
                   GOT=$(nvidia-smi --query-gpu=power.limit --format=csv,noheader,nounits | tr -d " ") && \
                   sudo nvidia-smi -pl "$CUR" >/dev/null 2>&1 && echo "$GOT"' 2>/dev/null)"
  if [ "${out%%.*}" = "100" ]; then ok "$n accepts a 100W cap and restores"
  else bad "$n did not take the cap (got '${out:-nothing}') — the asymmetry cannot be manufactured there"; fi
done

echo
echo "=== 2. the horizon outlives the drain ==="
# Constraint P means the signal informing a placement is always measured
# before the node is freed. A horizon shorter than the drain expires every
# signal before the decision, and both arms collapse onto the same fallback —
# which is the level-3 failure in a new costume.
if [ "$HORIZON" -gt "$DRAIN" ]; then
  ok "horizon ${HORIZON}s > drain ${DRAIN}s (margin $((HORIZON - DRAIN))s)"
else
  bad "horizon ${HORIZON}s <= drain ${DRAIN}s: every signal expires before the decision"
fi

echo
echo "=== 3. the dispatcher resolves to something that answers ==="
FLEET="${FLEET:-phasec-probe}"
URL="$(python3 "$(dirname "$0")/dispatch.py" --fleet "$FLEET" --url-only 2>/dev/null)"
if [ -z "$URL" ]; then
  bad "dispatcher returned no URL for '$FLEET' — placement not Ready, or no placement at all"
else
  code="$(kubectl run preflight-probe-$$ --rm -i --restart=Never --quiet \
          --image=curlimages/curl:8.11.1 -- \
          -s -o /dev/null -w '%{http_code}' --max-time 10 "${URL}/v1/models" 2>/dev/null)"
  if [ "$code" = "200" ]; then ok "$URL answered 200"
  else bad "$URL did not answer (got '${code:-nothing}') — traffic would not reach the placement"; fi
fi

echo
if [ "$FAIL" -eq 0 ]; then echo "PRE-FLIGHT: PASS — the session may proceed"; exit 0; fi
echo "PRE-FLIGHT: ABORT ($FAIL check(s) failed) — do not spend arms"
exit 1
