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
python_binding_venv := "py/bindings/.venv"
python_binding_bin := python_binding_venv + "/bin/python"
python_binding_maturin := python_binding_venv + "/bin/maturin"
wasm_target := "wasm32-unknown-unknown"

default: run

# Install development dependencies once. Dataset downloads remain Harbor's job.
bootstrap:
    uv sync --frozen
    cargo fetch --locked

# Install development tooling for the embedded language bindings.
bootstrap-bindings:
    uv venv "{{python_binding_venv}}"
    uv pip install --python "{{python_binding_bin}}" "maturin>=1.9,<2"
    rustup target add "{{wasm_target}}"
    npm ci --prefix js/bindings
    npm ci --prefix examples/node
    npm ci --prefix examples/react-vite

# Compile and install the PyO3 extension into its isolated development environment.
build-python:
    @test -x "{{python_binding_maturin}}" || { echo "run 'just bootstrap-bindings' first" >&2; exit 2; }
    VIRTUAL_ENV="{{justfile_directory()}}/{{python_binding_venv}}" "{{python_binding_maturin}}" develop --manifest-path py/bindings/Cargo.toml

# Run boundary tests. The live follow-on test activates when OPENAI_API_KEY is set.
test-python: build-python
    "{{python_binding_bin}}" -m unittest discover -s py/bindings/tests -v

# Run the persistent Python follow-on example against the live Responses API.
smoke-python: build-python
    "{{python_binding_bin}}" examples/python/follow_on.py

# Build one Rust/WASM artifact and generate both Node.js and browser bindings.
build-wasm:
    @command -v wasm-bindgen >/dev/null || { echo "install wasm-bindgen-cli matching Cargo.lock" >&2; exit 2; }
    ./scripts/build-js-package.sh

# Exercise the real WASM model loop under Node and the browser host contract.
test-wasm: build-wasm
    npm test --prefix js/bindings
    npm test --prefix js/react

# Run custom JavaScript tooling and a follow-on through Node-hosted WASM.
smoke-wasm-node: build-wasm
    npm ci --prefix examples/node
    npm start --prefix examples/node

# Type-check and bundle the React Worker example against the generated web WASM package.
build-react-example: build-wasm
    npm run build --prefix examples/react-vite

# Run the React frontend and API Worker together in Cloudflare's Vite environment.
dev-react-example:
    CLOUDFLARE_INCLUDE_PROCESS_ENV=true npm run dev --prefix examples/react-vite -- --host 127.0.0.1

# Exercise background MCP discovery, Code Mode tool_search, and one MCP call.
smoke-mcp:
    cargo run --quiet -p nanocodex-examples --bin mcp

# Start the ephemeral localhost Jaeger backend used by the OTLP trace demo.
otel-up:
    @docker compose -f docker-compose.otel.yml up --detach
    @for attempt in {1..50}; do \
        if curl --fail --silent http://127.0.0.1:16686/ >/dev/null; then exit 0; fi; \
        if [ "$attempt" -eq 50 ]; then echo "Jaeger did not become ready within 10 seconds" >&2; exit 1; fi; \
        sleep 0.2; \
    done
    @echo "Jaeger UI: http://127.0.0.1:16686"

# Launch the interactive TUI with OTLP export, loading OPENAI_API_KEY from .env.
run-otel: otel-up
    @test -n "${OPENAI_API_KEY:-}" || { echo "set OPENAI_API_KEY in .env or the environment" >&2; exit 2; }
    @echo "Building and launching the Nanocodex TUI..."
    @cargo run --manifest-path bin/nanocodex/Cargo.toml -- \
        --otel-endpoint http://127.0.0.1:4318 \
        --otel-environment local-tui

# Launch the TUI and export per-event/per-frame streaming diagnostics in addition
# to the compact per-turn summaries enabled by `run-otel`.
run-otel-detail: otel-up
    @test -n "${OPENAI_API_KEY:-}" || { echo "set OPENAI_API_KEY in .env or the environment" >&2; exit 2; }
    @echo "Building and launching the Nanocodex TUI with detailed stream timing..."
    @OTEL_LEVEL="warn,nanocodex=info,nanocodex_service=info,nanocodex_tools=info,nanocodex_mcp=info,nanocodex_stream_timing=trace" \
        cargo run --manifest-path bin/nanocodex/Cargo.toml -- \
        --otel-endpoint http://127.0.0.1:4318 \
        --otel-environment local-tui

# Focused streaming-performance gate: private event-envelope overhead plus
# trace-shaped transcript updates and steady Ratatui diff rendering.
bench-stream:
    cargo bench -p nanocodex-service --bench tower_responses -- timed_agent_event_delivery
    cargo bench -p nanocodex-bin --bench tui_render -- tui_stream_telemetry
    cargo bench -p nanocodex-bin --bench tui_render -- tui_transcript_delta
    cargo bench -p nanocodex-bin --bench tui_render -- tui_trace_render

# Run a tool-using turn and retain events and diagnostic logs independently.
otel-demo:
    @test -n "${OPENAI_API_KEY:-}" || { echo "set OPENAI_API_KEY in .env or the environment" >&2; exit 2; }
    @curl --fail --silent --show-error http://127.0.0.1:16686/ >/dev/null || { echo "run 'just otel-up' first" >&2; exit 2; }
    @mkdir -p .nanocodex/otel-demo
    @rm -f .nanocodex/otel-demo/events.jsonl .nanocodex/otel-demo/tracing.jsonl
    @cargo run --quiet --manifest-path bin/nanocodex/Cargo.toml -- \
        run \
        --otel-endpoint http://127.0.0.1:4318 \
        --otel-environment local-demo \
        --log-format json \
        --log-file .nanocodex/otel-demo/tracing.jsonl \
        --thinking=low "Use the available exec tool to run pwd exactly once without modifying anything, then report the path." \
        > .nanocodex/otel-demo/events.jsonl
    @jq --compact-output 'select(.type == "assistant.message" or .type == "tool.started" or .type == "tool.result" or .type == "run.completed") | {type, payload}' .nanocodex/otel-demo/events.jsonl
    @echo "Open http://127.0.0.1:16686 and select service 'nanocodex'."

# Run the deterministic retained-session and hostile-tool observability stress.
otel-stress turns="32" parallel_calls="16":
    @curl --fail --silent --show-error http://127.0.0.1:16686/ >/dev/null || { echo "run 'just otel-up' first" >&2; exit 2; }
    NANOCODEX_STRESS_TURNS="{{turns}}" \
        NANOCODEX_STRESS_PARALLEL_CALLS="{{parallel_calls}}" \
        cargo test --locked --manifest-path bin/nanocodex/Cargo.toml \
        --test observability_stress -- \
        --ignored --exact retained_turns_and_hostile_tools_preserve_trace_topology \
        --nocapture --test-threads=1

# Verify that attached child-agent turns share and overlap in their parent trace.
otel-subagent-stress:
    @curl --fail --silent --show-error http://127.0.0.1:16686/ >/dev/null || { echo "run 'just otel-up' first" >&2; exit 2; }
    cargo test --locked --manifest-path bin/nanocodex/Cargo.toml \
        --test observability_stress -- \
        --ignored --exact attached_subagents_share_the_parent_trace_and_overlap \
        --nocapture --test-threads=1

# Run the identical workload without installing the OTLP layer for comparison.
otel-stress-baseline turns="32" parallel_calls="16":
    NANOCODEX_STRESS_EXPORT=false \
        NANOCODEX_STRESS_TURNS="{{turns}}" \
        NANOCODEX_STRESS_PARALLEL_CALLS="{{parallel_calls}}" \
        cargo test --locked --manifest-path bin/nanocodex/Cargo.toml \
        --test observability_stress -- \
        --ignored --exact retained_turns_and_hostile_tools_preserve_trace_topology \
        --nocapture --test-threads=1

# Stop Jaeger and discard its in-memory trace data.
otel-down:
    @docker compose -f docker-compose.otel.yml down

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

# Validate the versioned artifacts before creating a release tag.
release-check version:
    @workspace_version=$(cargo metadata --no-deps --format-version 1 | jq -er '.packages[] | select(.name == "nanocodex") | .version'); \
        test "{{version}}" = "$workspace_version" || { echo "expected workspace version {{version}}, found $workspace_version" >&2; exit 1; }
    @js_version=$(node -p "require('./js/bindings/package.json').version"); \
        test "{{version}}" = "$js_version" || { echo "expected JavaScript package version {{version}}, found $js_version" >&2; exit 1; }
    @cargo metadata --no-deps --format-version 1 | jq -e --arg version "{{version}}" \
        '[.packages[].dependencies[] | select(.source == null and (.name | startswith("nanocodex"))) | .req] | all(. == ("^" + $version))' >/dev/null
    @grep -Fq "## [{{version}}]" CHANGELOG.md
    @grep -Fq '<!-- generated by git-cliff -->' CHANGELOG.md
    @for crate in nanocodex-core nanocodex-macros nanocodex-observability nanocodex-service nanocodex-tools nanocodex-mcp nanocodex; do \
        grep -Fq "## [{{version}}]" "crates/$crate/CHANGELOG.md"; \
        grep -Fq '<!-- generated by git-cliff -->' "crates/$crate/CHANGELOG.md"; \
    done
    bash -n install scripts/changelog.sh scripts/check-docs.sh scripts/publish-crates.sh
    @for crate in nanocodex-core nanocodex-macros nanocodex-observability nanocodex-service nanocodex-tools nanocodex-mcp nanocodex; do \
        cargo package --locked --allow-dirty --no-verify --config .cargo/release.toml --package "$crate"; \
    done
    ./scripts/check-docs.sh

# Regenerate the committed Alloy-style changelog for a release preparation PR.
changelog version:
    @command -v git-cliff >/dev/null || { echo "install git-cliff first: cargo install git-cliff --locked" >&2; exit 2; }
    ./scripts/changelog.sh --tag "v{{version}}"
