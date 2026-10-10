#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Intentionally uses the test profile: compare like-for-like, not production throughput.
# Test includes 5 discarded warmups, 101 samples, nearest-rank p99, output validation.
cargo test --locked -p nanocodex-agent --lib rollout_perf -- --ignored --nocapture
