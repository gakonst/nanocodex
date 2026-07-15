set dotenv-load := true
set shell := ["bash", "-euo", "pipefail", "-c"]

harbor := ".venv/bin/harbor"
fast_stop_compose := "harbor/fast-stop.compose.yaml"
fast_eval_compose := "harbor/fast-eval.compose.yaml"
fast_canary_image := "harness/fix-git-verifier:20251031-local-v2"
local_docker_environment := "harbor_adapter.local_docker:LocalDockerEnvironment"
canary_ref := "terminal-bench/fix-git@sha256:66be7179f07f1aa8f0d60f88800a883a68c1ffb7a349aae76aa60fa679485473"
canary_task_default := env_var("HOME") + "/.cache/harbor/tasks/packages/terminal-bench/fix-git/66be7179f07f1aa8f0d60f88800a883a68c1ffb7a349aae76aa60fa679485473"
canary_task := env_var_or_default("HARBOR_CANARY_TASK", canary_task_default)
fast_canary_task := ".harness/harbor/tasks/fix-git/66be7179f07f1aa8f0d60f88800a883a-fast"
harbor_jobs := ".harness/harbor/jobs"

default: run

# Install the pinned Harbor environment and fetch Rust dependencies once.
bootstrap:
    uv sync --frozen
    cargo fetch --locked
    test -d "{{canary_task_default}}" || HARBOR_TELEMETRY=off "{{harbor}}" download "{{canary_ref}}" --cache
    mkdir -p "$(dirname "{{fast_canary_task}}")"
    test -d "{{fast_canary_task}}" || cp -R "{{canary_task_default}}" "{{fast_canary_task}}"
    cp harbor/fast-verifier.sh "{{fast_canary_task}}/tests/test.sh"
    chmod +x "{{fast_canary_task}}/tests/test.sh"
    docker image inspect "{{fast_canary_image}}" >/dev/null 2>&1 || docker build --file "$PWD/harbor/fast-verifier.Dockerfile" --tag "{{fast_canary_image}}" "{{canary_task_default}}/environment"

# Run the local stdin/stdout JSONL transport probe.
run:
    @cargo run --quiet -- run < examples/task-start.jsonl

# Check formatting and warnings without introducing a unit-test suite.
check:
    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    "{{harbor}}" --version >/dev/null
    .venv/bin/python -m compileall -q harbor_adapter

# Exercise our local process through Harbor and a cached Terminal-Bench task.
harbor-probe task=canary_task:
    @test -x "{{harbor}}" || { echo "run 'just bootstrap' first" >&2; exit 2; }
    @test -d "{{task}}" || { echo "canary task is missing; run 'just bootstrap' first" >&2; exit 2; }
    @mkdir -p "{{harbor_jobs}}/probe"
    @echo "Probe: real Harbor container and harness adapter; verifier disabled."
    @HARBOR_TELEMETRY=off "{{harbor}}" run --path "{{task}}" --agent harbor_adapter.agent:HarnessAgent --env "{{local_docker_environment}}" --disable-verification --no-force-build --extra-docker-compose "{{fast_stop_compose}}" --jobs-dir "{{harbor_jobs}}/probe"

# Run the canary with its assertions unchanged and verifier dependencies baked.
harbor-eval:
    @test -x "{{harbor}}" || { echo "run 'just bootstrap' first" >&2; exit 2; }
    @test -d "{{fast_canary_task}}" || { echo "fast canary is missing; run 'just bootstrap' first" >&2; exit 2; }
    @docker image inspect "{{fast_canary_image}}" >/dev/null 2>&1 || { echo "fast verifier image is missing; run 'just bootstrap' first" >&2; exit 2; }
    @mkdir -p "{{harbor_jobs}}/eval"
    @echo "Eval: one Terminal-Bench task plus the pre-baked local verifier."
    @echo "Phase 0 has no model or tools, so reward 0 is expected; success here means one completed trial and zero errors."
    @HARBOR_TELEMETRY=off "{{harbor}}" run --path "{{fast_canary_task}}" --agent harbor_adapter.agent:HarnessAgent --env "{{local_docker_environment}}" --no-force-build --extra-docker-compose "{{fast_eval_compose}}" --jobs-dir "{{harbor_jobs}}/eval"
    @echo "Inspect the run with: just harbor-view"

# Run the untouched downloaded task, including its per-trial dependency setup.
harbor-eval-canonical task=canary_task:
    @test -x "{{harbor}}" || { echo "run 'just bootstrap' first" >&2; exit 2; }
    @test -d "{{task}}" || { echo "canary task is missing; run 'just bootstrap' first" >&2; exit 2; }
    @mkdir -p "{{harbor_jobs}}/canonical"
    @echo "Canonical eval: untouched downloaded Terminal-Bench task and verifier."
    @HARBOR_TELEMETRY=off "{{harbor}}" run --path "{{task}}" --agent harbor_adapter.agent:HarnessAgent --no-force-build --extra-docker-compose "{{fast_stop_compose}}" --jobs-dir "{{harbor_jobs}}/canonical"

# Open Harbor's job viewer for local probe and eval results.
harbor-view kind="eval":
    @test -x "{{harbor}}" || { echo "run 'just bootstrap' first" >&2; exit 2; }
    @test -d "{{harbor_jobs}}/{{kind}}" || { echo "no {{kind}} jobs yet; run 'just harbor-{{kind}}' first" >&2; exit 2; }
    @HARBOR_TELEMETRY=off "{{harbor}}" view --jobs "{{harbor_jobs}}/{{kind}}"
