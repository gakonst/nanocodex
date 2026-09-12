#!/bin/sh
set -eu
apk add --no-cache \
    bash ca-certificates curl wget git git-lfs github-cli openssh-client \
    ripgrep fd jq yq less vim tmux tree rsync zip unzip tar xz zstd \
    build-base clang cmake ninja pkgconf openssl-dev linux-headers \
    go nodejs npm pnpm uv python3 python3-dev py3-pip sqlite \
    xvfb openbox xterm mesa-dri-gallium chromium libpulse libxkbcommon \
    blender ffmpeg imagemagick inkscape libreoffice pandoc-cli \
    poppler-utils qpdf ghostscript tesseract-ocr tesseract-ocr-data-eng graphviz \
    font-dejavu font-noto font-noto-cjk font-noto-emoji \
    py3-numpy py3-scipy py3-pandas py3-matplotlib py3-sympy py3-pillow \
    py3-lxml py3-openpyxl py3-reportlab
python3 -m venv --system-site-packages /opt/hand-python
/opt/hand-python/bin/pip install --no-cache-dir -r /opt/hand-toolkit/python.txt
mkdir -p /app /workspace /run/nanocodex-desktop
chmod 0700 /run/nanocodex-desktop
