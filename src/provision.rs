//! D-0145: scale-up from zero. When a fleet has no candidate node and its
//! nodePool.provisionOnEmpty is set, a pause Pod requesting one GPU, with
//! the pool's nodeSelector, asks the cluster autoscaler for a node. It is
//! owned by the fleet and removed as soon as a candidate exists, so the GPU
//! it holds is freed before the fleet's own children are placed.
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use kube::{Client, ResourceExt};
use serde_json::json;
use std::collections::BTreeMap;

pub const PROVISION_MANAGER: &str = "vllm-coldstart-operator-provision";
pub const PAUSE_IMAGE: &str = "registry.k8s.io/pause:3.10";

pub fn placeholder_name(fleet: &str) -> String {
    format!("{fleet}-provision")
}

pub fn placeholder_pod(
    fleet: &str,
    ns: &str,
    owner: OwnerReference,
    selector: &BTreeMap<String, String>,
) -> Pod {
    let v = json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {
            "name": placeholder_name(fleet), "namespace": ns,
            "labels": {
                "app.kubernetes.io/managed-by": "vllm-coldstart-operator",
                "inference.michelecampi.dev/provision-for": fleet
            },
            "ownerReferences": [owner]
        },
        "spec": {
            "nodeSelector": selector,
            "tolerations": [{"key": "nvidia.com/gpu", "operator": "Exists", "effect": "NoSchedule"}],
            "terminationGracePeriodSeconds": 0,
            "containers": [{
                "name": "pause", "image": PAUSE_IMAGE,
                "resources": {
                    "requests": {"nvidia.com/gpu": "1", "cpu": "10m", "memory": "16Mi"},
                    "limits": {"nvidia.com/gpu": "1", "cpu": "10m", "memory": "16Mi"}
                }
            }]
        }
    });
    serde_json::from_value(v).expect("static Pod shape")
}

pub async fn ensure(client: Client, ns: &str, pod: &Pod) -> Result<(), kube::Error> {
    let api: Api<Pod> = Api::namespaced(client, ns);
    let pp = PatchParams::apply(PROVISION_MANAGER).force();
    api.patch(&pod.name_any(), &pp, &Patch::Apply(pod))
        .await
        .map(|_| ())
}

pub async fn remove(client: Client, ns: &str, fleet: &str) -> Result<(), kube::Error> {
    let api: Api<Pod> = Api::namespaced(client, ns);
    match api
        .delete(&placeholder_name(fleet), &DeleteParams::default())
        .await
    {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> OwnerReference {
        OwnerReference {
            api_version: "inference.michelecampi.dev/v1alpha1".into(),
            kind: "FleetService".into(),
            name: "f".into(),
            uid: "u".into(),
            controller: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn placeholder_requests_one_gpu_on_the_pool() {
        let sel = BTreeMap::from([(
            "cloud.google.com/gke-nodepool".to_string(),
            "gpu-pool".to_string(),
        )]);
        let p = placeholder_pod("f", "team-demo", owner(), &sel);
        assert_eq!(p.name_any(), "f-provision");
        assert_eq!(p.namespace().as_deref(), Some("team-demo"));
        let spec = p.spec.unwrap();
        assert_eq!(spec.node_selector, Some(sel));
        let r = spec.containers[0].resources.clone().unwrap();
        assert_eq!(r.limits.unwrap()["nvidia.com/gpu"].0, "1");
        assert_eq!(
            spec.tolerations.unwrap()[0].key.as_deref(),
            Some("nvidia.com/gpu")
        );
    }

    #[test]
    fn placeholder_is_owned_by_the_fleet() {
        let p = placeholder_pod("f", "ns", owner(), &BTreeMap::new());
        let o = p.metadata.owner_references.unwrap();
        assert_eq!(o[0].kind, "FleetService");
        assert_eq!(o[0].controller, Some(true));
    }
}
