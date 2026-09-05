#!/bin/sh
set -eu

logs=${NANOEVAL_VERIFIER_LOGS:-/logs/verifier}
mkdir -p "$logs"

if [ -f message.upper.txt ] \
    && printf 'NANOEVAL\nRUNS IN PARALLEL\n' | cmp -s - message.upper.txt \
    && printf 'Nanoeval\nruns in parallel\n' | cmp -s - message.txt; then
    printf '1\n' > "$logs/reward.txt"
    printf 'message.upper.txt is correct\n'
else
    printf '0\n' > "$logs/reward.txt"
    printf 'message transformation is missing or incorrect\n' >&2
    exit 1
fi
