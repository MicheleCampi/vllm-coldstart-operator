# syntax=mirror.gcr.io/docker/dockerfile:1.7@sha256:a57df69d0ea827fb7266491f2813635de6f17269be881f696fbfdf2d83dda33e
# Build images come from mirror.gcr.io, Google's cache of Docker Hub, so a
# Docker Hub outage cannot stop the build. The frontend above and the Rust
# builder below are pinned by digest: a stale mirror then fails the build
# instead of changing the image. Update a tag and its digest together; the
# CI step "Build images avoid Docker Hub" fails if either loses its pin.
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
# Rust 1.95, the toolchain rust-toolchain.toml pins (CI checks they agree).
FROM mirror.gcr.io/library/rust:1.95-bookworm@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1 AS builder
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
