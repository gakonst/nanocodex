# syntax=docker/dockerfile:1.7

FROM rust:1.88-alpine3.21 AS build

ARG TARGETARCH
ARG CARGO_PROFILE=dev
ARG TAG_NAME=dev
ARG VERGEN_GIT_SHA=unknown
ENV TAG_NAME=${TAG_NAME} \
    VERGEN_GIT_SHA=${VERGEN_GIT_SHA}
WORKDIR /build
RUN apk add --no-cache musl-dev

COPY Cargo.toml Cargo.lock ./
COPY bin/nanocodex/Cargo.toml bin/nanocodex/Cargo.toml
COPY bin/nanocodex/build.rs bin/nanocodex/build.rs
COPY js/bindings/Cargo.toml js/bindings/Cargo.toml
COPY py/bindings/Cargo.toml py/bindings/Cargo.toml
COPY crates/nanocodex/Cargo.toml crates/nanocodex/Cargo.toml
COPY crates/nanocodex-core/Cargo.toml crates/nanocodex-core/Cargo.toml
COPY crates/nanocodex-macros/Cargo.toml crates/nanocodex-macros/Cargo.toml
COPY crates/nanocodex-mcp/Cargo.toml crates/nanocodex-mcp/Cargo.toml
COPY crates/nanocodex-observability/Cargo.toml crates/nanocodex-observability/Cargo.toml
COPY crates/nanocodex-service/Cargo.toml crates/nanocodex-service/Cargo.toml
COPY crates/nanocodex-tools/Cargo.toml crates/nanocodex-tools/Cargo.toml
COPY examples/Cargo.toml examples/Cargo.toml
# Keep dependency compilation in a manifest-only layer. Source-only edits reuse
# this layer, while the cache mounts retain Cargo downloads and target outputs.
RUN mkdir bin/nanocodex/src \
        bin/nanocodex/benches \
        js/bindings/src \
        py/bindings/src \
        crates/nanocodex/src \
        crates/nanocodex-core/src \
        crates/nanocodex-core/benches \
        crates/nanocodex-macros/src \
        crates/nanocodex-mcp/src \
        crates/nanocodex-observability/src \
        crates/nanocodex-service/src \
        crates/nanocodex-service/benches \
        crates/nanocodex-tools/src && \
    printf 'fn main() {}\n' > bin/nanocodex/src/main.rs && \
    printf 'fn main() {}\n' > bin/nanocodex/benches/tui_render.rs && \
    printf '\n' > js/bindings/src/lib.rs && \
    printf '\n' > py/bindings/src/lib.rs && \
    printf '\n' > crates/nanocodex/src/lib.rs && \
    printf '\n' > crates/nanocodex-core/src/lib.rs && \
    printf 'fn main() {}\n' > crates/nanocodex-core/benches/fork_history.rs && \
    printf '\n' > crates/nanocodex-macros/src/lib.rs && \
    printf '\n' > crates/nanocodex-mcp/src/lib.rs && \
    printf '\n' > crates/nanocodex-observability/src/lib.rs && \
    printf '\n' > crates/nanocodex-service/src/lib.rs && \
    printf 'fn main() {}\n' > crates/nanocodex-service/benches/tower_responses.rs && \
    printf '\n' > crates/nanocodex-tools/src/lib.rs && \
    printf 'fn main() {}\n' > examples/minimal.rs && \
    printf 'fn main() {}\n' > examples/follow_on.rs && \
    printf 'fn main() {}\n' > examples/custom_tool.rs && \
    printf 'fn main() {}\n' > examples/subagents.rs && \
    printf 'fn main() {}\n' > examples/mcp.rs && \
    printf 'fn main() {}\n' > examples/fork_conversations.rs && \
    printf 'fn main() {}\n' > examples/fork_checkpoint_bench.rs
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
        crates/nanocodex-macros/src/lib.rs \
        crates/nanocodex-mcp/src/lib.rs \
        crates/nanocodex-observability/src/lib.rs \
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

FROM alpine:3.21 AS runtime
RUN apk add --no-cache ca-certificates git
COPY --from=build /out/nanocodex /usr/local/bin/nanocodex
ENTRYPOINT ["/usr/local/bin/nanocodex"]
