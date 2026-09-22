# Shared Codex tool contracts

Reference: `openai/codex` commit `36430b36881cf5c289cb48e671cfc9e8b542ae7b`.

This change aligns shell approval argument names, plan/image tool metadata, Code
Mode declarations and schema rendering, and bounded image/helper argument
behavior. It retains QuickJS and the existing host execution model.

`python3 scripts/codex-parity/check.py /path/to/pinned/codex` compiles the actual
upstream tool constructors in a serialization-only harness, checks the small
recorded contract fixture, and checks the schema renderer source after import
adaptation. Upstream source is supplied externally, not vendored. Rust and JS
tests assert the public tool shapes and helper cases consumed by this change.

Web schemas, model prompts, compaction, memory, and broader Code Mode execution
semantics are separate changes. This is not a claim of full Codex equivalence.
