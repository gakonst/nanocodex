# Build context: crates/experimental/nanocodex-vm/image.
FROM rust:1.97-alpine3.24 AS rust-toolchain
RUN rustup component add clippy rustfmt && rustup target add wasm32-unknown-unknown
FROM rust-toolchain AS computer-build
RUN apk add --no-cache build-base pkgconf pulseaudio-dev libxkbcommon-dev
WORKDIR /src/computer
COPY --from=computer-source /Cargo.toml /Cargo.lock ./
COPY --from=computer-source /src ./src
COPY --from=computer-source /vendor ./vendor
RUN RUSTFLAGS="-C target-feature=-crt-static" cargo build --locked --release --bin nanocodex-computer
FROM alpine:3.24@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b
COPY --from=computer-build /src/computer/target/release/nanocodex-computer /usr/local/bin/nanocodex-computer
COPY --from=rust-toolchain /usr/local/rustup /opt/rustup
COPY --from=rust-toolchain /usr/local/cargo /opt/cargo
COPY toolkit/install-alpine.sh toolkit/python.txt /opt/hand-toolkit/
RUN sh /opt/hand-toolkit/install-alpine.sh
COPY toolkit/check.py toolkit/install-paths.sh /opt/hand-toolkit/
RUN sh /opt/hand-toolkit/install-paths.sh \
    && ln -s /opt/hand-toolkit/check.py /usr/local/bin/nanocodex-check-hand-toolkit \
    && chmod 0755 /opt/hand-toolkit/check.py
ENV RUSTUP_HOME=/opt/rustup \
    PATH=/opt/hand-python/bin:/opt/cargo/bin:$PATH \
    DISPLAY=:0
WORKDIR /app
CMD ["/bin/sh"]
