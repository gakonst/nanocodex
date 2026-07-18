#!/bin/sh
set -eu

cat > /app/analyze.py <<'PY'
def compare(records, left, right):
    latest = {}
    for record in records:
        key = (record["agent"], record["task"])
        current = latest.get(key)
        if current is None or record["completed_at"] >= current["completed_at"]:
            latest[key] = record

    by_agent = {
        agent: {task: record for (owner, task), record in latest.items() if owner == agent}
        for agent in (left, right)
    }
    passed = {
        agent: {
            task
            for task, record in values.items()
            if record["reward"] == 1 and not record["exception"]
        }
        for agent, values in by_agent.items()
    }
    shared = sorted(passed[left] & passed[right])

    faster = {left: [], right: []}
    for task in shared:
        left_seconds = by_agent[left][task]["seconds"]
        right_seconds = by_agent[right][task]["seconds"]
        if left_seconds < right_seconds:
            faster[left].append(task)
        elif right_seconds < left_seconds:
            faster[right].append(task)

    usage = {}
    for agent, values in by_agent.items():
        input_tokens = sum(record["input_tokens"] for record in values.values())
        cached = sum(record["cached_input_tokens"] for record in values.values())
        usage[agent] = {
            "input_tokens": input_tokens,
            "cached_input_tokens": cached,
            "output_tokens": sum(record["output_tokens"] for record in values.values()),
            "cache_rate": cached / input_tokens if input_tokens else 0,
        }

    return {
        "score": {
            agent: [len(passed[agent]), len(values)]
            for agent, values in by_agent.items()
        },
        "shared_passes": shared,
        "faster": faster,
        "usage": usage,
    }
PY

python3 -m unittest -q
