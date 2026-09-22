# Codex web command contract

This change preserves the pinned upstream web command schema and description in
native and JavaScript tools, including PDF screenshots and multi-operation
requests. The account gateway forwards the complete command batch and explicit
model/context instead of selecting a single operation.

`scripts/codex-parity/web.py /path/to/pinned/codex` extracts the schema from
upstream commit `36430b36881cf5c289cb48e671cfc9e8b542ae7b`, checks the native wire
shape, and verifies the small native/JavaScript schema fixtures. Upstream source
is external; no complete tool inventory is copied into this PR.

Provider execution and authentication remain host implementations. This change
does not add every upstream agent-history propagation behavior to the JS host.
