# Long-prompt latency profile — 2026-07-20

This follow-up asks whether long Nanocodex turns are limited by the Responses
API and caller-requested tools, or by local orchestration, serialization,
tracing, and scheduling. It uses the 41-task retained Harbor gate from
`2026-07-19__10-00-16-eval-35805`, a 118-second live repository audit, and a
51-second output-heavy live call. Runs used the symbolized optimized
`profiling` build on an Apple M1 Max.

Full prompts, API traffic, tool activity, local JSON tracing, and process
measurements remain under `.nanocodex/profile-single-prompt/` or the retained
Harbor job and are intentionally ignored by Git.

## 41-task retained workload

The 41 terminal runs contain 503 model calls, 892 tool calls, 544 Responses
attempts, 81,618 retained API events, and 63.1 MB of JSONL. One run terminated
with a non-retryable API policy error; its terminal timing is included. The
durations below are sums of per-run wall time, not the wall time of Harbor's
concurrent job.

| Phase | Sum | Share of run time | Per-run p50 | Per-run p95 | Maximum |
| --- | ---: | ---: | ---: | ---: | ---: |
| Model generation | 2,321.397 s | 56.518% | 43.477 s | 139.254 s | 194.436 s |
| Caller tool wall time | 1,753.458 s | 42.691% | 3.425 s | 104.522 s | 1,042.947 s |
| Warmup | 26.915 s | 0.655% | 0.591 s | 0.791 s | 2.484 s |
| Unattributed local remainder | 5.585 s | 0.136% | 20 ms | 214 ms | 3.662 s |
| Total | 4,107.355 s | 100% | 48.464 s | 230.178 s | 1,241.761 s |

The longest run was `compile-compcert`: 1,241.8 seconds total, of which
1,042.9 seconds was tool wall time and 194.4 seconds was model time. This is
real work performed by compilers/tests requested through tools, not hidden SDK
latency.

Across the 503 calls, model duration was 3.005 seconds p50, 8.649 seconds p90,
14.647 seconds p95, and 62.885 seconds maximum. Time to the first protocol event
was only 100 ms p50 and 172 ms p95. Time to first output was 1.582 seconds p50
and 8.938 seconds p95, showing that most of the gap after the initial event is
server-side reasoning/sampling rather than local receipt.

The fmt subscriber's `responses.attempt` close records report 20.956 seconds
busy and 2,325.908 seconds idle across all 544 attempts: a 0.893% busy share.
This is Tokio span poll time, not a precise CPU measurement, and includes
instrumentation. It is nevertheless a useful conservative upper bound on work
performed while the response future was runnable.

## Representative inbound JSON

The retained job contains 81,074 inbound frames and 37.3 MB of raw API JSON.
Frame size was 198 bytes p50, 261 bytes p95, 11,552 bytes p99, and 27,597 bytes
maximum. The largest single retained run has 4,438 inbound frames totaling
4,258,163 bytes.

Criterion results for that largest response workload:

| Complete 4,438-frame operation | Time |
| --- | ---: |
| Validate raw JSON | 2.032 ms |
| Decode typed `ServerEvent`s | 3.953 ms |
| Encode full-fidelity public API-event payloads | 1.499 ms |
| Decode every frame as possible turn-state metadata | 3.772 ms |
| Guard metadata decoding by the wire type prefix | 15.3 us |

Raw validation plus typed decoding is intentional: the former preserves the
exact API event for the contractual event stream, while the latter drives typed
agent behavior. Together with full-fidelity event encoding, all three passes
cost about 7.5 ms per 4.26 MB on this machine.

## Instrumented live output-heavy call

A one-model-call prompt emitted 2,189 output tokens through 2,163 inbound frames
and 554,192 raw response bytes.

| Stage | Measured wall time | Share of 50.295 s generation |
| --- | ---: | ---: |
| Await inbound frames | 50,209.332 ms | 99.830% |
| Emit full-fidelity public API events | 50.534 ms | 0.100% |
| Typed event decode | 21.003 ms | 0.042% |
| Raw JSON validation | 8.008 ms | 0.016% |
| Request encode, 11,780 bytes | 0.0217 ms | 0.00004% |
| WebSocket send | 0.0658 ms | 0.00013% |

The complete `responses.attempt` span reported 99.6 ms busy and 50.2 seconds
idle, a 0.198% busy share. The full process took 51.172 seconds: 50.295 seconds
model generation, 869 ms warmup, no tools, and 8.35 ms outside those phases.
It used 230 ms user CPU and 260 ms system CPU, including process startup, TLS,
tracing, and JSONL output.

The receive-wait measurement intentionally includes time waiting for the socket
pump and Tokio to reschedule the consumer. Treating all of that as API wait
makes the conclusion conservative with respect to hidden scheduler latency.

A separate read-only repository audit took 117.746 seconds across nine model
calls and forty nested tool calls: 114.982 seconds model time (97.65%), 1.789
seconds tool wall time (1.52%), 966 ms warmup (0.82%), and 9.8 ms unattributed
local time (0.008%).

## Local tool path

The retained workload made 462 top-level Code Mode calls and 430 nested tool
calls. Of the nested calls, 317 were `exec_command`, 59 were `apply_patch`, 53
were `write_stdin`, and one was `view_image`. The long tail is requested work:
`write_stdin` was 30.005 seconds p50 because it was polling live processes,
while `exec_command` was 94.958 ms p50 and 2.416 seconds p90.

For all 317 shell calls, the difference between the complete nested-tool
duration and the shell session's own measured wall time was 0.476 ms p50,
1.631 ms p90, and 0.599 seconds in total. This includes argument decoding,
tracing, registry dispatch, result construction, and the Code Mode value
conversion. Those Rust-side layers are not a material tool bottleneck.

The retained Harbor gate predates the persistent Node-host change, so its
roughly 70--80 ms common Code Mode wrapper gap is per-cell Node startup rather
than current behavior. With a persistent prewarmed host, three live `pwd`
trials spent 8.9--9.8 ms in the shell and 11.0--12.9 ms in the complete
top-level Code Mode call: 2.1--3.2 ms of bridge overhead.

One interaction was expensive. Fifty-five Code Mode cells reached the
outer default 10-second yield while a nested tool was still pending, and all
55 required a later `wait` call. The model calls whose sole action was to issue
that wait consumed 123.684 seconds of Responses API time (1.401 seconds p50,
4.504 seconds p90), or 3.01% of total retained run time. The first ten seconds
overlap real subprocess work and are not themselves wasted; the extra model
round trip is. None of those yielded calls set the outer `@exec` pragma even
when the nested tool requested a longer wait: 13 `exec_command` calls requested
30 seconds, 41 `write_stdin` polls requested 30 seconds, and one poll requested
20 seconds.

The default outer cell now extends its deadline when those built-in shell tools
explicitly request a longer nested wait, plus a bounded five-second scheduling
and protocol grace. An explicit outer `@exec` pragma still wins. All 55 retained
nested calls completed within that derived deadline, so trace replay projects
that this removes all 55 wait-selection model calls without delaying ordinary
10-second cells or changing caller-selected outer yields.

## Avoidable work found and removed

1. The service serialized model input only to calculate a tracing byte count,
   after which it serialized the real request for the wire. That sizing pass is
   removed: the attempt records the exact byte length from the already encoded
   request allocation. At 128 KiB, one real encoding measured 65.3 us and
   adding one sizing-plus-encoding pass measured 129.7 us. The agent's separate
   full-content serialization remains because full-fidelity tracing is a
   deliberate contract.
2. Until a turn state was available, the socket decoded every inbound event as
   a possible metadata event. Compact wire events now reject known non-metadata
   type prefixes before deserialization, while unusual whitespace or field
   ordering retains the original full-decode fallback. On the largest trace
   this reduced the check from 3.772 ms to 15.3 us.
3. Generation and compaction attempts now expose request bytes, encode/send
   duration, response event/byte counts, socket wait, raw parse, public-event
   emission, and typed decode duration as structural span fields. A
   `stage="responses.pipeline.completed"` info event carries the same values so
   local compact/JSON fmt logs are sufficient without an OTLP collector.
4. Shell completion previously gave stdout and stderr drain tasks separate
   sequential two-second grace periods. A detached child holding both pipes
   open could therefore add four seconds after its shell had already exited.
   Both drains now share one deadline; the focused background-process
   regression completes in 2.13 seconds instead of waiting two full periods.
5. Every top-level Code Mode call flattened the segmented conversation and
   then deep-cloned that complete snapshot again into its background cell.
   Resumed `wait` calls repeated both copies even though waiting never reads
   history. The agent now moves one shared owned snapshot into a new cell and
   gives `wait` an empty borrowed history. With 512-byte representative items,
   the focused benchmark reduced 100 items from 16.0 to 8.2 us, 1,000 items
   from 180.0 to 92.0 us, and 10,000 items from 2.05 to 0.98 ms. This remains
   small beside API and subprocess time, but it removes linear duplicate work
   from the long-session tool boundary.
6. Four of the 61 retained nested shell results that returned a live session
   ID lost that ID from the enclosing Code Mode output because the JavaScript
   emitted only `result.output`. Code Mode now appends a resume notice when a
   live nested session is not otherwise present in cell output, while avoiding
   a duplicate notice when the script emits the complete result.
7. Nested calls that completed after an outer cell yield kept the original
   `exec` tracing-span parent but were emitted as public tool events under the
   later `wait` call and model-call index. Nested call IDs now carry their
   original exec parent through the background cell, and the agent retains the
   corresponding model-call index so tracing and JSONL lineage agree.

## Remaining boundaries

- The WebSocket pump and typed event stream use unbounded queues. This prevents
  a slow consumer from directly backpressuring model receipt, but a stalled
  consumer can grow memory without bound.
- The JSONL adapter performs synchronous `Write` and flushes every contractual
  event from an async task. The measured file/pipe runs were still API-bound,
  but a genuinely slow stdout sink can block a Tokio worker and indirectly
  delay the socket pump.
- The connection mutex is held for a complete streamed attempt. That is correct
  for one owned sequential conversation, but concurrent clones queue behind the
  active response even though Tower `poll_ready` returns ready.
- Event fidelity costs two JSON parses and one payload copy per inbound frame.
  The representative measurements are far below 1% of generation latency, so
  replacing typed/raw ownership with a riskier custom parser is not justified.

## Conclusion

For the representative retained and live workloads, Nanocodex is bottlenecked
on Responses API/model time plus subprocess work the model explicitly asked it
to run. Local unaccounted orchestration is 0.136% across the 41-task corpus and
0.016% in the output-heavy live run. Within a 50-second generation, the complete
instrumented local response pipeline is about 80 ms and the span's conservative
busy upper bound is 100 ms.

The next meaningful latency work is service/model behavior (time to useful
output and avoiding unnecessary additional model calls) and preventing a slow
application-owned output sink from starving the runtime—not switching JSON
libraries or bypassing Tower.

## Reproduction

```sh
NANOCODEX_BENCH_EVENTS=/path/to/retained/agent/events.jsonl \
  cargo bench --locked -p nanocodex-service --bench tower_responses \
  'retained_response_event_pipeline|responses_request_encoding' -- --noplot

cargo bench --locked -p nanocodex-core --bench fork_history \
  code_mode_history_snapshot -- --noplot

target/profiling/nanocodex run \
  --thinking low \
  --log-format json \
  --log-file .nanocodex/profile-single-prompt/long.trace.jsonl \
  'your prompt' \
  > .nanocodex/profile-single-prompt/long.events.jsonl

jq -c 'select(.fields.stage == "responses.pipeline.completed")' \
  .nanocodex/profile-single-prompt/long.trace.jsonl
```
