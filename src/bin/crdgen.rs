// Emits the VllmService CRD as YAML to stdout.
// Usage: cargo run --bin crdgen > deploy/crd.yaml
use kube::CustomResourceExt;
use vllm_coldstart_operator::VllmService;

fn main() {
    print!("{}", serde_yaml::to_string(&VllmService::crd()).unwrap());
}
