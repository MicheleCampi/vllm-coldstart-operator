use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use k8s_openapi::api::apps::v1::{
    Deployment, DeploymentSpec, DeploymentStrategy, RollingUpdateDeployment,
};
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EmptyDirVolumeSource, EnvVar, HTTPGetAction, PodSpec,
    PodTemplateSpec, Probe, ResourceRequirements, Volume, VolumeMount,
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

use vllm_coldstart_operator::{
    metrics::Metrics, phase_for, VllmService, VllmServiceStatus, WarmupStrategy,
};

mod fleet_controller;

const MANAGER: &str = "vllm-coldstart-operator";

#[derive(Debug, Error)]
enum Error {
    #[error("Kube API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("VllmService is missing its namespace")]
    MissingNamespace,
}

impl Error {
    /// Stable, low-cardinality label for the failures metric.
    fn metric_label(&self) -> &'static str {
        match self {
            Error::Kube(_) => "kube",
            Error::MissingNamespace => "missing_namespace",
        }
    }
}

struct Context {
    client: Client,
    metrics: Metrics,
}

/// Drain window for serving pods, in seconds. On termination the pod leaves
/// Service endpoints immediately (new requests route to the replacement);
/// in-flight generations get this long to complete after SIGTERM before the
/// kubelet kills the pod. Bounds the tail of a make-before-break move.
/// Deliberately a constant, not a spec field: the drain window is operator
/// policy in v1 (same YAGNI stance as ADR-0005's deferred hysteresis).
const TERMINATION_GRACE_SECONDS: i64 = 120;

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

    // A request for GPUs is the signal that this is a real vLLM workload
    // rather than the inert CI placeholder (gpu=0, pause image). Only then
    // do we inject the serving command, the GPU limit, the shared-memory
    // volume and any tuning args; the placeholder stays a do-nothing pod.
    let serving = svc.spec.gpu > 0;

    // GPU resource limit, only when requested. With gpu=0 (CI / CPU-only)
    // the container requests no GPU and schedules on any node.
    let resources = if serving {
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

    // Invocation. The vllm/vllm-openai image's entrypoint is `vllm serve`,
    // but we set command+args explicitly so the operator does not depend on
    // the image's entrypoint staying stable across tags. `vllm serve` takes
    // the model as a positional argument; warmupStrategy maps to
    // --enforce-eager (CUDA graphs off => faster cold start); extraArgs
    // carries per-deployment engine tuning (e.g. --max-model-len,
    // --gpu-memory-utilization). The placeholder keeps the pause entrypoint.
    let (command, args) = if serving {
        let mut a = vec![
            svc.spec.model.clone(),
            "--host".to_string(),
            "0.0.0.0".to_string(),
            "--port".to_string(),
            "8000".to_string(),
        ];
        if enforce_eager {
            a.push("--enforce-eager".to_string());
        }
        a.extend(svc.spec.extra_args.iter().cloned());
        (Some(vec!["vllm".to_string(), "serve".to_string()]), Some(a))
    } else {
        (None, None)
    };

    // vLLM uses /dev/shm for intra-process tensor transfer; the container
    // runtime's default 64MiB is too small and causes hard-to-diagnose
    // crashes under load. Back it with a memory-medium emptyDir on real
    // serving pods. The placeholder needs none.
    let (volumes, volume_mounts) = if serving {
        (
            Some(vec![Volume {
                name: "dshm".to_string(),
                empty_dir: Some(EmptyDirVolumeSource {
                    medium: Some("Memory".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            Some(vec![VolumeMount {
                name: "dshm".to_string(),
                mount_path: "/dev/shm".to_string(),
                ..Default::default()
            }]),
        )
    } else {
        (None, None)
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
    // vLLM's base image (CUDA 12.8+) sets LD_LIBRARY_PATH to /usr/local/cuda
    // paths, but GKE/EKS/AKS mount the GPU driver at /usr/local/nvidia/lib64;
    // without this the loader cannot find libcuda.so.1 and vLLM falls back to
    // "UnspecifiedPlatform" and fails to infer the device. Point the loader at
    // the managed-cluster driver mount. Only meaningful on real GPU pods.
    let env = if serving {
        Some(vec![EnvVar {
            name: "LD_LIBRARY_PATH".to_string(),
            value: Some("/usr/local/nvidia/lib64".to_string()),
            ..Default::default()
        }])
    } else {
        None
    };
    let container = Container {
        name: "inference".to_string(),
        image: Some(svc.spec.image.clone()),
        command,
        args,
        env,
        ports: Some(vec![ContainerPort {
            container_port: 8000,
            name: Some("http".to_string()),
            ..Default::default()
        }]),
        resources,
        readiness_probe,
        volume_mounts,
        ..Default::default()
    };

    // RuntimeClass is cluster-dependent and comes from the spec (default
    // None). Managed clusters (GKE/EKS/AKS) expose GPUs via the device
    // plugin with the default runtime and define no "nvidia" RuntimeClass;
    // setting a non-existent one makes the API server reject the pod. K3s
    // GPU nodes set it to "nvidia".
    // Pin to an exact node when the spec names one (ADR-0004). The fleet
    // controller sets node_name to land a placement on its chosen node via
    // the well-known kubernetes.io/hostname label, keeping the default
    // scheduler in the loop (resource fit, admission, taint/toleration)
    // rather than bypassing it with spec.nodeName. Unset => no constraint.
    let node_selector = svc.spec.node_name.as_ref().map(|n| {
        let mut m = BTreeMap::new();
        m.insert("kubernetes.io/hostname".to_string(), n.clone());
        m
    });

    let template = PodTemplateSpec {
        metadata: Some(ObjectMeta {
            labels: Some(labels.clone()),
            ..Default::default()
        }),
        spec: Some(PodSpec {
            containers: vec![container],
            runtime_class_name: svc.spec.runtime_class_name.clone(),
            node_selector,
            volumes,
            termination_grace_period_seconds: serving.then_some(TERMINATION_GRACE_SECONDS),
            ..Default::default()
        }),
    };

    // Make-before-break, declared rather than inherited: maxSurge=1 /
    // maxUnavailable=0 is what the k8s defaults (25%/25%) already round to at
    // replicas=1, but the anti-cascade drain of ADR-0005 dec.5 relies on it,
    // so it must not drift silently with defaults. On a cross-node move
    // (nodeSelector change, ADR-0004) the replacement pod must be Ready on
    // the target node before the displaced pod is terminated. Trade-off,
    // stated: an in-place update on a full single-GPU node holds (surge pod
    // unschedulable) until capacity frees — correct for fleet moves, which
    // always change node.
    let strategy = DeploymentStrategy {
        type_: Some("RollingUpdate".to_string()),
        rolling_update: Some(RollingUpdateDeployment {
            max_surge: Some(IntOrString::Int(1)),
            max_unavailable: Some(IntOrString::Int(0)),
        }),
    };

    let spec = DeploymentSpec {
        replicas: Some(svc.spec.replicas),
        selector: LabelSelector {
            match_labels: Some(labels.clone()),
            ..Default::default()
        },
        strategy: Some(strategy),
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
    let _measurer = ctx.metrics.count_and_measure();
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
    ctx.metrics
        .set_phase(&name, &ns, phase, &["Pending", "Warming", "Ready"]);

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

fn error_policy(svc: Arc<VllmService>, err: &Error, ctx: Arc<Context>) -> Action {
    warn!("reconcile failed: {}", err);
    ctx.metrics.set_failure(&svc.name_any(), err.metric_label());
    Action::requeue(Duration::from_secs(15))
}

mod http {
    use crate::Metrics;
    use axum::{
        extract::State, http::header::CONTENT_TYPE, response::IntoResponse, routing::get, Router,
    };
    use tokio::net::TcpListener;
    use tracing::info;

    /// OpenMetrics scrape endpoint.
    async fn metrics_handler(State(metrics): State<Metrics>) -> impl IntoResponse {
        let body = metrics.encode();
        (
            [(
                CONTENT_TYPE,
                "application/openmetrics-text; version=1.0.0; charset=utf-8",
            )],
            body,
        )
    }

    /// Liveness/readiness probe target.
    async fn health_handler() -> &'static str {
        "ok"
    }

    /// Serve `/metrics` and `/healthz` on 0.0.0.0:8080 until shutdown.
    pub async fn serve(metrics: Metrics) -> anyhow::Result<()> {
        let app = Router::new()
            .route("/metrics", get(metrics_handler))
            .route("/healthz", get(health_handler))
            .with_state(metrics);
        let listener = TcpListener::bind("0.0.0.0:8080").await?;
        info!("metrics server listening on 0.0.0.0:8080");
        axum::serve(listener, app).await?;
        Ok(())
    }
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
    let metrics = Metrics::default();
    let fleet_metrics = metrics.clone();
    let fleet_client = client.clone();
    let context = Arc::new(Context {
        client: client.clone(),
        metrics: metrics.clone(),
    });

    tokio::spawn(async move {
        if let Err(e) = http::serve(metrics).await {
            tracing::error!("metrics server exited: {}", e);
        }
    });
    info!("starting vllm-coldstart-operator controller");

    let vllm_fut = Controller::new(services, watcher::Config::default())
        .owns(deployments, watcher::Config::default())
        .run(reconcile, error_policy, context)
        .for_each(|res| async move {
            match res {
                Ok((obj, _action)) => info!("reconciled {}", obj.name),
                Err(e) => warn!("reconcile loop error: {:?}", e),
            }
        });

    // Both controllers share the runtime and the metrics registry. join!
    // keeps both alive for the process lifetime; neither stream terminates
    // under normal operation, so this never returns. (select! would tear the
    // other controller down the instant one stream ended — wrong here.)
    let fleet_fut = fleet_controller::run(fleet_client, fleet_metrics);
    tokio::join!(vllm_fut, fleet_fut);

    Ok(())
}
