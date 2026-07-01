// Emits all operator CRDs as multi-document YAML to stdout.
// Usage: cargo run --bin crdgen > deploy/crd.yaml
use kube::CustomResourceExt;
use vllm_coldstart_operator::fleet_types::{FleetService, NodeState};
use vllm_coldstart_operator::VllmService;

fn main() {
    let docs = [
        serde_yaml::to_string(&VllmService::crd()).unwrap(),
        serde_yaml::to_string(&FleetService::crd()).unwrap(),
        serde_yaml::to_string(&NodeState::crd()).unwrap(),
    ];
    print!("{}", docs.join("---\n"));
}
