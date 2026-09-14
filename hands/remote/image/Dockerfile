# Build from the repository root. Both architectures use the same pinned Waymote source.
FROM golang:1.26.5-trixie AS waymote
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl git xz-utils libwayland-dev libxkbcommon-dev && rm -rf /var/lib/apt/lists/*
RUN case "$TARGETARCH" in \
      arm64) arch=aarch64; checksum=ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17 ;; \
      amd64) arch=x86_64; checksum=70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00 ;; \
      *) exit 1 ;; \
    esac && curl --fail --location "https://ziglang.org/download/0.16.0/zig-${arch}-linux-0.16.0.tar.xz" -o /tmp/zig.tar.xz \
    && echo "$checksum  /tmp/zig.tar.xz" | sha256sum --check \
    && mkdir /opt/zig && tar -xf /tmp/zig.tar.xz --strip-components=1 -C /opt/zig && rm /tmp/zig.tar.xz
WORKDIR /src/waymote
RUN git init && git remote add origin https://github.com/rockorager/waymote.git \
    && git fetch --depth=1 origin 69f585e6a1dfb84a3ac8135f1a0536aa9d75f6c5 \
    && git checkout --detach FETCH_HEAD
# Distributable images must not inherit the CI runner's CPU extensions (for
# example ARM LSE/SVE unavailable on some supported deployment machines).
RUN /opt/zig/zig build -Dcpu=baseline -Doptimize=ReleaseFast

FROM golang:1.26.5-trixie AS remote
WORKDIR /src/remote
COPY hands/remote/go.mod hands/remote/go.sum ./
RUN go mod download
COPY hands/remote/*.go ./
RUN CGO_ENABLED=0 go build -trimpath -o /out/nanocodex-remote .

FROM rust:1.97-slim-trixie AS rust-toolchain
RUN rustup component add clippy rustfmt && rustup target add wasm32-unknown-unknown
FROM node:24.19.0-trixie-slim AS node-toolchain
RUN npm install --global pnpm@11.25.0
FROM debian:trixie-slim
COPY --from=rust-toolchain /usr/local/rustup /opt/rustup
COPY --from=rust-toolchain /usr/local/cargo /opt/cargo
COPY --from=node-toolchain /usr/local /opt/node
COPY --from=remote /usr/local/go /usr/local/go
COPY --from=ghcr.io/astral-sh/uv:0.8.11 /uv /uvx /usr/local/bin/
ENV RUSTUP_HOME=/opt/rustup PATH=/opt/hand-python/bin:/opt/node/bin:/opt/cargo/bin:/usr/local/go/bin:$PATH
COPY crates/experimental/nanocodex-vm/image/toolkit /opt/hand-toolkit
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates labwc foot chromium ffmpeg grim libwayland-client0 libxkbcommon0 \
      dbus-x11 wlr-randr xwayland fonts-dejavu-core curl procps \
      build-essential clang cmake ninja-build pkg-config libssl-dev git git-lfs gh \
      ripgrep fd-find jq openssh-client rsync zip unzip xz-utils zstd tmux tree vim-tiny \
      blender imagemagick inkscape libreoffice pandoc poppler-utils qpdf ghostscript \
      tesseract-ocr tesseract-ocr-eng graphviz fonts-noto-core fonts-noto-cjk fonts-noto-color-emoji \
      python3 python3-dev python3-venv python3-pip python3-numpy python3-scipy python3-pandas \
      python3-matplotlib python3-sympy python3-pil python3-lxml python3-openpyxl python3-reportlab \
    && rm -rf /var/lib/apt/lists/* \
    && python3 -m venv --system-site-packages /opt/hand-python \
    && /opt/hand-python/bin/pip install --no-cache-dir -r /opt/hand-toolkit/python.txt \
    && ln -s /usr/bin/fdfind /usr/local/bin/fd \
    && sh /opt/hand-toolkit/install-paths.sh \
    && chmod 0755 /opt/hand-toolkit/check.py \
    && ln -s /opt/hand-toolkit/check.py /usr/local/bin/nanocodex-check-hand-toolkit \
    && useradd --create-home --uid 1000 --shell /bin/bash desktop \
    && install -d -o desktop -g desktop -m 0700 /run/nanocodex-desktop \
    && install -d -o desktop -g desktop /workspace
COPY --from=waymote /src/waymote/zig-out/bin/waymote-streamd /usr/local/bin/waymote-streamd
COPY --from=remote /out/nanocodex-remote /usr/local/bin/nanocodex-remote
COPY hands/remote/image/labwc /etc/nanocodex-desktop
ENV WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_HEADLESS_OUTPUTS=1 \
    XDG_RUNTIME_DIR=/run/nanocodex-desktop WAYLAND_DISPLAY=wayland-0 \
    XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=labwc
USER desktop
WORKDIR /workspace
ENTRYPOINT ["labwc", "--config-dir", "/etc/nanocodex-desktop"]
