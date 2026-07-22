# syntax=docker/dockerfile:1.7
# ---- Build args ------------------------------------------------------------
# Default: static musl binary on distroless/static (CPU-only path, unchanged).
# GPU variant (level-3 sessions): nvml-wrapper dlopens libnvidia-ml.so at
# runtime, which a static musl binary cannot do. Build with:
#   --build-arg RUST_TARGET=x86_64-unknown-linux-gnu \
#   --build-arg CARGO_FEATURES=gpu-nvidia \
#   --build-arg RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot
# (distroless/cc = glibc + dynamic loader; NVIDIA driver libs are injected
# on the node by the NVIDIA container runtime, never baked into the image.)
ARG RUNTIME_IMAGE=gcr.io/distroless/static:nonroot
# ---- Builder ---------------------------------------------------------------
# Pinned to the dev toolchain (1.95) for reproducible builds.
FROM rust:1.95-bookworm AS builder
ARG RUST_TARGET=x86_64-unknown-linux-musl
ARG CARGO_FEATURES=""
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add "${RUST_TARGET}"
WORKDIR /build
# Manifests + sources together: the manifest uses target auto-discovery,
# so `cargo fetch` needs the targets present. Heavy compile cost stays
# behind the BuildKit cache mounts below.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo fetch --locked
# CC points at musl-gcc so ring's C code compiles for the musl target
# (harmless for the gnu target, which never reads this variable).
# The LINKER is intentionally NOT overridden: rustc's default linker emits a
# correct static-pie binary, whereas forcing musl-gcc as linker breaks
# static-pie and produces a bogus INTERP (rust-lang/rust#95926).
ENV CC_x86_64_unknown_linux_musl=musl-gcc
RUN --mount=type=cache,target=/build/target,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo build --release --locked \
        --target "${RUST_TARGET}" \
        ${CARGO_FEATURES:+--features "${CARGO_FEATURES}"} \
        --bin vllm-coldstart-operator \
        --bin reporter \
    && cp "target/${RUST_TARGET}/release/vllm-coldstart-operator" /vllm-coldstart-operator \
    && cp "target/${RUST_TARGET}/release/reporter" /reporter \
    && strip /vllm-coldstart-operator /reporter
# ---- Runtime ---------------------------------------------------------------
# Default distroless static: no shell, no libc, nonroot (uid 65532).
FROM ${RUNTIME_IMAGE} AS runtime
LABEL org.opencontainers.image.source="https://github.com/MicheleCampi/vllm-coldstart-operator"
LABEL org.opencontainers.image.description="Kubernetes operator for vLLM cold-start lifecycle management"
LABEL org.opencontainers.image.licenses="Apache-2.0"
COPY --from=builder /vllm-coldstart-operator /usr/local/bin/vllm-coldstart-operator
# Reporter DaemonSet reuses this image with command: ["/usr/local/bin/reporter"].
COPY --from=builder /reporter /usr/local/bin/reporter
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/vllm-coldstart-operator"]
