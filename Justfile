set dotenv-load := true
set shell := ["bash", "-euo", "pipefail", "-c"]
export PYTHONPATH := justfile_directory()

harbor := ".venv/bin/harbor"
build_profile := env_var_or_default("NANOCODEX_BUILD_PROFILE", "dev")
agent_artifact_dir := ".nanocodex/installed"
agent_artifact := agent_artifact_dir + "/nanocodex"
hosted_agent_artifact_dir := agent_artifact_dir + "/daytona-amd64"
hosted_agent_artifact := hosted_agent_artifact_dir + "/nanocodex"
default_eval := "evals/terminal-bench-2.yaml"
default_jobs := ".nanocodex/harbor/jobs"
setup_jobs := ".nanocodex/harbor/setup"
prepare_concurrency := env_var_or_default("HARBOR_PREPARE_CONCURRENCY", "4")
# Six fits the current suite's heaviest mixed-resource wave on the local Docker VM.
# Lighter suites can raise this without changing the eval definition.
eval_concurrency := env_var_or_default("HARBOR_EVAL_CONCURRENCY", "6")
# Cloud sandboxes make trials I/O-bound. Keep this independently tunable from
# the local Docker concurrency, since Daytona account quotas vary.
hosted_eval_concurrency := env_var_or_default("HARBOR_HOSTED_EVAL_CONCURRENCY", "32")
canonical_verifier := "harbor.verifier.verifier:Verifier"

default: run

# Install development dependencies once. Dataset downloads remain Harbor's job.
bootstrap:
    uv sync --frozen
    cargo fetch --locked

# Tight inner loop: native model process with local code mode, no Harbor or Docker.
run:
    @cargo run --quiet --manifest-path bin/nanocodex/Cargo.toml -- run --thinking=low "Use the available exec tool to run pwd exactly once without modifying anything, then report the path."

# Build a static Linux executable for the Docker daemon's native architecture.
# This is a native container build, not an amd64 cross-compile on Apple Silicon.
build-agent:
    @mkdir -p "{{agent_artifact_dir}}"
    @echo "Building native Linux agent artifact (Cargo profile: {{build_profile}})..."
    @docker build --quiet --build-arg CARGO_PROFILE="{{build_profile}}" --file harbor_adapter/nanocodex.Dockerfile --target artifact --output type=local,dest="{{agent_artifact_dir}}" .
    @test -x "{{agent_artifact}}"

# Daytona sandboxes are AMD64 even when Harbor is orchestrated from Apple
# Silicon. Keep this artifact separate from the native local-Docker build.
build-agent-hosted:
    @mkdir -p "{{hosted_agent_artifact_dir}}"
    @echo "Building AMD64 Linux agent artifact for Daytona (Cargo profile: {{build_profile}})..."
    @docker build --quiet --platform linux/amd64 --build-arg CARGO_PROFILE="{{build_profile}}" --file harbor_adapter/nanocodex.Dockerfile --target artifact --output type=local,dest="{{hosted_agent_artifact_dir}}" .
    @test -f "{{hosted_agent_artifact}}" && test -x "{{hosted_agent_artifact}}"

check-hosted-auth:
    @test -n "${DAYTONA_API_KEY:-}" || { test -n "${DAYTONA_JWT_TOKEN:-}" && test -n "${DAYTONA_ORGANIZATION_ID:-}"; } || { echo "set DAYTONA_API_KEY (or DAYTONA_JWT_TOKEN and DAYTONA_ORGANIZATION_ID) in .env" >&2; exit 2; }

# Pay native task and shared verifier-toolbox construction outside measured jobs.
# The no-op agent performs no model call, verification, or nanocodex build.
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

# Run the same pinned task selection in hosted Daytona sandboxes. Harbor still
# writes the job record locally; use `harbor upload` separately to share it.
eval-hosted config=default_eval: check-hosted-auth build-agent-hosted
    @test -x "{{harbor}}" || { echo "run 'just bootstrap' first" >&2; exit 2; }
    @job_name="$(date +%Y-%m-%d__%H-%M-%S)-eval-daytona-$BASHPID"; \
        HARBOR_TELEMETRY=off "{{harbor}}" run --config "{{config}}" --env daytona --verifier "{{canonical_verifier}}" --agent-kwarg "binary_path={{hosted_agent_artifact}}" --agent-kwarg "install_node=true" --job-name "$job_name" --n-concurrent "{{hosted_eval_concurrency}}"

eval-task-hosted task effort="low" config=default_eval: check-hosted-auth build-agent-hosted
    @test -x "{{harbor}}" || { echo "run 'just bootstrap' first" >&2; exit 2; }
    @task="{{task}}"; \
        dataset=$(HARBOR_TELEMETRY=off "{{harbor}}" run --config "{{config}}" --print-config | jq -er '.datasets | if length == 1 then .[0] | "\(.name)@\(.ref)" else error("expected exactly one dataset") end'); \
        job_name="$(date +%Y-%m-%d__%H-%M-%S)-${task##*/}-daytona-$BASHPID"; \
        HARBOR_TELEMETRY=off "{{harbor}}" run --config "{{config}}" --env daytona --verifier "{{canonical_verifier}}" --dataset "$dataset" --include-task-name "$task" --job-name "$job_name" --agent-kwarg "binary_path={{hosted_agent_artifact}}" --agent-kwarg "install_node=true" --agent-kwarg "effort={{effort}}"

# Open all locally retained Harbor jobs unless another jobs directory is supplied.
view jobs=default_jobs:
    @test -x "{{harbor}}" || { echo "run 'just bootstrap' first" >&2; exit 2; }
    @test -d "{{jobs}}" || { echo "no Harbor jobs at {{jobs}}; run 'just eval' first" >&2; exit 2; }
    @HARBOR_TELEMETRY=off "{{harbor}}" view --jobs "{{jobs}}"

# Checks stay small until the end-to-end agent path is real.
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace
    .venv/bin/python -m unittest discover -s harbor_adapter -p 'test_*.py'
    .venv/bin/python -m compileall -q harbor_adapter
    "{{harbor}}" run --config "{{default_eval}}" --print-config >/dev/null
    "{{harbor}}" run --config "{{default_eval}}" --env daytona --verifier "{{canonical_verifier}}" --agent-kwarg "binary_path={{hosted_agent_artifact}}" --agent-kwarg "install_node=true" --print-config >/dev/null
