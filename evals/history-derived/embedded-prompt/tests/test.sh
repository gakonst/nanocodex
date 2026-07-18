#!/bin/sh
set -u

mkdir -p /logs/verifier
if python3 /tests/verify.py; then
  printf '1\n' > /logs/verifier/reward.txt
else
  status=$?
  printf '0\n' > /logs/verifier/reward.txt
  exit "$status"
fi
