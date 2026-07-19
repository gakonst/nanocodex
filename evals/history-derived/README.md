# History-derived Harbor eval

This local Harbor dataset contains synthetic coding tasks distilled from
recurring patterns in the repository owner's Codex history. It deliberately
does not copy private prompts, repository contents, secrets, session IDs, or
model responses.

Each child directory is a standard Harbor task with:

- `instruction.md` for the agent-visible request;
- `task.toml` for timeouts and sandbox resources;
- `environment/` for the starting repository state;
- `tests/` for the agent-hidden deterministic verifier; and
- `solution/` for an oracle implementation used to validate the task itself.

Run the nanocodex against the complete dataset with:

```sh
just prepare-evals config=evals/history-derived.yaml
just eval config=evals/history-derived.yaml
```

Run the stock Codex 0.144.5 comparison arm with:

```sh
just prepare-evals config=evals/history-derived-codex.yaml
just eval config=evals/history-derived-codex.yaml
```

Before spending model tokens, validate the task environments and oracle
solutions with Harbor:

```sh
HARBOR_TELEMETRY=off .venv/bin/harbor run \
  --path evals/history-derived \
  --agent oracle \
  --jobs-dir .nanocodex/harbor/setup
```

The two context-compaction probes found in history are not included yet. Their
success criteria depend on retained trajectory events (tool-call count,
compaction, and exact dependencies), which a normal filesystem verifier cannot
honestly establish. They should become Harbor tasks only with a trajectory-aware
metric or verifier.
