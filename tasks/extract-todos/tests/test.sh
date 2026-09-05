#!/bin/sh
set -eu

logs=${NANOEVAL_VERIFIER_LOGS:-/logs/verifier}
mkdir -p "$logs"

if [ -f todos.txt ] \
    && printf 'publish benchmarks\nverify trajectories\n' | cmp -s - todos.txt \
    && grep -q '^- TODO: verify trajectories$' notes.md \
    && grep -q '^- TODO: publish benchmarks$' notes.md; then
    printf '1\n' > "$logs/reward.txt"
    printf 'todos.txt is correct\n'
else
    printf '0\n' > "$logs/reward.txt"
    printf 'TODO extraction is missing or incorrect\n' >&2
    exit 1
fi
