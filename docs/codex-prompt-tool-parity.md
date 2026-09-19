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

The base Astra/Sol/Terra/Luna instructions match their upstream model templates.
Astra retains the existing Nanocodex identity substitutions and stripped trailing
spaces. The apply_patch grammar and image-generation description are byte exact.
Exec's grammar, helper preamble, wait description/schema, and MCP TypeScript
preamble are checked. The schema renderer is a direct upstream port, checked for
source equality after only import/module-path adaptation; this fixes references,
compositions, tuples, boolean schemas, and bounded recursive expansion.

Additional fixes: shell approval metadata now decodes the advertised argument
names; JavaScript update_plan exposes upstream metadata and returns `Plan updated`
with an empty structured result; image-generation metadata now matches upstream;
JS image defaults/detail precedence, generatedImage validation, empty notify
validation and exec pragma empty/null handling match the tested upstream cases.

## Intentional host differences

- Native Code Mode uses QuickJS rather than V8, so its first sentence says
  `fresh JavaScript context`. Embedded runtime rewrites the evaluator and lifetime
  sentences (`crates/nanocodex-tools/src/embedded/runtime.rs`): the host owns nested
  tool work until it finishes or is cancelled. Upstream stops remaining tasks when
  the root promise completes. This ownership behavior is covered by existing JS
  runtime tests and was retained.
- Provider summaries and deferred metadata may add host tools to the exec prompt.
  Namespace headers are inferred from local `__` names, rather than using
  upstream namespace descriptions. Local MCP preamble inclusion also accounts
  for deferred tools whose full schemas are not in the visible definition list.
- JS workspace shell descriptions in `tools/execution-contract.mjs`, `bash.mjs`,
  and the browser/host adapters describe selected hands, bounded Bash, and host
  approval capabilities. Their parameter keys agree with the shared contract;
  their descriptions and operating effects are deliberately not upstream PTY
  promises. The Rust native standard contracts are compared exactly.
- Web guidance has three explicit substitutions, checked by the script: no PDF
  screenshot command, image search returns source pages/captions, and response
  length describes host text limits. Web's host adapter is not upstream's full
  web capability set. Image-generation and web provider execution/authentication
  are host implementations; schema parity does not imply provider parity.
- Image helpers retain stricter data URL/base64 validation. Hosted QuickJS retains
  resource limits and serialized evaluation. JS timers retain the platform delay
  cap. Runtime environment, permission, memory, connector, goal and skill prompts
  are host additions, not copies of Codex CLI prompt layers. Upstream-only tools
  and modes are not introduced by this change.

## Remaining semantic gaps (not intentional-parity claims)

These were audited but not changed in this bounded contract/helper patch:

| Area | Local evidence | Upstream evidence / difference |
| --- | --- | --- |
| Storage | `js/nanocodex-tools/runtime/code-runtime.mjs` shared session Map and structuredClone; hosted QuickJS store forwarding | `code-mode-runtime/src/runtime/callbacks.rs` and `cell_actor/mod.rs`: JSON snapshot/write set, key coercion, commit only when completion wins. Local JS writes can survive termination and retain non-JSON values. |
| Wait budgets | Native `code_mode/mod.rs` clamps budgets to at least one, inherits exec budget for omitted wait budget, ignores terminate budget; JS inherits exec budget | `core/src/tools/code_mode/wait_handler.rs`, `code_mode/mod.rs`: fresh default per wait, zero permitted, termination budget honored. |
| Wait validation | Local native denies unknown fields and accepts null optional yield; JS requires safe integers and rejects null wait numeric fields | Upstream permits unknown fields, accepts optional max_tokens null, rejects yield_time_ms null, supports u64 range. |
| Text serialization | JS host stringify plus hosted QuickJS guest stringify | Upstream `runtime/value.rs` uses primitive string conversion and propagates JSON exceptions; local JS NaN/Infinity/cycles/functions differ. |
| Notifications | Native buffers until observation; JS wakes observer (direct execution becomes text) | Upstream `cell_actor/callbacks.rs` delegates immediate out-of-band notify injection. |
| Completion race | JS wait terminate overwrites completed-but-unobserved status | Upstream and local native arbitrate completion versus termination; a completed cell wins. |
| Output budget/format | JS truncates individual text chunks; native `code_mode/output.rs` drops audio in mixed output | Upstream merges text-only output and has multimodal/audio token accounting. |
| Yield grace | JS exact requested timer | Native and upstream add 1 second grace for yields of at least 10 seconds. |

Future runtime changes need coordinated native, JS-host and QuickJS guest tests;
changing only the prompt would misstate the implementation. Native runtime tests
already cover wait, cancellation, output deltas, and image semantics. The new
`js/nanocodex/test/code-mode-parity.test.mjs` adds 27 parser/helper regression cases.

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
