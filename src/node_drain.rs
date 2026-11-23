//! ADR-0028: take this operator's pods off the Service when their node shuts
//! down.
//!
//! On a graceful node shutdown (EKS AL2023 1.34+, nodeadm: 150 s) the
//! kubelet terminates its pods itself, without the API: no
//! deletionTimestamp, so the EndpointSlice keeps a pod `ready` as long as
//! its Ready condition holds. And it holds: the kubelet writes the pod's
//! terminal phase into its status cache when the termination starts, so the
//! readiness worker exits a few seconds later ("Pod is terminated, exiting
//! probe worker") and the last result, Ready, stays until the containers
//! stop (kubernetes/kubernetes#105780, #116965; rehearsed on kind, CI job
//! `node-shutdown`). With the ADR-0019 pre-stop sleep the server was routed
//! until SIGTERM: the request lost in the EKS game day re-run, 21/11.
//!
//! The kubelet does report the shutdown on its Node at once: Ready=False,
//! "node is shutting down". This watcher deletes, through the API, the pods
//! this operator manages on such a node. The deletion sets the
//! deletionTimestamp, so the endpoint stops being `ready` within a second or
//! two and the ADR-0019 sleep drains it, as in a Karpenter drain; the
//! kubelet's own termination goes on, and the Deployment starts the
//! replacement at once. Only that message triggers it: a node NotReady for
//! any other reason (network, kubelet down) is the node lifecycle
//! controller's business, not ours.

use futures::StreamExt;
use k8s_openapi::api::core::v1::{Node, Pod};
use kube::{
    api::{Api, DeleteParams, ListParams},
    runtime::{watcher, WatchStreamExt},
    Client, ResourceExt,
};
use tracing::{info, warn};

/// The kubelet's Ready message while its shutdown manager runs
/// (pkg/kubelet/nodeshutdown, `ShutdownStatus`).
const SHUTTING_DOWN: &str = "node is shutting down";

/// True when the node's kubelet reports a graceful shutdown in progress.
pub fn shutting_down(node: &Node) -> bool {
    node.status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .and_then(|cs| cs.iter().find(|c| c.type_ == "Ready"))
        .is_some_and(|c| {
            c.status != "True" && c.message.as_deref().unwrap_or("").contains(SHUTTING_DOWN)
        })
}

/// The pods to delete: not already deleting (the API path is under way).
pub fn to_delete(pods: &[Pod]) -> Vec<(String, String)> {
    pods.iter()
        .filter(|p| p.metadata.deletion_timestamp.is_none())
        .map(|p| (p.namespace().unwrap_or_default(), p.name_any()))
        .collect()
}

async fn drain(client: &Client, node: &str, manager: &str) {
    let pods: Api<Pod> = Api::all(client.clone());
    let lp = ListParams::default()
        .fields(&format!("spec.nodeName={node}"))
        .labels(&format!("app.kubernetes.io/managed-by={manager}"));
    let list = match pods.list(&lp).await {
        Ok(l) => l.items,
        Err(e) => {
            warn!("node {node} is shutting down; listing its pods failed: {e}");
            return;
        }
    };
    for (ns, name) in to_delete(&list) {
        let api: Api<Pod> = Api::namespaced(client.clone(), &ns);
        // The pod's own grace period: the kubelet's termination already runs
        // the pre-stop hook; the API only adds the deletionTimestamp.
        match api.delete(&name, &DeleteParams::default()).await {
            Ok(_) => info!(
                "node {node} is shutting down: deleted pod {ns}/{name} so it leaves the Service"
            ),
            Err(kube::Error::Api(s)) if s.code == 404 => {}
            Err(e) => warn!("node {node} is shutting down: deleting pod {ns}/{name} failed: {e}"),
        }
    }
}

/// Watch Nodes for the rest of the process; the stream never ends.
pub async fn run(client: Client, manager: &'static str) {
    let nodes: Api<Node> = Api::all(client.clone());
    watcher(nodes, watcher::Config::default())
        .default_backoff()
        .applied_objects()
        .for_each(|res| {
            let client = client.clone();
            async move {
                match res {
                    Ok(node) if shutting_down(&node) => {
                        drain(&client, &node.name_any(), manager).await
                    }
                    Ok(_) => {}
                    Err(e) => warn!("node watch error: {e}"),
                }
            }
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(status: &str, message: &str) -> Node {
        serde_json::from_value(json!({
            "metadata": {"name": "n"},
            "status": {"conditions": [
                {"type": "MemoryPressure", "status": "False"},
                {"type": "Ready", "status": status, "reason": "KubeletNotReady", "message": message}
            ]}
        }))
        .unwrap()
    }

    #[test]
    fn only_a_graceful_shutdown_drains_a_node() {
        // The message the kubelet 1.36 sets (seen on kind, job node-shutdown).
        assert!(shutting_down(&node("False", "node is shutting down")));
        // Ready, or NotReady for another reason: not ours to act on.
        assert!(!shutting_down(&node(
            "True",
            "kubelet is posting ready status"
        )));
        assert!(!shutting_down(&node(
            "False",
            "container runtime network not ready"
        )));
        assert!(!shutting_down(&node(
            "Unknown",
            "Kubelet stopped posting node status."
        )));
        assert!(!shutting_down(
            &serde_json::from_value(json!({"metadata": {"name": "n"}})).unwrap()
        ));
    }

    #[test]
    fn pods_already_deleting_are_left_alone() {
        let pod = |name: &str, deleting: bool| -> Pod {
            let mut m = json!({"name": name, "namespace": "ns"});
            if deleting {
                m["deletionTimestamp"] = json!("2026-11-23T23:16:48Z");
            }
            serde_json::from_value(json!({"metadata": m})).unwrap()
        };
        assert_eq!(
            to_delete(&[pod("a", false), pod("b", true)]),
            vec![("ns".to_string(), "a".to_string())]
        );
    }
}
