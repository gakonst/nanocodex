set dotenv-load := true
set shell := ["bash", "-euo", "pipefail", "-c"]
export PYTHONPATH := justfile_directory()

harbor := ".venv/bin/harbor"
build_profile := env_var_or_default("HARNESS_BUILD_PROFILE", "dev")
agent_artifact_dir := ".harness/installed"
agent_artifact := agent_artifact_dir + "/harness"
default_eval := "evals/terminal-bench-2.yaml"
default_jobs := ".harness/harbor/jobs"
setup_jobs := ".harness/harbor/setup"
prepare_concurrency := env_var_or_default("HARBOR_PREPARE_CONCURRENCY", "4")
# Six fits the current suite's heaviest mixed-resource wave on the local Docker VM.
# Lighter suites can raise this without changing the eval definition.
eval_concurrency := env_var_or_default("HARBOR_EVAL_CONCURRENCY", "6")

default: run

# Install development dependencies once. Dataset downloads remain Harbor's job.
bootstrap:
    uv sync --frozen
    cargo fetch --locked

# Tight inner loop: native model process with local code mode, no Harbor or Docker.
run:
    @cargo run --quiet -- run --model=gpt-5.6-sol --effort=low < examples/task-start.jsonl

# Build a static Linux executable for the Docker daemon's native architecture.
# This is a native container build, not an amd64 cross-compile on Apple Silicon.
build-agent:
    @mkdir -p "{{agent_artifact_dir}}"
    @echo "Building native Linux agent artifact (Cargo profile: {{build_profile}})..."
    @docker build --quiet --build-arg CARGO_PROFILE="{{build_profile}}" --file harbor_adapter/harness.Dockerfile --target artifact --output type=local,dest="{{agent_artifact_dir}}" .
    @test -x "{{agent_artifact}}"

# Pay native task/verifier image construction once, outside measured eval jobs.
# The no-op agent performs no model call, verification, or harness build.
prepare-evals config=default_eval:
    @test -x "{{harbor}}" || { echo "run 'just bootstrap' first" >&2; exit 2; }
    @job_name="$(date +%Y-%m-%d__%H-%M-%S)-prepare-evals-$BASHPID"; \
        HARBOR_TELEMETRY=off "{{harbor}}" run --config "{{config}}" --agent nop --install-only --jobs-dir "{{setup_jobs}}" --job-name "$job_name" --n-concurrent "{{prepare_concurrency}}"

# Prepare only the task being added to the benchmark ladder.
prepare-task task config=default_eval:
    @test -x "{{harbor}}" || { echo "run 'just bootstrap' first" >&2; exit 2; }
    @task="{{task}}"; \
        dataset=$(HARBOR_TELEMETRY=off "{{harbor}}" run --config "{{config}}" --print-config | jq -er '.datasets | if length == 1 then .[0] | "\(.name)@\(.ref)" else error("expected exactly one dataset") end'); \
        job_name="$(date +%Y-%m-%d__%H-%M-%S)-prepare-${task##*/}-$BASHPID"; \
        HARBOR_TELEMETRY=off "{{harbor}}" run --config "{{config}}" --dataset "$dataset" --include-task-name "$task" --agent nop --install-only --jobs-dir "{{setup_jobs}}" --job-name "$job_name" --n-concurrent 1

# Run a Harbor-native job config. Rust executes inside each benchmark container.
eval config=default_eval: build-agent
    @test -x "{{harbor}}" || { echo "run 'just bootstrap' first" >&2; exit 2; }
    @job_name="$(date +%Y-%m-%d__%H-%M-%S)-eval-$BASHPID"; \
        HARBOR_TELEMETRY=off "{{harbor}}" run --config "{{config}}" --job-name "$job_name" --n-concurrent "{{eval_concurrency}}"

# Run one registry task through the configured agent, environment, and verifier.
eval-task task effort="low" config=default_eval: build-agent
    @test -x "{{harbor}}" || { echo "run 'just bootstrap' first" >&2; exit 2; }
    @task="{{task}}"; \
        dataset=$(HARBOR_TELEMETRY=off "{{harbor}}" run --config "{{config}}" --print-config | jq -er '.datasets | if length == 1 then .[0] | "\(.name)@\(.ref)" else error("expected exactly one dataset") end'); \
        job_name="$(date +%Y-%m-%d__%H-%M-%S)-${task##*/}-$BASHPID"; \
        HARBOR_TELEMETRY=off "{{harbor}}" run --config "{{config}}" --dataset "$dataset" --include-task-name "$task" --job-name "$job_name" --agent-kwarg "effort={{effort}}"

# Open all locally retained Harbor jobs unless another jobs directory is supplied.
view jobs=default_jobs:
    @test -x "{{harbor}}" || { echo "run 'just bootstrap' first" >&2; exit 2; }
    @test -d "{{jobs}}" || { echo "no Harbor jobs at {{jobs}}; run 'just eval' first" >&2; exit 2; }
    @HARBOR_TELEMETRY=off "{{harbor}}" view --jobs "{{jobs}}"

# Checks stay small until the end-to-end agent path is real.
check:
    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test
    .venv/bin/python -m compileall -q harbor_adapter
    "{{harbor}}" run --config "{{default_eval}}" --print-config >/dev/null
