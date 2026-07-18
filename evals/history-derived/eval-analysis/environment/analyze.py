def compare(records, left, right):
    by_agent = {
        left: [record for record in records if record["agent"] == left],
        right: [record for record in records if record["agent"] == right],
    }
    passed = {
        agent: {record["task"] for record in values if record["reward"] == 1}
        for agent, values in by_agent.items()
    }
    shared = sorted(passed[left] & passed[right])
    faster = {left: [], right: []}
    for task in {record["task"] for record in records}:
        times = {
            agent: next(record["seconds"] for record in values if record["task"] == task)
            for agent, values in by_agent.items()
        }
        winner = min(times, key=times.get)
        faster[winner].append(task)
    usage = {}
    for agent, values in by_agent.items():
        rates = [record["cached_input_tokens"] / record["input_tokens"] for record in values]
        usage[agent] = {
            "input_tokens": sum(record["input_tokens"] for record in values),
            "cached_input_tokens": sum(record["cached_input_tokens"] for record in values),
            "output_tokens": sum(record["output_tokens"] for record in values),
            "cache_rate": sum(rates) / len(rates),
        }
    return {
        "score": {agent: [len(passed[agent]), len(values)] for agent, values in by_agent.items()},
        "shared_passes": shared,
        "faster": faster,
        "usage": usage,
    }
