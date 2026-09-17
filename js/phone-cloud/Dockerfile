# syntax=docker/dockerfile:1
FROM rust:1.97.0-bookworm AS voice
RUN apt-get update && apt-get install -y --no-install-recommends cmake pkg-config libssl-dev clang && rm -rf /var/lib/apt/lists/*
WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY bin ./bin
COPY crates ./crates
COPY examples ./examples
COPY js/nanocodex ./js/nanocodex
COPY py/bindings ./py/bindings
COPY third_party ./third_party
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/usr/local/cargo/git --mount=type=cache,target=/source/target CARGO_BUILD_JOBS=2 cargo build --release --locked -p nanocodex-phone && cp target/release/phone-voice-cloud /phone-voice-cloud

FROM node:24.19.0-bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
RUN npm install --omit=dev --ignore-scripts --no-audit --no-fund ws@8.18.3
COPY --from=voice /phone-voice-cloud /usr/local/bin/phone-voice
COPY js/managed/scripts/phone-bridge.mjs ./scripts/phone-bridge.mjs
COPY js/managed/scripts/phone-delegation.mjs ./scripts/phone-delegation.mjs
COPY js/managed/src/twilio-voice.ts ./src/twilio-voice.ts
ENV NANOCODEX_PHONE_HOST=0.0.0.0 NANOCODEX_PHONE_PORT=8788 NANOCODEX_PHONE_VOICE_BINARY=/usr/local/bin/phone-voice NANOCODEX_PHONE_DATABASE=/tmp/phone.sqlite
USER node
EXPOSE 8788
CMD ["node", "scripts/phone-bridge.mjs"]
