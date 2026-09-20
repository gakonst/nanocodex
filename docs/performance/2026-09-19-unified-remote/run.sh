#!/bin/sh
set -eu
# Execute from repository root; uses only isolated container filesystem.
name=nanocodex-unified-perf-20260919
if ! docker inspect "$name" >/dev/null 2>&1; then
  docker run -d --name "$name" --cpus 2 --memory 4g -p 127.0.0.1:18769:8769 --entrypoint /bin/sh nanocodex-durable-agent-sandbox:worker -c 'sleep infinity'
fi
docker cp docs/performance/2026-09-19-unified-remote/. "$name":/perf
docker exec "$name" sh -c 'chmod +x /perf/synthetic-waymote.py; python3 -c "import aiohttp,playwright"'
docker exec -d "$name" sh -c 'python3 /perf/harness.py --synthetic --label go-synthetic > /perf/run-synthetic.log 2>&1'
printf '%s\n' 'Started. After completion: docker cp nanocodex-unified-perf-20260919:/perf/go-synthetic-result.json docs/performance/2026-09-19-unified-remote/'
