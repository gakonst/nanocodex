# syntax=docker/dockerfile:1.7

FROM rust:1.85-alpine3.21 AS build

ARG TARGETARCH
ARG CARGO_PROFILE=dev
WORKDIR /build
RUN apk add --no-cache musl-dev

COPY Cargo.toml Cargo.lock ./
COPY bin/harness/Cargo.toml bin/harness/Cargo.toml
COPY crates/harness-agent/Cargo.toml crates/harness-agent/Cargo.toml
COPY crates/harness-core/Cargo.toml crates/harness-core/Cargo.toml
COPY crates/harness-service/Cargo.toml crates/harness-service/Cargo.toml
COPY crates/harness-tools/Cargo.toml crates/harness-tools/Cargo.toml
# Keep dependency compilation in a manifest-only layer. Source-only edits reuse
# this layer, while the cache mounts retain Cargo downloads and target outputs.
RUN mkdir bin/harness/src \
        crates/harness-agent/src \
        crates/harness-core/src \
        crates/harness-service/src \
        crates/harness-service/benches \
        crates/harness-tools/src && \
    printf 'fn main() {}\n' > bin/harness/src/main.rs && \
    printf '\n' > crates/harness-agent/src/lib.rs && \
    printf '\n' > crates/harness-core/src/lib.rs && \
    printf '\n' > crates/harness-service/src/lib.rs && \
    printf 'fn main() {}\n' > crates/harness-service/benches/tower_responses.rs && \
    printf '\n' > crates/harness-tools/src/lib.rs
RUN --mount=type=cache,id=harness-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=harness-target-${TARGETARCH},target=/build/target \
    cargo build --locked --profile "${CARGO_PROFILE}"

COPY bin ./bin
COPY crates ./crates
RUN --mount=type=cache,id=harness-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=harness-target-${TARGETARCH},target=/build/target \
    touch bin/harness/src/main.rs \
        crates/harness-agent/src/lib.rs \
        crates/harness-core/src/lib.rs \
        crates/harness-service/src/lib.rs \
        crates/harness-tools/src/lib.rs && \
    cargo build --locked --profile "${CARGO_PROFILE}" && \
    mkdir /out && \
    case "${CARGO_PROFILE}" in \
        dev) artifact_dir=debug ;; \
        *) artifact_dir="${CARGO_PROFILE}" ;; \
    esac && \
    cp "target/${artifact_dir}/harness" /out/harness

FROM scratch AS artifact
COPY --from=build /out/harness /harness
