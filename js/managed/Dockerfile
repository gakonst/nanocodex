FROM --platform=$BUILDPLATFORM golang:1.26.5-trixie AS hand
ARG TARGETARCH
WORKDIR /src/hand
COPY .generated/hand/go.mod .generated/hand/go.sum ./
RUN go mod download
COPY .generated/hand/*.go ./
RUN CGO_ENABLED=0 GOOS=linux GOARCH=$TARGETARCH go build -trimpath -o /out/nanocodex-remote .

# The SDK's Ubuntu 22.04 base has no labwc package. Bundle the VM's Debian
# desktop stack with its own loader, preserving the SDK and shell libraries.
FROM debian:trixie-slim AS desktop-libs
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl git xz-utils libwayland-dev libxkbcommon-dev labwc foot ffmpeg grim wlr-randr xwayland patchelf \
    && rm -rf /var/lib/apt/lists/*

# Compile natively even on Apple Silicon; Zig's generated translate-c executable
# currently traps under Rosetta. The linker uses the target image's libraries.
FROM --platform=$BUILDPLATFORM golang:1.26.5-trixie AS waymote
ARG BUILDARCH
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl git xz-utils pkg-config && rm -rf /var/lib/apt/lists/*
RUN case "$BUILDARCH" in \
      arm64) arch=aarch64; checksum=ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17 ;; \
      amd64) arch=x86_64; checksum=70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00 ;; \
      *) exit 1 ;; \
    esac && curl --fail --location "https://ziglang.org/download/0.16.0/zig-${arch}-linux-0.16.0.tar.xz" -o /tmp/zig.tar.xz \
    && echo "$checksum  /tmp/zig.tar.xz" | sha256sum --check \
    && mkdir /opt/zig && tar -xf /tmp/zig.tar.xz --strip-components=1 -C /opt/zig
COPY --from=desktop-libs /usr/include /sysroot/usr/include
COPY --from=desktop-libs /usr/lib/x86_64-linux-gnu /sysroot/usr/lib/x86_64-linux-gnu
WORKDIR /src/waymote
RUN git init && git remote add origin https://github.com/rockorager/waymote.git \
    && git fetch --depth=1 origin 69f585e6a1dfb84a3ac8135f1a0536aa9d75f6c5 && git checkout --detach FETCH_HEAD \
    && PKG_CONFIG_SYSROOT_DIR=/sysroot PKG_CONFIG_LIBDIR=/sysroot/usr/lib/x86_64-linux-gnu/pkgconfig \
       /opt/zig/zig build -Dtarget=x86_64-linux-gnu.2.41 -Dcpu=baseline -Doptimize=ReleaseFast

FROM desktop-libs AS desktop
COPY --from=waymote /src/waymote/zig-out/bin/waymote-streamd /src/waymote/zig-out/bin/waymote-streamd
COPY scripts/bundle-hand-desktop.sh /tmp/bundle-hand-desktop.sh
RUN sh /tmp/bundle-hand-desktop.sh

# Linux toolchains; Apple SDKs and xcodebuild remain on macOS CI.
FROM golang:1.26.5-bookworm AS go-toolchain
FROM swift:6.3.3-jammy AS swift-toolchain
FROM node:24.19.0-bookworm-slim AS node-toolchain
RUN npm install --global pnpm@11.25.0

FROM docker.io/cloudflare/sandbox:0.12.4
COPY --from=swift-toolchain /usr /opt/swift/usr
COPY --from=node-toolchain /usr/local /opt/node
COPY --from=go-toolchain /usr/local/go /usr/local/go

COPY --from=ghcr.io/astral-sh/uv:0.8.11 /uv /uvx /usr/local/bin/

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    CARGO_TARGET_DIR=/tmp/nanocodex-cargo-target \
    PATH=/opt/node/bin:/opt/cargo/bin:/opt/swift/usr/bin:/usr/local/go/bin:${PATH}

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        clang \
        cmake \
        curl \
        fd-find \
        gh \
        git \
        git-lfs \
        jq \
        less \
        fonts-dejavu-core \
        libssl-dev \
        libcurl4-openssl-dev \
        libedit2 \
        libgcc-11-dev \
        libpython3-dev \
        libsqlite3-0 \
        libstdc++-11-dev \
        libxml2-dev \
        libz3-dev \
        python3-lldb-13 \
        tzdata \
        zlib1g-dev \
        musl-tools \
        ninja-build \
        openssh-client \
        pkg-config \
        python3 \
        python3-pip \
        python3-venv \
        ripgrep \
        rsync \
        shellcheck \
        sqlite3 \
        tmux \
        tree \
        unzip \
        vim-tiny \
        wget \
        zip \
    && apt-get clean \
    && rm -rf /var/cache/apt/* /var/lib/apt/lists/*

# Ubuntu 22.04's chromium-browser package requires Snap, which is unavailable
# in the Sandbox container. Install Google's native AMD64 Debian package.
RUN curl --fail --location --proto '=https' --tlsv1.2 \
        https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb \
        -o /tmp/google-chrome.deb \
    && apt-get update \
    && apt-get install -y --no-install-recommends /tmp/google-chrome.deb \
    && google-chrome --version \
    && apt-get clean \
    && rm -rf /tmp/google-chrome.deb /var/cache/apt/* /var/lib/apt/lists/*

RUN ln -s /usr/bin/fdfind /usr/local/bin/fd \
    && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain 1.97.0 \
    && rustup component add clippy rustfmt \
    && rustup target add wasm32-unknown-unknown x86_64-unknown-linux-musl \
    && cargo install --locked wasm-bindgen-cli --version 0.2.126 \
    && rm -rf "$CARGO_TARGET_DIR" \
    && cargo --version \
    && rustc --version \
    && pnpm --version \
    && uv --version

# Creative, document, OCR, and scientific tools are available without a download.
COPY .generated/toolkit /opt/hand-toolkit
RUN apt-get update && apt-get install -y --no-install-recommends \
        blender ffmpeg imagemagick inkscape libreoffice pandoc \
        poppler-utils qpdf ghostscript tesseract-ocr tesseract-ocr-eng graphviz \
        fonts-noto-core fonts-noto-cjk fonts-noto-color-emoji \
    && rm -rf /var/lib/apt/lists/* \
    && uv venv --python /usr/bin/python3 --seed /opt/hand-python \
    && /opt/hand-python/bin/pip install --no-cache-dir \
        numpy==2.2.6 scipy==1.15.3 pandas==2.3.3 matplotlib==3.10.6 \
        sympy==1.14.0 Pillow==11.3.0 lxml==6.0.2 openpyxl==3.1.5 reportlab==4.4.4 \
        -r /opt/hand-toolkit/python.txt \
    && chmod 0755 /opt/hand-toolkit/check.py \
    && ln -s /opt/hand-toolkit/check.py /usr/local/bin/nanocodex-check-hand-toolkit
ENV PATH=/opt/hand-python/bin:${PATH}

COPY scripts/check-dev-stack.sh /usr/local/bin/nanocodex-check-dev-stack
RUN sh /usr/local/bin/nanocodex-check-dev-stack

COPY --from=hand /out/nanocodex-remote /usr/local/bin/nanocodex-remote
COPY --from=desktop /opt/nanocodex-desktop /opt/nanocodex-desktop
COPY --from=desktop /out/desktop-bin/ /usr/local/bin/
# wlroots uses this compiled-in path when starting Xwayland.
RUN ln -s /usr/local/bin/Xwayland /usr/bin/Xwayland
COPY --from=desktop /usr/share/X11/xkb /usr/share/X11/xkb
COPY --from=desktop /usr/share/terminfo /usr/share/terminfo
COPY --from=desktop /etc/xdg/foot /etc/xdg/foot
COPY .generated/hand/labwc /etc/nanocodex-desktop

EXPOSE 3000
EXPOSE 5173
EXPOSE 8000
EXPOSE 8080
