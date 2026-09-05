FROM python:3.12-slim

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends \
        file \
        libreoffice-calc \
        libreoffice-draw \
        libreoffice-impress \
        libreoffice-writer \
        poppler-utils \
    && rm -rf /var/lib/apt/lists/*
