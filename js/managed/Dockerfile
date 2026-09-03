FROM docker.io/cloudflare/sandbox:0.12.4

COPY --from=ghcr.io/astral-sh/uv:0.8.11 /uv /uvx /usr/local/bin/

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    CARGO_TARGET_DIR=/tmp/nanocodex-cargo-target \
    PATH=/opt/cargo/bin:${PATH}

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
        libssl-dev \
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

RUN ln -s /usr/bin/fdfind /usr/local/bin/fd \
    && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain stable \
    && rustup component add clippy rustfmt \
    && npm install --global pnpm@11.25.0 \
    && cargo --version \
    && rustc --version \
    && pnpm --version \
    && uv --version

EXPOSE 3000
EXPOSE 5173
EXPOSE 8000
EXPOSE 8080
