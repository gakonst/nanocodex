# Python binding

This Maturin/PyO3 package embeds the native Nanocodex runtime in Python. One
`Nanocodex` object owns the persistent Rust agent session, so follow-on prompts
reuse its WebSocket, retained history, response chain, and prompt-cache key.

```sh
python -m venv bindings/python/.venv
bindings/python/.venv/bin/pip install maturin
bindings/python/.venv/bin/maturin develop -m bindings/python/Cargo.toml
bindings/python/.venv/bin/python examples/python/follow_on.py
```

`prompt()` only accepts the turn and returns a `Turn`; `Turn.result()` does the
blocking wait while releasing Python's GIL. `AgentEvents.recv_json()` likewise
releases the GIL, so applications can consume it from a normal Python thread.
The Rust runtime, tools, transport, retries, history, and event ordering stay
inside the extension; no app server or per-tool Python bridge is involved.

Pass an API key positionally, or use native subscription credentials created by
`nanocodex auth login`:

```python
agent, events = Nanocodex(auth_file="/path/to/.codex/auth.json")
```

GPT-5.6 Pro is a reasoning mode, not a different model slug. Select it
independently from any supported effort level:

```python
agent, events = Nanocodex(
    api_key,
    reasoning_mode="pro",
    thinking="xhigh",  # none, low, medium, high, xhigh, or max
)
```

Runnable consumers live together at the repository boundary under
[`examples/python`](../../examples/python): `follow_on.py` demonstrates retained
conversation state and `events.py` consumes the ordered event receiver.
