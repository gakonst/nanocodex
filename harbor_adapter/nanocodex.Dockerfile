# syntax=docker/dockerfile:1.7

FROM rust:1.85-alpine3.21 AS build

ARG TARGETARCH
ARG CARGO_PROFILE=dev
WORKDIR /build
RUN apk add --no-cache musl-dev

COPY Cargo.toml Cargo.lock ./
COPY bin/nanocodex/Cargo.toml bin/nanocodex/Cargo.toml
COPY crates/nanocodex/Cargo.toml crates/nanocodex/Cargo.toml
COPY crates/nanocodex-core/Cargo.toml crates/nanocodex-core/Cargo.toml
COPY crates/nanocodex-service/Cargo.toml crates/nanocodex-service/Cargo.toml
COPY crates/nanocodex-tools/Cargo.toml crates/nanocodex-tools/Cargo.toml
# Keep dependency compilation in a manifest-only layer. Source-only edits reuse
# this layer, while the cache mounts retain Cargo downloads and target outputs.
RUN mkdir bin/nanocodex/src \
        crates/nanocodex/src \
        crates/nanocodex-core/src \
        crates/nanocodex-service/src \
        crates/nanocodex-service/benches \
        crates/nanocodex-tools/src && \
    printf 'fn main() {}\n' > bin/nanocodex/src/main.rs && \
    printf '\n' > crates/nanocodex/src/lib.rs && \
    printf '\n' > crates/nanocodex-core/src/lib.rs && \
    printf '\n' > crates/nanocodex-service/src/lib.rs && \
    printf 'fn main() {}\n' > crates/nanocodex-service/benches/tower_responses.rs && \
    printf '\n' > crates/nanocodex-tools/src/lib.rs
RUN --mount=type=cache,id=nanocodex-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=nanocodex-target-${TARGETARCH},target=/build/target \
    cargo build --locked --profile "${CARGO_PROFILE}"

COPY bin ./bin
COPY crates ./crates
RUN --mount=type=cache,id=nanocodex-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=nanocodex-target-${TARGETARCH},target=/build/target \
    touch bin/nanocodex/src/main.rs \
        crates/nanocodex/src/lib.rs \
        crates/nanocodex-core/src/lib.rs \
        crates/nanocodex-service/src/lib.rs \
        crates/nanocodex-tools/src/lib.rs && \
    cargo build --locked --profile "${CARGO_PROFILE}" && \
    mkdir /out && \
    case "${CARGO_PROFILE}" in \
        dev) artifact_dir=debug ;; \
        *) artifact_dir="${CARGO_PROFILE}" ;; \
    esac && \
    cp "target/${artifact_dir}/nanocodex" /out/nanocodex

FROM scratch AS artifact
COPY --from=build /out/nanocodex /nanocodex
