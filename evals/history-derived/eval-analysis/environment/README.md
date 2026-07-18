# Honest A/B aggregation

`compare(records, left, right)` receives dictionaries with `agent`, `task`,
`completed_at`, `reward`, `exception`, `seconds`, `input_tokens`,
`cached_input_tokens`, and `output_tokens`.

- Keep only the latest record for each `(agent, task)`.
- A pass has reward `1` and no exception.
- Report each arm's passed and total latest tasks.
- `shared_passes` is the sorted intersection of passed task names.
- Compare speed only on shared passes; ties belong to neither arm.
- Usage totals include every latest task for that arm, including misses.
- Cache rate is aggregate cached input divided by aggregate input, never the
  average of per-task percentages. Zero input gives rate `0`.

Return a dictionary with `score`, `shared_passes`, `faster`, and `usage`.
Scores are `[passed, total]` pairs and faster-task lists are sorted.
