# Build context: crates/experimental/nanocodex-vm/image.
FROM rust:1.97-alpine3.24 AS rust-toolchain
RUN rustup component add clippy rustfmt && rustup target add wasm32-unknown-unknown
FROM alpine:3.24@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b
COPY --from=rust-toolchain /usr/local/rustup /opt/rustup
COPY --from=rust-toolchain /usr/local/cargo /opt/cargo
COPY toolkit /opt/hand-toolkit
RUN sh /opt/hand-toolkit/install-alpine.sh \
    && ln -s /opt/hand-toolkit/check.py /usr/local/bin/nanocodex-check-hand-toolkit \
    && chmod 0755 /opt/hand-toolkit/check.py
ENV RUSTUP_HOME=/opt/rustup \
    PATH=/opt/hand-python/bin:/opt/cargo/bin:$PATH \
    DISPLAY=:0
WORKDIR /app
CMD ["/bin/sh"]
