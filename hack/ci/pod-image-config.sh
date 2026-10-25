#!/usr/bin/env bash
# Print the config digest of the image behind a pod's imageID, as the kind
# node's containerd sees it. On containerd 1.x (kind v0.31) the kubelet's
# imageID is the config digest itself; on containerd 2 (kind v0.33) it is the
# repo digest containerd made on import, docker.io/library/import-<date>@sha256:...
# Exactly one image on the node must carry that repo digest.
# Usage: pod-image-config.sh <kind node container> <imageID>
set -euo pipefail
node=$1 id=$2
if [[ $id =~ ^sha256:[0-9a-f]{64}$ ]]; then echo "$id"; exit 0; fi
[[ $id == *@sha256:* ]] || { echo "imageID '$id' is neither a config digest nor a repo digest" >&2; exit 1; }
docker exec "$node" crictl images -o json | python3 -I -c '
import sys, json
d = sys.argv[1]
m = [i["id"] for i in json.load(sys.stdin)["images"] if any(r.endswith("@" + d) for r in i["repoDigests"])]
if len(m) != 1:
    sys.exit("%d images on the node carry %s" % (len(m), d))
print(m[0])' "${id##*@}"
