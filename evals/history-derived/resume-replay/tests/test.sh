#!/bin/sh
set -u

mkdir -p /logs/verifier /app/tests
cp /tests/hidden.rs /app/tests/hidden.rs
if cargo test --quiet --manifest-path /app/Cargo.toml; then
  printf '1\n' > /logs/verifier/reward.txt
  rm -f /app/tests/hidden.rs
else
  status=$?
  printf '0\n' > /logs/verifier/reward.txt
  rm -f /app/tests/hidden.rs
  exit "$status"
fi
