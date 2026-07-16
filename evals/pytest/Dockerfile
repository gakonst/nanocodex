# syntax=docker/dockerfile:1.7

ARG BASE_IMAGE
FROM ghcr.io/astral-sh/uv:0.9.5 AS uv

FROM ${BASE_IMAGE}
COPY --from=uv /uv /uvx /usr/local/bin/
# Canonical tests may repeat this package check inside the trial. Keep the
# indexes and dependencies in the cached verifier layer so that check is local.
RUN find /etc/apt -type f \( -name '*.list' -o -name '*.sources' \) \
        -exec sed -i 's|http://ports.ubuntu.com|https://ports.ubuntu.com|g' {} + && \
    if [ ! -s /etc/ssl/certs/ca-certificates.crt ]; then \
        DEBIAN_FRONTEND=noninteractive \
            apt-get -o Acquire::https::Verify-Peer=false update; \
        DEBIAN_FRONTEND=noninteractive \
            apt-get -o Acquire::https::Verify-Peer=false install \
                --yes --no-install-recommends ca-certificates; \
    fi && \
    DEBIAN_FRONTEND=noninteractive apt-get update && \
    DEBIAN_FRONTEND=noninteractive \
    apt-get install --yes --no-install-recommends \
        curl \
        expect \
        git \
        openssh-client \
        python3 \
        python-is-python3 \
        vim && \
    if apt-cache policy python3-distutils | \
            grep -Eq 'Candidate: [^ (]'; then \
        DEBIAN_FRONTEND=noninteractive \
        apt-get install --yes --no-install-recommends python3-distutils; \
    fi
RUN uv pip install --system --break-system-packages --no-cache \
    pytest==8.4.1 \
    pytest-json-ctrf==0.3.5 \
    requests==2.32.5
# Keep new task-specific system tools in an append-only layer so existing apt
# and Python dependency layers remain reusable across verifier overlays.
RUN DEBIAN_FRONTEND=noninteractive \
    apt-get install --yes --no-install-recommends primer3
RUN uv pip install --system --break-system-packages --no-cache \
    rdflib==7.1.4
# NumPy 2.3 requires Python 3.11. Older task images do not need it unless their
# canonical verifier asks for it, and those verifier tasks use newer Python.
RUN if python -c 'import sys; sys.exit(sys.version_info < (3, 11))'; then \
        uv pip install --system --break-system-packages --no-cache \
            numpy==2.3.0; \
    fi
