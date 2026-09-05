#!/bin/sh
set -eu

answer=$(cat "$NANOCODEX_EVAL_WORKSPACE/answer.txt")
if [ "$answer" = "NANOCODEX_ADAPTER_SMOKE_OK" ]; then
  printf '1\n' > /logs/verifier/reward.txt
else
  printf '0\n' > /logs/verifier/reward.txt
fi
