# Actual CLI verification — PR #436, 2026-09-21

Historical verification record: these runs found failures in live object-valued child results and idle reconstruction. Both are fixed and pass the subsequent [native and managed CLI recovery verification](THREAD_ROUTING_CHILD_RECOVERY_2026_09_21.md). The original observations below are preserved.

Both native executables were built from PR head `96e49f104357207d9cdf4dc25910af9498a27e0b` in the isolated worktree. These runs use the actual `nanocodex` and `nanocodex2` binaries, not SDK substitutes. Live inference used the authenticated Workers AI binding; task data was synthetic. Both binaries were rebuilt and native repair/restart was repeated after the source fixes below. No installed executable, saved login, or production deployment was changed.

## Native nanocodex

The native CLI has no embedded Jev admission flag. The harness called the PR's `resolveThreadRoute` once, persisted its result with `ThreadRoutePin`, and launched the real CLI with that model/thinking choice. A loopback HTTP server forwarded Responses requests through the PR's unmodified Workers AI adapter to live GLM. This proves the native agent/tool/transport path with an externally composed route; it does not prove a standalone native routing UX.

Jev proposed GLM-5.3 low. Effective confidence was 0.42, so the policy retained it as a low-confidence proposal, not a calibrated success guarantee. All six final-run native model requests matched the pin and sent full history without `previous_response_id`.

| Journey | Result | CLI elapsed | Model requests |
| --- | --- | ---: | ---: |
| Read a service and policy file; apply retry settings while preserving name and port | pass | 15.368 s | 5 |
| Exit, start a second CLI process against the same SQLite state, recall the prior receipt and original retry count without rereading files | pass | 2.175 s | 1 |

The final-code rerun made four actual Code Mode calls containing two `exec_command` and two `apply_patch` calls. One patch used incorrect whitespace, was rejected, and the model corrected it before completing the task. The earlier run also passed (9.632 s/three requests for repair; 1.650 s/one request for restart). Independent file inspection found attempts=3, backoff_ms=250, name=`fixture`, port=8080. The second process answered receipt=`ORCHID-731`, old_attempts=9. Both exited zero with `run.completed`; restart used no tools and made no additional classifier request. Jev admission itself took 1.079 s in the final-code rerun, outside the first CLI elapsed time.

The CLI used its default system instructions with optional browser, computer, MCP, web search, image generation, memory and subagents disabled for this isolated file task. It ran HTTPS with provider storage disabled, `--local-durability` and a fresh `--request-id` per process.

## Managed nanocodex2

The actual client ran against a locally bundled PR Worker in Miniflare/workerd with real SQLite Durable Objects, Rust/WASM agent execution and Just Bash tools. Account identity and ancillary account egress were local synthetic fixtures; Jev and GLM responses were live. The agent was admitted through `POST /v1/agents`, then two separate `nanocodex2 run --agent ID` processes submitted work. The CLI currently has no `model_routing` creation flag.

This controlled managed smoke restricted eligible candidates to GLM low/medium. It verifies admission, transport, tool execution and persistence, not unrestricted cross-provider chooser quality.

| Journey | Result | CLI elapsed | Model requests |
| --- | --- | ---: | ---: |
| Copy a synthetic input file to result.txt and read it | pass | 4.214 s | 2 |
| Start another CLI process, read result.txt and append CONTINUED to the answer | pass | 3.355 s | 2 |

Both terminal events reported GLM-5.3 low. Real `exec_command` results returned exit_code=0 and `cli-route-proof-7391\n`; the final assistant messages were exactly `cli-route-proof-7391` and `cli-route-proof-7391 CONTINUED`. The route stored in SQLite was identical after both turns. Inference counters showed one Jev request and four GLM requests. Before the first task was admitted, local Hand attachment received 503 while routing awaited its opening prompt; the existing client retry recovered to 101 after the route was pinned. Neither completed turn failed or retried a model request.

## Integration defect found and fixed

The first public creation request with both `model_routing` and enabled `multi_agent` returned HTTP 400: `thread routing PoC requires multi_agent disabled`. This was a stale admission restriction left over from the root-only implementation, so the previously implemented child router was unreachable through normal creation. The follow-up removes that restriction while retaining the settings ownership check and bounded positive child-limit validation. A regression now exercises routed multi-agent creation through the public request parser.

The next live attempt reached `spawn_agent`, but GLM repeatedly encoded `output_schema` as a string. The published tool declaration had only a description for this field, while the runtime requires an object or boolean JSON Schema. After ten GLM requests and no child admission, the test was cancelled; this failed attempt is retained. The follow-up advertises the actual object/boolean contract, explicitly rejects JSON-encoded strings in the description, and adds a schema-validation regression. It does not silently coerce model arguments.

A subsequent attempt successfully spawned and routed the child and read the fixture, then exposed the same omission on `submit_result.output`. The child repeatedly returned JSON-encoded text instead of its required object. That cancelled run consumed two Jev and twelve GLM requests and is also retained. The final follow-up explicitly declares all six JSON value kinds for result submission, preserves legitimate primitive results, and clarifies that objects/arrays must be passed directly. Runtime schema validation remains authoritative. A further live retry still failed object submission and was cancelled after two invalid result calls. The type/description improvement is not evidence that GLM reliably follows object-result contracts; this remains a failing live case.

The routing suite passes 96 tests after the admission change, all 13 hosted admission/authorization tests pass, all 59 native subagent tests pass with the tool-contract fix, and the managed TypeScript check passes.

## Remaining live child failures

A separate string-result child completed its first task in 13.776 s: the parent spawned one child, Jev chose its independent GLM-low route, the child ran the real file-read tool, `submit_result` accepted `CHILD-ORCHID-4821`, and `wait_agent` returned that exact value. The parent answered `CHILD_OK:CHILD-ORCHID-4821`. The initial verifier incorrectly demanded a space after the colon; inspection corrected the verifier without repeating inference or changing the returned result.

The next CLI process, after the Worker logged `idle_shutdown` and then `agent_constructed`, could not address the prior child. `send_agent_message` returned `unknown agent_id 1`, the directory was empty, and the persisted child route count had fallen from one to zero. The model correctly refused to spawn a replacement or read the file itself. That turn exited zero but **failed the task assertion** in 10.747 s. Root and child admission together used two Jev calls; both turns used ten GLM requests. No new Jev decision was made during the failed continuation.

The existing mock-provider WASM regression for continuation within a live runtime still passes. It does not establish continuation after managed idle shutdown. The runtime currently closes/releases children on shutdown, and its reconstruction support restores tombstones rather than resumable child harnesses. Persistent child reconstruction needs further implementation and a passing live regression before claiming that guarantee.

The object-result task is also still failing: GLM serializes its structured result as a string even with explicit JSON types and instructions. No result coercion was added and no failed object task was relabeled as a string-task success.

All local test servers were stopped. Managed inference across successful and failed attempts totaled eight Jev and 48 GLM requests. The two native repair/restart runs totaled two Jev and ten GLM requests. These include development retries and are not performance or reliability estimates.

## Build and lint

Build command:

```sh
cargo build --locked -p nanocodex-bin --bin nanocodex -p nanocodex2-bin --bin nanocodex2
```

CI on the starting commit rejected a nested route-bind error check with `clippy::collapsible_if`. The follow-up combines the conditions into a let-chain while preserving child shutdown and error propagation. Focused validation passes:

```sh
cargo clippy --package nanocodex-subagents --all-targets --all-features -- -D warnings -A clippy::missing_const_for_fn
```

These bounded development smokes do not measure comparative model quality, throughput, gateway generation, interactive TUI behavior or production account integration.
