#!/usr/bin/env bash
set -euo pipefail
# Run from the repository root with Node 24 and pnpm workspace dependencies.
# Linux also needs util-linux, iproute2, openssl, and permitted unprivileged
# user/network/mount namespaces for the real TLS callback fixture.
pnpm --filter nanocodex-connect-protocol build
pnpm --filter @nanocodex/connect-api typecheck
mkdir -p output/mcp-events
node --test js/connect-api/test/mcpServerWorker.test.mjs js/connect-api/test/mcpEventsWorker.test.mjs 2>&1 | tee output/mcp-events/journeys.log
