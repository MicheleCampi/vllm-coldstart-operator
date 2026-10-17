#!/usr/bin/env bash
# Validate the manifests a reader copies, deploy/examples and the YAML
# blocks of README.md, against the CRDs in deploy/crd.yaml with
# kubeconform -strict. No cluster, same run in CI and locally:
#   bash hack/validate-manifests.sh
# Needs curl, python3 with venv, and network access for the tools pinned
# in hack/validate-manifests.lock, PyYAML and the Kubernetes schemas.
#
# Every ```yaml block in README.md must be a whole manifest (apiVersion
# and kind); fence fragments differently. KEDA's ScaledObject is skipped:
# its CRD belongs to KEDA, not to this operator.
#
# The self-test adds an unknown field to the spec of one example of each
# kind and requires it to be rejected for that reason: a check that cannot
# fail proves nothing.
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
K8S_SCHEMA_VERSION=1.34.12
K8S_SCHEMA_COMMIT=8df8a883b68a24a104b4a9e43c1288090ae60b3b
PYYAML_VERSION=6.0.3
W=$(mktemp -d); trap 'rm -rf "$W"' EXIT
mkdir -p "$W/bin"; export PATH="$W/bin:$PATH"
fail=0
pass() { echo "PASS $1"; }
check() { local n=$1; shift; if "$@" >"$W/$n.log" 2>&1; then pass "$n"; else echo "FAIL $n"; tail -20 "$W/$n.log"; fail=1; fi; }
expect_reject() {
  local n=$1 reason=$2; shift 2
  if "$@" >"$W/$n.log" 2>&1; then echo "FAIL $n: accepted, should have been rejected"; fail=1
  elif grep -q -E "$reason" "$W/$n.log"; then echo "PASS $n: rejected ($(grep -m1 -o -E ".{0,40}$reason.{0,40}" "$W/$n.log"))"
  else echo "FAIL $n: rejected for another reason"; tail -5 "$W/$n.log"; fail=1; fi
}

echo "== tools, verified by SHA-256"
while read -r name url sha; do
  case "$name" in ''|'#'*) continue ;; esac
  f="$W/$(basename "$url")"
  curl -fsSL --retry 3 -o "$f" "$url" || { echo "FAIL download $name"; exit 1; }
  got=$(sha256sum "$f" | cut -d' ' -f1)
  [ "$got" = "$sha" ] || { echo "FAIL checksum $name: expected $sha, got $got"; exit 1; }
  case "$f" in
    *.tar.gz) mkdir -p "$W/$name.x" && tar -xzf "$f" -C "$W/$name.x" && install -m 0755 "$(find "$W/$name.x" -type f -name "$name" | head -1)" "$W/bin/$name" ;;
    *.py)     install -m 0644 "$f" "$W/bin/$name.py" ;;
  esac || { echo "FAIL install $name"; exit 1; }
  echo "ok $name ${sha:0:12}"
done < hack/validate-manifests.lock
python3 -m venv "$W/venv" && "$W/venv/bin/pip" install --quiet --disable-pip-version-check "pyyaml==$PYYAML_VERSION" || { echo "FAIL pyyaml"; exit 1; }
echo "ok pyyaml $PYYAML_VERSION"
PY="$W/venv/bin/python"

echo "== schemas from deploy/crd.yaml"
G=inference.michelecampi.dev
mkdir -p "$W/schemas/$G"
(cd "$W/schemas/$G" && "$PY" "$W/bin/openapi2jsonschema.py" "$ROOT/deploy/crd.yaml" >/dev/null) || { echo "FAIL schemas"; exit 1; }
(cd "$W/schemas" && find . -name '*.json' | sort | sed 's/^/  /')

echo "== manifests"
"$PY" - "$W/readme" <<'PY' || fail=1
import os, re, sys
out = sys.argv[1]; os.makedirs(out)
blocks = re.findall(r'^```yaml\n(.*?)^```', open('README.md').read(), re.M | re.S)
if not blocks:
    sys.exit('FAIL readme: no yaml blocks in README.md')
for i, b in enumerate(blocks, 1):
    if not (re.search(r'^apiVersion: ', b, re.M) and re.search(r'^kind: ', b, re.M)):
        sys.exit(f'FAIL readme: yaml block {i} is not a whole manifest')
    open(f'{out}/block-{i}.yaml', 'w').write(b)
print(f'  {len(blocks)} yaml blocks from README.md')
PY
KC=(kubeconform -strict -summary -skip ScaledObject -kubernetes-version "$K8S_SCHEMA_VERSION"
    -schema-location "https://raw.githubusercontent.com/yannh/kubernetes-json-schema/$K8S_SCHEMA_COMMIT/{{.NormalizedKubernetesVersion}}-standalone{{.StrictSuffix}}/{{.ResourceKind}}{{.KindSuffix}}.json"
    -schema-location "$W/schemas/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json")
check examples "${KC[@]}" deploy/examples
check readme "${KC[@]}" "$W/readme"
grep -h Summary "$W/examples.log" "$W/readme.log" | sed 's/^/  /'

echo "== self-test: an unknown spec field in each kind must be rejected"
for f in ci-placeholder ci-fleet ci-fleet-node; do
  "$PY" - "deploy/examples/$f.yaml" "$W/neg-$f.yaml" <<'PY'
import sys, yaml
d = yaml.safe_load(open(sys.argv[1]))
d["spec"]["zzzUnknown"] = 1
yaml.safe_dump(d, open(sys.argv[2], "w"))
PY
  expect_reject "neg-unknown-field-$f" "additional properties 'zzzUnknown' not allowed" "${KC[@]}" "$W/neg-$f.yaml"
done

if [ "$fail" = 0 ]; then echo "ALL PASS"; else echo "FAILED"; exit 1; fi
