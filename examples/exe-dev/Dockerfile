FROM docker.io/library/rust:1.97-bookworm AS builder

WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release -p nanocodex-exe-dev --bin nanocodex-exe-dev && \
    cp /src/target/release/nanocodex-exe-dev /tmp/nanocodex-exe-dev

FROM scratch AS artifact

COPY --from=builder /tmp/nanocodex-exe-dev /nanocodex-exe-dev

FROM ghcr.io/boldsoftware/exeuntu:latest

COPY --from=builder /tmp/nanocodex-exe-dev /usr/local/bin/nanocodex-exe-dev
COPY examples/exe-dev/nanocodex-exe-dev.service /etc/systemd/system/nanocodex-exe-dev.service

RUN install -d -o exedev -g exedev /home/exedev/workspace /home/exedev/.local/state/nanocodex && \
    systemctl enable nanocodex-exe-dev.service

EXPOSE 8000 9998
