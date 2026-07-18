# syntax=docker/dockerfile:1.7
# ---- Builder ---------------------------------------------------------------
# Pinned to the dev toolchain (1.95) for reproducible builds.
# Static musl target -> single self-contained binary, no shared libs at runtime.
FROM rust:1.95-bookworm AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-unknown-linux-musl
WORKDIR /build
# Manifests + sources together: the manifest uses target auto-discovery,
# so `cargo fetch` needs the targets present. Heavy compile cost stays
# behind the BuildKit cache mounts below.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo fetch --locked
# CC points at musl-gcc so ring's C code compiles for the musl target.
# The LINKER is intentionally NOT overridden: rustc's default linker emits a
# correct static-pie binary, whereas forcing musl-gcc as linker breaks
# static-pie and produces a bogus INTERP (rust-lang/rust#95926).
ENV CC_x86_64_unknown_linux_musl=musl-gcc
RUN --mount=type=cache,target=/build/target,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo build --release --locked \
        --target x86_64-unknown-linux-musl \
        --bin vllm-coldstart-operator \
        --bin reporter \
    && cp target/x86_64-unknown-linux-musl/release/vllm-coldstart-operator /vllm-coldstart-operator \
    && cp target/x86_64-unknown-linux-musl/release/reporter /reporter \
    && strip /vllm-coldstart-operator /reporter
# ---- Runtime ---------------------------------------------------------------
# distroless static: no shell, no libc, nonroot (uid 65532) by default.
FROM gcr.io/distroless/static:nonroot AS runtime
LABEL org.opencontainers.image.source="https://github.com/MicheleCampi/vllm-coldstart-operator"
LABEL org.opencontainers.image.description="Kubernetes operator for vLLM cold-start lifecycle management"
LABEL org.opencontainers.image.licenses="Apache-2.0"
COPY --from=builder /vllm-coldstart-operator /usr/local/bin/vllm-coldstart-operator
# Reporter DaemonSet reuses this image with command: ["/usr/local/bin/reporter"].
COPY --from=builder /reporter /usr/local/bin/reporter
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/vllm-coldstart-operator"]
