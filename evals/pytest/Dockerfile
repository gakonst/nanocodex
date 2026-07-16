# syntax=docker/dockerfile:1.7

ARG BASE_IMAGE
FROM ghcr.io/astral-sh/uv:0.9.5 AS uv

FROM ${BASE_IMAGE}
COPY --from=uv /uv /uvx /usr/local/bin/
RUN apt-get update && \
    apt-get install --yes --no-install-recommends vim && \
    rm -rf /var/lib/apt/lists/*
RUN uv pip install --system --no-cache \
    pytest==8.4.1 \
    pytest-json-ctrf==0.3.5 \
    requests==2.32.5
