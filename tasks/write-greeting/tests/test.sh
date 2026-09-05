#!/bin/sh
set -eu

logs=${NANOEVAL_VERIFIER_LOGS:-/logs/verifier}
mkdir -p "$logs"

if [ -f greeting.txt ] && [ "$(cat greeting.txt)" = "hello from nanoeval" ] && [ "$(wc -l < greeting.txt | tr -d ' ')" = "1" ]; then
    printf '1\n' > "$logs/reward.txt"
    printf 'greeting.txt is correct\n'
else
    printf '0\n' > "$logs/reward.txt"
    printf 'greeting.txt is missing or incorrect\n' >&2
    exit 1
fi
