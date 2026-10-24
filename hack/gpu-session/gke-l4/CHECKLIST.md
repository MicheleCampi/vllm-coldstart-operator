# GPU calibration on one L4: session checklist

The commands of PLAN.md, in order. Every command runs on optim-dev from
/root/capstone-iac unless it says otherwise. T0 is the start of
`terraform apply`; the hard stop is T0 + 4 h (BUDGET B-001).

## Phase 0: before spending (0 EUR, done 2026-10-24)

- PLAN.md merged before the run (#49); run-load.sh rehearsed on kind (#50).
- gcloud project and account active; secret `grafana-cloud-remote-write`
  present, readable by `eso-grafana-reader`; deploy key on optim-dev equal
  to the read-only key on GitHub; gke main at bc368ad (operator v0.6.0).

## Phase 1: cluster (target: T0 + 20 min)

    E=/root/gpu-evidence/gke-l4-$(date -u +%Y%m%d); mkdir -p "$E"
    export TF_VAR_authorized_networks='[{cidr_block="'"$MYIP"'/32",display_name="operator"}]'
    date -u +%FT%TZ > "$E/t0-apply"
    terraform -chdir=terraform init && terraform -chdir=terraform apply
    gcloud container clusters get-credentials capstone-inference \
      --region europe-west4 --project rosy-precinct-477218-u8

Gate A, before anything can ask for a GPU:

    gcloud container clusters describe capstone-inference --region europe-west4 \
      --format='value(releaseChannel.channel,currentMasterVersion)' | tee "$E/cluster-version"
    kubectl get nodes -L cloud.google.com/gke-nodepool -o wide | tee "$E/nodes-gate-a"

Pass: channel REGULAR (not EXTENDED, which B-001 prices at 0.44 EUR/h
more), every node Ready, no node in gpu-pool. Fail: Phase 5 now.

## Phase 2: platform and cold start (target: T0 + 60 min)

The Workload Identity binding, ArgoCD, the deploy key: README "Run it",
unchanged. Then:

    date -u +%FT%TZ > "$E/t0-root-app"; kubectl apply -f argocd/root-app.yaml
    # ArgoCD creates the VllmService minutes later; kubectl wait fails at
    # once on an object that does not exist yet.
    for i in $(seq 1 90); do
      kubectl get vllmservice qwen-7b -n vllm-system >/dev/null 2>&1 && break
      sleep 10
    done
    date -u +%FT%TZ > "$E/t-vllmservice-created"
    kubectl wait vllmservice/qwen-7b -n vllm-system \
      --for=jsonpath='{.status.phase}'=Ready --timeout=45m
    date -u +%FT%TZ > "$E/t1-ready"
    for k in events pods nodes; do
      kubectl get $k -A -o json > "$E/coldstart1-$k.json"
    done

Second cold start, image already on the node:

    date -u +%FT%TZ > "$E/t0-repod"; kubectl delete pod -n vllm-system -l app=qwen-7b
    kubectl wait vllmservice/qwen-7b -n vllm-system \
      --for=jsonpath='{.status.phase}'=Ready --timeout=30m
    date -u +%FT%TZ > "$E/t1-repod"
    for k in events pods nodes; do
      kubectl get $k -A -o json > "$E/coldstart2-$k.json"
    done

Both intervals are split afterwards from the events and pod conditions
copied at each Ready: events expire after 1 h, so Phase 4 is too late
for them. Event timestamps have a 1 s resolution.

Stop rule, capacity: if the pod is still Pending 10 min after the
VllmService appears and the events show FailedScaleUp, copy events,
pods, nodes and VllmServices to "$E/stockout-*.json" and go to Phase 5;
a stockout has no end time. Read the cluster from a second terminal with
its own kubeconfig (gcloud container clusters get-credentials into a
separate KUBECONFIG file) and E exported again. Seen on 2026-10-24 in
europe-west4-a, before the GPU pool spanned three zones.

## Phase 3: measured runs (target: T0 + 3 h 15 min)

From /root/vllm-coldstart-operator. Only the records are collected here;
slo_check and the analysis run after the destroy, at 0 EUR.

    G=hack/gpu-session/gke-l4/run-load.sh; M=Qwen/Qwen2.5-7B-Instruct
    for r in 1 2 3; do bash $G replay-$r 0.025 42 $M "$E"; done
    bash $G d3-005-s42 0.05 42 $M "$E"
    bash $G d3-005-s1 0.05 1 $M "$E"
    bash $G d3-005-s2 0.05 2 $M "$E"
    bash $G d3-0075-s42 0.075 42 $M "$E"
    bash $G d3-01-s42 0.1 42 $M "$E"

At T0 + 3 h 15 min the remaining rows are skipped, and the skip is
written down.

## Phase 4: evidence (target: 10 min)

    for k in events pods vllmservices nodes; do kubectl get $k -A -o json > "$E/$k.json"; done
    kubectl logs -n vllm-system -l app.kubernetes.io/name=vllm-coldstart-operator --tail=-1 > "$E/operator.log"
    kubectl logs -n vllm-system -l app=qwen-7b --tail=-1 > "$E/vllm.log"
    kubectl exec -n vllm-system deploy/qwen-7b -- sh -c \
      'nvidia-smi || /usr/local/nvidia/bin/nvidia-smi' > "$E/nvidia-smi.txt" || true
    kubectl get applications -n argocd -o json > "$E/argocd-apps.json"

## Phase 5: destroy (by T0 + 4 h)

    terraform -chdir=terraform destroy
    gcloud container clusters list --format='value(name)' | tee "$E/after-clusters"
    gcloud compute instances list --format='value(name)' | tee "$E/after-instances"
    date -u +%FT%TZ > "$E/t-destroyed"

Pass: both lists empty. The hours between t0-apply and t-destroyed go
into BUDGET B-001 as spent, at the estimate, until billing shows the
real figure.
