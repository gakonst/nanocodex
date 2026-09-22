#!/bin/sh
set -eu
cd "$(dirname "$0")/../.."
LUA=${LUA:-lua}
for test in addon/tests/*_test.lua; do
    "$LUA" "$test"
done
