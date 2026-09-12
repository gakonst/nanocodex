FROM rust:1.97-alpine3.24 AS build
RUN apk add --no-cache build-base pkgconf pulseaudio-dev libxkbcommon-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY vendor ./vendor
RUN RUSTFLAGS="-C target-feature=-crt-static" cargo build --locked --release --bin nanocodex-computer

FROM alpine:3.24
RUN apk add --no-cache libgcc libpulse libxkbcommon
COPY --from=build /src/target/release/nanocodex-computer /usr/local/bin/nanocodex-computer
ENTRYPOINT ["nanocodex-computer"]
