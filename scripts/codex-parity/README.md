# Runtime prompt fidelity

`prompts.py` verifies the exact bytes consumed by the four supported model
configurations, native permissions, managed goal continuation, and realtime voice.
It requires a clean **external** Codex checkout at the revision in `prompts.json`:

```sh
python3 scripts/codex-parity/prompts.py /path/to/codex
# Refresh only the explicit consumed outputs:
python3 scripts/codex-parity/prompts.py /path/to/codex --write
```

The manifest records SHA-256 hashes for each consumed source and output, the four
model catalog fields, and six external composition references. The catalog and
Rust reference files remain in that external checkout. No optional prompt modes,
vendored upstream source/tests, runtime inventory, or generated tool catalog are
included. Adding a mode requires its own runtime integration and tests first.

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
