use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EnvVar, HTTPGetAction, PodSpec, PodTemplateSpec, Probe,
    ResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::{
    api::{Api, ObjectMeta, Patch, PatchParams},
    runtime::{
        controller::{Action, Controller},
        watcher,
    },
    Client, Resource, ResourceExt,
};
use serde_json::json;
use thiserror::Error;
use tracing::{info, warn};

use vllm_coldstart_operator::{phase_for, VllmService, VllmServiceStatus, WarmupStrategy};

const MANAGER: &str = "vllm-coldstart-operator";

#[derive(Debug, Error)]
enum Error {
    #[error("Kube API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("VllmService is missing its namespace")]
    MissingNamespace,
}

struct Context {
    client: Client,
}

fn build_deployment(svc: &VllmService) -> Result<Deployment, Error> {
    let name = svc.name_any();
    let ns = svc.namespace().ok_or(Error::MissingNamespace)?;

    let mut labels = BTreeMap::new();
    labels.insert("app".to_string(), name.clone());
    labels.insert(
        "app.kubernetes.io/managed-by".to_string(),
        MANAGER.to_string(),
    );

    let enforce_eager = matches!(svc.spec.warmup_strategy, WarmupStrategy::Eager);

    // GPU resource limit, only when requested. With gpu=0 (CI / CPU-only)
    // the container requests no GPU and schedules on any node.
    let resources = if svc.spec.gpu > 0 {
        let mut limits = BTreeMap::new();
        limits.insert(
            "nvidia.com/gpu".to_string(),
            Quantity(svc.spec.gpu.to_string()),
        );
        Some(ResourceRequirements {
            limits: Some(limits),
            ..Default::default()
        })
    } else {
        None
    };

    // Readiness probe, built only when health_path is non-empty. This is
    // what makes "Ready" mean "warm": when set, Kubernetes marks the pod
    // ready only once the server answers on this endpoint, so the
    // Warming->Ready transition tracks the real cold start. Inert
    // placeholder images set health_path to "" to disable the probe.
    let readiness_probe = if svc.spec.health_path.is_empty() {
        None
    } else {
        Some(Probe {
            http_get: Some(HTTPGetAction {
                path: Some(svc.spec.health_path.clone()),
                port: IntOrString::Int(8000),
                ..Default::default()
            }),
            initial_delay_seconds: Some(5),
            period_seconds: Some(5),
            failure_threshold: Some(60),
            ..Default::default()
        })
    };
    let container = Container {
        name: "inference".to_string(),
        image: Some(svc.spec.image.clone()),
        ports: Some(vec![ContainerPort {
            container_port: 8000,
            name: Some("http".to_string()),
            ..Default::default()
        }]),
        env: Some(vec![
            EnvVar {
                name: "VLLM_MODEL".to_string(),
                value: Some(svc.spec.model.clone()),
                ..Default::default()
            },
            EnvVar {
                name: "VLLM_ENFORCE_EAGER".to_string(),
                value: Some(enforce_eager.to_string()),
                ..Default::default()
            },
        ]),
        resources,
        readiness_probe,
        ..Default::default()
    };

    let template = PodTemplateSpec {
        metadata: Some(ObjectMeta {
            labels: Some(labels.clone()),
            ..Default::default()
        }),
        spec: Some(PodSpec {
            containers: vec![container],
            ..Default::default()
        }),
    };

    let spec = DeploymentSpec {
        replicas: Some(svc.spec.replicas),
        selector: LabelSelector {
            match_labels: Some(labels.clone()),
            ..Default::default()
        },
        template,
        ..Default::default()
    };

    Ok(Deployment {
        metadata: ObjectMeta {
            name: Some(name),
            namespace: Some(ns),
            labels: Some(labels),
            owner_references: Some(vec![svc.controller_owner_ref(&()).unwrap()]),
            ..Default::default()
        },
        spec: Some(spec),
        ..Default::default()
    })
}

async fn reconcile(svc: Arc<VllmService>, ctx: Arc<Context>) -> Result<Action, Error> {
    let name = svc.name_any();
    let ns = svc.namespace().ok_or(Error::MissingNamespace)?;
    info!(
        "reconciling VllmService '{}' in '{}': model={} replicas={} strategy={:?}",
        name, ns, svc.spec.model, svc.spec.replicas, svc.spec.warmup_strategy
    );

    // 1. Apply the owned Deployment (idempotent server-side apply).
    let deployments: Api<Deployment> = Api::namespaced(ctx.client.clone(), &ns);
    let desired = build_deployment(&svc)?;
    let pp = PatchParams::apply(MANAGER).force();
    let applied = deployments
        .patch(&name, &pp, &Patch::Apply(&desired))
        .await?;

    // 2. Derive the lifecycle phase from the Deployment's ready replicas.
    let desired_replicas = svc.spec.replicas;
    let ready_replicas = applied
        .status
        .as_ref()
        .and_then(|s| s.ready_replicas)
        .unwrap_or(0);
    let (phase, message) = phase_for(desired_replicas, ready_replicas);

    // 3. Write status on the /status subresource (does not retrigger the
    //    spec watcher, so no reconcile loop).
    let services: Api<VllmService> = Api::namespaced(ctx.client.clone(), &ns);
    let status_patch = json!({
        "status": VllmServiceStatus {
            phase: phase.to_string(),
            message,
        }
    });
    services
        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&status_patch))
        .await?;
    info!(
        "status of '{}' -> {} ({}/{} ready)",
        name, phase, ready_replicas, desired_replicas
    );

    // Requeue sooner while warming so the phase converges to Ready quickly;
    // slow steady-state requeue once ready.
    let requeue = if phase == "Ready" { 300 } else { 5 };
    Ok(Action::requeue(Duration::from_secs(requeue)))
}

fn error_policy(_svc: Arc<VllmService>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!("reconcile failed: {}", err);
    Action::requeue(Duration::from_secs(15))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,kube=warn".into()),
        )
        .init();

    let client = Client::try_default().await?;
    let services: Api<VllmService> = Api::all(client.clone());
    let deployments: Api<Deployment> = Api::all(client.clone());
    let context = Arc::new(Context {
        client: client.clone(),
    });

    info!("starting vllm-coldstart-operator controller");

    Controller::new(services, watcher::Config::default())
        .owns(deployments, watcher::Config::default())
        .run(reconcile, error_policy, context)
        .for_each(|res| async move {
            match res {
                Ok((obj, _action)) => info!("reconciled {}", obj.name),
                Err(e) => warn!("reconcile loop error: {:?}", e),
            }
        })
        .await;

    Ok(())
}
