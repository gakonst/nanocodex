# Pinned Codex prompt and tool parity

Reference: `openai/codex` commit `36430b36881cf5c289cb48e671cfc9e8b542ae7b`.
This is a scoped parity audit, **not a claim of complete runtime equivalence**.
Custom subagents are excluded.

## Reproduce the contract checks

Run `python3 scripts/codex-parity/check.py /path/to/pinned/codex` (add
`--write` only to regenerate evidence). The script rejects a different upstream
HEAD or tracked modifications. It compiles upstream shell, plan, view-image and
wait constructors, the upstream Code Mode declaration renderer, and the upstream
image-generation argument derive/schema normalization into a serialization-only
program. No provider credentials or live API calls are involved.

`crates/nanocodex-tools/tests/fixtures/codex-parity/shared-tools.json` records the
result. Rust `code_mode_description::parity_tests` compares local definitions,
output metadata and generated declarations to it. The standard JavaScript tool
test compares update_plan, view_image and image-generation contracts too.

The base Astra/Sol/Terra/Luna instructions now match their upstream model templates
exactly, including Astra identity and whitespace. The canonical prompt inventory
contains 102 assets backed by 255 pinned source files; availability in the registry
does not mean every optional runtime mode is wired to consume it. Native bundled
permissions, managed goal continuation, and Realtime voice templates are wired.
The apply_patch grammar and image-generation description are byte exact.
Exec's grammar, helper preamble, wait description/schema, and MCP TypeScript
preamble are checked. The schema renderer is a direct upstream port, checked for
source equality after only import/module-path adaptation; this fixes references,
compositions, tuples, boolean schemas, and bounded recursive expansion.

Additional fixes: shell approval metadata now decodes the advertised argument
names; JavaScript update_plan exposes upstream metadata and returns `Plan updated`
with an empty structured result; image-generation metadata now matches upstream;
JS image defaults/detail precedence, generatedImage validation, empty notify
validation and exec pragma empty/null handling match the tested upstream cases.

## QuickJS is the required engine

The user explicitly retained QuickJS on 2026-09-18. Do not replace it with V8.
The production Code Mode runtime remains QuickJS; the standalone comparison
script uses V8 only to execute upstream helper code and generate test evidence.
The description says `fresh JavaScript context` to avoid falsely advertising V8.
Prompts, schemas and observable host behavior are still parity targets.

## Code Mode behavior verified against upstream

`python3 scripts/codex-parity/native-behavior.py /path/to/pinned/codex` executes
21 upstream V8 helper cases, seven upstream Rust wait argument cases and 15
upstream Rust truncation cases. `native-behavior.json` records the results.
The JS `code-mode-upstream.test.mjs` runs all 21 helper cases through the native
JS evaluator, QuickJS, and the worker evaluator, and checks all 15 truncations.
The focused JS Code Mode and evaluator suite contains 90 passing tests.

Native and JS now use the same primitive/JSON serialization, image and audio
normalization rules, default image detail, generated-image hints, per-wait
budgets, zero budgets, u64 argument acceptance, yield grace and completion versus
termination rules. Session storage uses JSON snapshots and per-cell writes.
Normal root completion discards unawaited work, so callers must await tool calls
whose completion matters. Malformed data URI decoding belongs to later image
preparation, as in upstream, instead of adding stricter helper-only validation.

## Remaining work; not a claim of complete equivalence

- `notify` is out of band in the JS host observer. The WASM bridge retains those
  notifications for the model, but the agent loop still inserts them on return
  from an observation. Immediate upstream-style injection into an active model
  turn is not yet implemented on native or WASM.
- Native audio duration decoding uses the upstream decoder behavior. The JS
  helper currently estimates PCM WAV duration; additional audio formats need
  the same decoder coverage to claim full audio budget equivalence.
- QuickJS engine resource limits and JavaScript engine edge cases remain
  distinct from V8. The focused corpus is evidence of compatibility for its
  tested cases, not a proof of arbitrary JavaScript equivalence.
- Embedded applications can supply their own evaluator. Its execution globals
  and capabilities still require a separate review; the current host description
  explains this instead of advertising upstream isolation it cannot guarantee.
- The canonical memories adapter is wired on native and managed paths; see
  [its storage and API notes](codex-memory-api.md). Custom `computer` registration
  is removed; [CUA provider notes](computer/cua-provider-contract.md) distinguish
  preserved MCP contracts from compatibility of the external provider itself.
- Selected-hand shell behavior, remaining context/permission/clock/MCP-resource
  handlers and optional prompt-mode dispatch still need integration. See the
  [tool inventory](codex-tool-inventory.md). A catalog declaration is not evidence
  that its handler exists.

This work is on the integration branch; these checks are not a deployment receipt.

## Standalone web follow-up

The native and JavaScript web schemas and descriptions now come from the pinned
upstream source without host substitutions. `scripts/codex-parity/web.py` compiles
the upstream command schema and verifies the Rust wire types and both public
contracts. PDF screenshot operations and optional `sports.tool` are preserved.
Both adapters accept empty commands, ignore unknown object fields, reject invalid
operation types, and leave query limits to the provider, matching Codex parsing.
The native adapter sends the complete operation batch as one request. The platform
gateway preserves the caller's model, optional history, and output budget.

Validation: eight native web tests, sixteen JavaScript tool tests, and two gateway
request tests pass. This is contract and request parity; live provider capabilities,
full JavaScript history propagation, and the other gaps above still require their
own integration evidence before claiming end-to-end parity.
