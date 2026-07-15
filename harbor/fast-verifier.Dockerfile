FROM ghcr.io/astral-sh/uv:0.9.5 AS uv

FROM python:3.13-slim-bookworm

WORKDIR /app

# Reproduce the pinned canary's Dockerfile on the local daemon architecture.
RUN apt-get update && apt-get install -y git

COPY setup.sh ./
COPY resources /app/resources

RUN bash /app/setup.sh

WORKDIR /app/personal-site

COPY --from=uv /uv /uvx /bin/

# These are the exact dependencies installed by the canonical verifier on
# every trial. Bake them once; the benchmark assertions remain unchanged.
RUN uv pip install --system \
    pytest==8.4.1 \
    pytest-json-ctrf==0.3.5
