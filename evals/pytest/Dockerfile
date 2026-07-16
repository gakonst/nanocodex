# syntax=docker/dockerfile:1.7

ARG BASE_IMAGE
FROM ghcr.io/astral-sh/uv:0.9.5 AS uv

FROM ${BASE_IMAGE}
COPY --from=uv /uv /uvx /usr/local/bin/
# Canonical tests may repeat this package check inside the trial. Keep the
# indexes and dependencies in the cached verifier layer so that check is local.
RUN DEBIAN_FRONTEND=noninteractive apt-get update && \
    DEBIAN_FRONTEND=noninteractive \
    apt-get install --yes --no-install-recommends \
        curl \
        expect \
        git \
        openssh-client \
        python3 \
        python-is-python3 \
        vim
RUN uv pip install --system --break-system-packages --no-cache \
    pytest==8.4.1 \
    pytest-json-ctrf==0.3.5 \
    requests==2.32.5
