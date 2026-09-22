# Runtime prompt fidelity

`prompts.py` verifies the exact bytes consumed by the three supported OpenAI model
configurations, native permissions, managed goal continuation, and realtime voice.
It requires one clean **external** Codex repository with both revisions from
`prompts.json` available to `jj`:

```sh
python3 scripts/codex-parity/prompts.py /path/to/codex
# Refresh only the explicit consumed outputs:
python3 scripts/codex-parity/prompts.py /path/to/codex --write
```

The manifest's `upstream` revision owns the existing permission, goal, voice, and
Astra prompt sources. `model_upstream` owns the Sol and Luna model catalog fields.
The manifest records SHA-256 hashes for each consumed source, nine runtime outputs,
three model catalog fields, and six external composition references. Together with
the manifest itself, the script verifies ten generated files. The catalog and Rust
reference files remain in the external repository. No optional
prompt modes, vendored upstream source/tests, runtime inventory, or generated tool
catalog are included. Adding a mode requires its own runtime integration and tests
first.

Native execution supplies its enforced full-access/never facts. Hosted WASM omits
that native filesystem claim and defers permission facts to its host. Goal values
are interpolated strictly in a single pass; user objectives are XML escaped.
Realtime boundaries match the upstream fragment wrapper. Model prompt bytes,
including upstream identity and trailing spaces, are preserved; explicit caller
overrides and additional instructions continue to work.

Focused validation:

```sh
cargo test -p nanocodex-oai-api -p nanocodex-voice-protocol -p nanocodex-agent --lib --locked
pnpm --dir js/managed exec vitest run test/codex-prompts.test.ts test/goals.test.ts
```
