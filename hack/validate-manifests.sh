#!/usr/bin/env bash
# Validate the manifests a reader copies, deploy/examples and the YAML
# blocks of README.md, against the CRDs in deploy/crd.yaml with
# kubeconform -strict. No cluster, same run in CI and locally:
#   bash hack/validate-manifests.sh
# Needs curl, git, python3 with venv, and network access for
# inference-platform-ci (pinned in hack/platform-ci.env), the tools and
# PyYAML it pins, and the Kubernetes schemas.
#
# Every ```yaml block in README.md must be a whole manifest (apiVersion
# and kind); fence fragments differently. KEDA's ScaledObject and the HTTP
# Add-on's InterceptorRoute are skipped: their CRDs belong to KEDA, not to
# this operator. The scale-from-zero job applies the InterceptorRoute to a
# cluster that has the add-on's CRD.
#
# The self-test adds an unknown field to the spec of one example of each
# kind and requires it to be rejected for that reason: a check that cannot
# fail proves nothing.
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
W=$(mktemp -d); trap 'rm -rf "$W"' EXIT
mkdir -p "$W/bin"; export PATH="$W/bin:$PATH"
fail=0

echo "== inference-platform-ci at the tag pinned in hack/platform-ci.env"
# The tool pins, installers, helpers (pass, check, expect_reject) and the
# Kubernetes schema pins come from there. The tag must resolve to the pinned
# commit before anything from it runs, so this check cannot come from its
# own lib.sh. PLATFORM_CI_ENV replaces hack/platform-ci.env, for testing
# that a wrong pin fails.
. "${PLATFORM_CI_ENV:-hack/platform-ci.env}"
P=$W/platform-ci
git init -q "$P" && git -C "$P" fetch -q --depth 1 https://github.com/MicheleCampi/inference-platform-ci.git "refs/tags/$PLATFORM_CI_TAG:refs/tags/$PLATFORM_CI_TAG" || { echo "FAIL fetch inference-platform-ci $PLATFORM_CI_TAG"; exit 1; }
got=$(git -C "$P" rev-parse --verify -q "refs/tags/$PLATFORM_CI_TAG^{commit}")
[ "$got" = "$PLATFORM_CI_COMMIT" ] || { echo "FAIL inference-platform-ci $PLATFORM_CI_TAG resolves to $got, not the pinned $PLATFORM_CI_COMMIT"; exit 1; }
git -C "$P" checkout -q "$PLATFORM_CI_COMMIT" || { echo "FAIL checkout inference-platform-ci"; exit 1; }
echo "ok inference-platform-ci ${PLATFORM_CI_COMMIT:0:12}"
. "$P/lib.sh" && . "$P/versions.env" || { echo "FAIL lib.sh or versions.env"; exit 1; }

echo "== tools and PyYAML, verified by SHA-256"
bash "$P/install.sh" "$W/bin" "$P/tools.lock" kubeconform openapi2jsonschema || { echo "FAIL tools"; exit 1; }
bash "$P/install-python.sh" "$W/venv" "$P/requirements.lock" || { echo "FAIL pyyaml"; exit 1; }
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
KC=(kubeconform -strict -summary -skip ScaledObject,InterceptorRoute -kubernetes-version "$K8S_SCHEMA_VERSION"
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
