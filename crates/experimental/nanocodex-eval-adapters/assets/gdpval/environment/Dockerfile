FROM debian:bookworm-slim

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends \
        ca-certificates \
        curl \
        ffmpeg \
        file \
        fonts-dejavu-core \
        jq \
        libreoffice-calc \
        libreoffice-draw \
        libreoffice-impress \
        libreoffice-writer \
        poppler-utils \
        python3 \
        python3-pip \
        unzip \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace
