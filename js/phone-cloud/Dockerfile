# syntax=docker/dockerfile:1
FROM rust:1.97.0-bookworm AS voice-chef
# Explicit upstream refresh without disabling normal source-layer reuse.
ARG NANOCODEX_IMAGE_CACHE_EPOCH=1
RUN test -n "$NANOCODEX_IMAGE_CACHE_EPOCH"
RUN apt-get update && apt-get install -y --no-install-recommends cmake pkg-config libssl-dev clang && rm -rf /var/lib/apt/lists/*
# Install once in the toolchain layer, independently of application sources.
RUN cargo install cargo-chef --version 0.1.78 --locked --jobs 2
WORKDIR /source

FROM voice-chef AS voice-planner
COPY Cargo.toml Cargo.lock ./
COPY bin ./bin
COPY crates ./crates
COPY examples ./examples
COPY js/nanocodex ./js/nanocodex
COPY py/bindings ./py/bindings
COPY third_party ./third_party
RUN cargo chef prepare --recipe-path recipe.json

FROM voice-chef AS voice
COPY --from=voice-planner /source/recipe.json recipe.json
# Cook only the recipe in this otherwise empty directory. Keep target AND Cargo
# sources in image layers so a fresh builder can reuse the exported dependencies.
RUN CARGO_BUILD_JOBS=2 cargo chef cook --release --locked -p nanocodex-phone --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY bin ./bin
COPY crates ./crates
COPY examples ./examples
COPY js/nanocodex ./js/nanocodex
COPY py/bindings ./py/bindings
COPY third_party ./third_party
RUN CARGO_BUILD_JOBS=2 cargo build --release --locked -p nanocodex-phone && cp target/release/phone-voice-cloud /phone-voice-cloud

FROM node:24.19.0-bookworm-slim
# Explicit upstream refresh without disabling normal source-layer reuse.
ARG NANOCODEX_IMAGE_CACHE_EPOCH=1
RUN test -n "$NANOCODEX_IMAGE_CACHE_EPOCH"
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
RUN npm install --omit=dev --ignore-scripts --no-audit --no-fund ws@8.18.3
COPY --from=voice /phone-voice-cloud /usr/local/bin/phone-voice
COPY js/managed/scripts/phone-bridge.mjs ./scripts/phone-bridge.mjs
COPY js/managed/scripts/phone-delegation.mjs ./scripts/phone-delegation.mjs
COPY js/managed/scripts/phone-media-diagnostics.mjs ./scripts/phone-media-diagnostics.mjs
COPY js/managed/src/twilio-voice.ts ./src/twilio-voice.ts
ENV NANOCODEX_PHONE_HOST=0.0.0.0 NANOCODEX_PHONE_PORT=8788 NANOCODEX_PHONE_VOICE_BINARY=/usr/local/bin/phone-voice NANOCODEX_PHONE_DATABASE=/tmp/phone.sqlite
USER node
EXPOSE 8788
CMD ["node", "scripts/phone-bridge.mjs"]
