set dotenv-load := true
set shell := ["bash", "-euo", "pipefail", "-c"]

harbor := ".venv/bin/harbor"
build_profile := env_var_or_default("HARNESS_BUILD_PROFILE", "dev")
agent_artifact_dir := ".harness/installed"
agent_artifact := agent_artifact_dir + "/harness"
default_eval := "evals/terminal-bench-2.yaml"
default_jobs := ".harness/harbor/jobs"

default: run

# Install development dependencies once. Dataset downloads remain Harbor's job.
bootstrap:
    uv sync --frozen
    cargo fetch --locked

# Tight inner loop: native PTC-only model process, no Harbor or Docker.
run:
    @cargo run --quiet -- run --mode=model --model=gpt-5.6-sol --effort=low < examples/task-start.jsonl

# Build a static Linux executable for the Docker daemon's native architecture.
# This is a native container build, not an amd64 cross-compile on Apple Silicon.
build-agent:
    @mkdir -p "{{agent_artifact_dir}}"
    @echo "Building native Linux agent artifact (Cargo profile: {{build_profile}})..."
    @docker build --quiet --build-arg CARGO_PROFILE="{{build_profile}}" --file harbor_adapter/harness.Dockerfile --target artifact --output type=local,dest="{{agent_artifact_dir}}" .
    @test -x "{{agent_artifact}}"

# Run a Harbor-native job config. Rust executes inside each benchmark container.
eval config=default_eval: build-agent
    @test -x "{{harbor}}" || { echo "run 'just bootstrap' first" >&2; exit 2; }
    @HARBOR_TELEMETRY=off "{{harbor}}" run --config "{{config}}"

# Open all locally retained Harbor jobs unless another jobs directory is supplied.
view jobs=default_jobs:
    @test -x "{{harbor}}" || { echo "run 'just bootstrap' first" >&2; exit 2; }
    @test -d "{{jobs}}" || { echo "no Harbor jobs at {{jobs}}; run 'just eval' first" >&2; exit 2; }
    @HARBOR_TELEMETRY=off "{{harbor}}" view --jobs "{{jobs}}"

# Checks stay small until the end-to-end agent path is real.
check:
    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    .venv/bin/python -m compileall -q harbor_adapter
    "{{harbor}}" run --config "{{default_eval}}" --print-config >/dev/null
