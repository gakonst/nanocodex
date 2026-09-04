# Nanocodex for Python

The `nanocodex` wheel embeds the native Rust agent lifecycle. An agent owns its
WebSocket, typed history, response chain, cache identity, tools, and cleanup;
Python receives a cheap command handle and an optional independent event
receiver.

## One complete turn

Python 3.11 or newer is required.

```python
import os

from nanocodex import Nanocodex

agent, events = Nanocodex(
    os.environ["OPENAI_API_KEY"],
    instructions=(
        "You are a Rust coding agent. Preserve unrelated work and run "
        "the tests relevant to each change."
    ),
)

turn = agent.prompt("Explain why the parser test is failing.")
result = turn.result()
print(result.final_message)
usage = result.usage()
if estimated_cost := usage["estimated_cost"]:
    print(estimated_cost["usd"])
else:
    print(usage["cost_status"])

agent.shutdown()
```

`prompt()` waits only for the driver to accept the work. It returns a `Turn`
whose blocking `result()` releases the GIL and produces a typed `TurnResult`.
Events do not need to be consumed for results to complete.

## Follow-on turns and events

The same agent automatically reuses retained history and its persistent
WebSocket:

```python
import os
import threading

from nanocodex import Nanocodex

agent, events = Nanocodex(
    os.environ["OPENAI_API_KEY"],
    model="gpt-5.6-luna",
    thinking="low",
)


def print_events() -> None:
    while event := events.recv():
        print(event.seq, event.kind, event.payload)


event_thread = threading.Thread(target=print_events)
event_thread.start()

first = agent.prompt("Remember the identifier PYO3_17.").result()
second = agent.prompt("Return the identifier I asked you to remember.").result()
print(first.final_message)
print(second.final_message)

agent.shutdown()
event_thread.join()
```

Every typed event contains `protocol_version`, `request_id`, monotonic `seq`,
`kind`, and a native Python `payload`; `kind` is serialized as `type` by the
JSON convenience path. `events.request_id` equals `agent.session_id`.
`recv_json()` remains available when the embedding application already owns
a JSONL boundary.

## Lifecycle controls

```python
turn = agent.prompt("Inspect the parser and propose a fix.")
turn.steer("Keep the public grammar unchanged.")
result = turn.result()

branch, branch_events = agent.fork_from(result)
latest, latest_events = agent.fork()
sibling, sibling_events = agent.spawn()

agent.compact()
agent.set_thinking("high")
agent.set_fast_mode(True)

branch.shutdown()
latest.shutdown()
sibling.shutdown()
agent.shutdown()
```

- `steer()` adds input at the next safe model boundary.
- `cancel()` stops that exact active or queued turn.
- `fork_from(result)` starts from an exact completed historical boundary.
- `fork()` starts from the latest safe boundary.
- `spawn()` creates a clean sibling with the same private configuration.
- `compact()` replaces retained history with a model-generated compaction
  without fabricating a user prompt.

Call `shutdown()` at an application or session boundary. It cancels unfinished
turns, joins model and tool resources, and invalidates that Python handle.

## Snapshots and resume

Snapshots contain the complete unredacted model-visible conversation. Protect
them like the underlying prompts and tool output.

```python
completed = agent.prompt("Remember the exact identifier SNAP_42.").result()
encoded = completed.snapshot().to_json()

from nanocodex import SessionSnapshot

snapshot = SessionSnapshot.from_json(encoded)
resumed, resumed_events = Nanocodex(
    os.environ["OPENAI_API_KEY"],
    instructions=(
        "You are a Rust coding agent. Preserve unrelated work and run "
        "the tests relevant to each change."
    ),
    resume=snapshot,
)
answer = resumed.prompt("Which identifier did I provide?").result()
print(answer.final_message)
resumed.shutdown()
```

Resume with the same instructions, tools, and workspace policy used to create
the snapshot.

## Authentication and advanced client settings

Pass an API key positionally, or load subscription credentials created by
`nanocodex auth login`:

```python
agent, events = Nanocodex(auth_file="/path/to/.codex/auth.json")
agent.shutdown()
```

Pro is an execution mode, not a model slug. GPT-6 Astra is selected with
`model="gpt-6-astra"`, requires `thinking="low"` or greater, and currently
supports only standard reasoning mode:

```python
agent, events = Nanocodex(
    api_key,
    model="gpt-6-astra",
    reasoning_mode="standard",
    thinking="xhigh",
    fast_mode=True,
)
agent.shutdown()
```

`session_id` accepts an explicit UUIDv7 identity. `prompt_cache_key` selects a
stable immutable-prefix cache identity. `websocket_url` and `api_base_url`
replace the matching OpenAI client endpoints for gateways and deterministic
integration tests.

## Develop and verify

From the repository root:

```sh
uv venv --python 3.11 py/bindings/.venv
uv pip install --python py/bindings/.venv/bin/python 'maturin>=1.9,<2'
VIRTUAL_ENV="$PWD/py/bindings/.venv" \
  py/bindings/.venv/bin/maturin develop \
  --manifest-path py/bindings/Cargo.toml
py/bindings/.venv/bin/python -m unittest discover \
  -s py/bindings/tests -v
py/bindings/.venv/bin/python \
  py/bindings/benchmarks/benchmark_binding.py --check
```

Python agents share one process-wide async runtime. Each driver lazily starts
one intentional Code Mode worker after its first model I/O, reuses it while
live, and joins it during `shutdown()`. CI separately gates import,
construction, prompt-acceptance, and result p50/p95; retained shared threads;
eight-agent concurrency; and packed and unpacked wheel size.

Runnable live consumers are under
[`examples/python`](../../examples/python).
