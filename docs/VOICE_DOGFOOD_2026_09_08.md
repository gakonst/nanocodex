# Voice and Apple app verification — September 8–9, 2026

The first-response regression was introduced by `9da9eb1e`: every first utterance
required a main-agent memory lookup, and the client suppressed voice transcripts
and playback until that agent returned. `3df71719` additionally prefetched from
partial speech. A greeting could finish generating while playback was suppressed
and never be spoken after the result arrived.

`fe08f392` removes synthetic first-turn delegation, speculative memory prefetch,
and blanket playback gating. Like local stock codex-rs `ac192cd793`, genuine
provider handoffs trigger agent work. Greetings and self-contained answers can be
immediate. Missing personal facts require a brief checking acknowledgment and a
main-agent memory/history lookup before making factual claims; missing results
must be acknowledged. No invented profile or startup memory fetch was added.

## Physical iPhone 17 Pro comparison

Same recorded input, “Hi, say hello briefly,” same physical phone and speaker
fixture. All timestamps below are UTC on September 8. Audio means the first
positive WebRTC output-energy observation while playback is enabled, sampled
roughly every 200 ms; this is not an external acoustic recording.

| Stage | Before | After, run 1 | After, run 2 |
|---|---|---|---|
| Call tap | 20:51:49.114 | 20:55:51.233 | 21:05:30.997 |
| Voice ready | 20:51:53.203 | 20:55:54.200 | 21:05:34.825 |
| Startup duration | 4.088 s | 2.967 s | 3.828 s |
| Recorded speech ended | 20:51:58.228 | 20:55:59.355 | 21:05:40.152 |
| First input transcript | 20:51:58.237 | 20:55:59.227 | 21:05:40.031 |
| Main-agent dispatch | 20:51:59.900 | Not needed | Not needed |
| Dispatch acknowledgment | 20:52:00.168 | Not needed | Not needed |
| Main-agent model call | 20:52:00.283 | Not needed | Not needed |
| Main-agent answer | 20:52:05.008 | Not needed | Not needed |
| Voice response transcript after input | None within 30 s | 20:55:59.915 | 21:05:41.125 |
| Voice response audio after input | None within 30 s | 20:56:00.163 | 21:05:41.527 |
| Speech end to response audio | More than 30 s | **0.808 s** | **1.376 s** |

A third phone repeat after the terminal-failure fix also passed: call tap
21:18:20.507 UTC, ready 21:18:25.169 (4.662 s startup), fixture end
21:18:30.588, first input transcript 21:18:30.603, response transcript
21:18:31.899, and response audio 21:18:32.104 (**1.516 s after speech ended**).
No main-agent greeting delegation occurred. Three after samples therefore span
0.808–1.516 s; they are not a percentile benchmark.

Startup varies independently; these samples do not establish a startup improvement.
The measured removed bottleneck is mandatory dispatch/model work plus playback
suppression. The baseline model itself took 4.725 s, and dispatch-to-answer took
5.109 s. This does not measure memory I/O in isolation.

The second after test explicitly requires a new assistant transcript after the
input fixture, preventing the spontaneous call greeting from satisfying it.
Evidence is retained locally as `/tmp/nanocodex-phone-greeting-{Before02,After01,After02}.log`
and the corresponding `*-timing.log` files and xcresult bundles.

## Native and browser grounding

The matching native greeting was silent for more than 30 s before the fix; after
it, first output was observed 1.028 s from fixture start. A personal question about
an unknown synthetic passphrase produced “Let me check,” delegated two memory tool
calls, and eventually said it did not know and could not find the passphrase.
In that successful run: acknowledgment 20:57:52.334, agent run 53.117, model call
53.159, memory tool results 57.721, final agent answer 20:58:02.087, spoken turn
complete 08.288. Prompt grounding is behavioral guidance, not a formal guarantee.

Account/browser deployment `90e29cc0`, version
`14141d15-c53b-4bb6-8845-798314ade65c`, reached 100% at 21:02:46.803 UTC.
The actual served WASM was checked: a greeting changed from two requests and no
visible transcript to one startup request and a visible transcript; a later
explicit provider handoff remained functional. WASM SHA-256:
`3777d4d336c6c8d82ac563bbf9917640c182b08d4f4602f80b8b1b6e356a17b4`.
The health endpoint's deployment SHA was stale, so attribution used deployment
metadata and the served artifact, not that endpoint.

## Focused checks and dogfood coverage

- Rust protocol/FFI: 29 passed; browser/WASM contracts: 28 passed.
- Swift voice package: 29 executed, one opt-in live skip, zero failures.
- Shared VoiceCore built for all five Apple targets.
- Native UI/fixture checks: 44 passed; desktop runtime: 33 passed.
- iPhone: live text admission, camera capture/cancel, image pick/remove/draft
  persistence/send/history, navigation menus, scheduled-job details across relaunch,
  and repeated greeting checks passed. Hand disable/relaunch/background/reconnect
  passed in 73.087 s. Stored original-video playback after relaunch passed in
  23.362 s; that history also displayed the correct digest. The earlier live
  digest-reply check still missed its 120 s deadline.
- Simulator: browser tabs, back/draft retention, last-activity ordering, queued
  steering, voice minimization, long history, setup, and scheduled-job fixtures.
- iPad: context capture/search, account isolation, and actual Safari share-extension
  persistence passed. Real incoming third-party messages were not sent.
- Physical screen-menu lifecycle verification is recorded in `REMOTE_CONTROL.md`.

## Separate failures and limits

Intermittent `/realtime/calls` requests reached the original 20 s client deadline
with no HTTP response on both native and phone; another returned HTTP 502.
The issuing layer is unproven. A diagnostic 60 s deadline returned successfully
in 3.914 s and therefore did not establish that extending the deadline helps.
Production deadlines remain unchanged.

A later personal lookup received a 202 acknowledgment but no final answer within
60 s. Existing logs recorded no nested agent events; they did not log all outer
SSE events and cannot alone distinguish dispatch failure from event-delivery
failure. A video attachment was imported, previewed, persisted, and submitted,
but its requested digest answer missed the 120 s test deadline. A later relaunch
showed the correct digest and original-video playback passed. This establishes
retained bytes and eventual completion, not timely live delivery. The Hand task
similarly missed its live reply window. Investigation must retain these failures
rather than hide them with larger test timeouts.

The retained video turn accepted at 21:01:56.350 UTC and completed durably at
21:03:03.595, **67.245 s later**, before the phone's 120 s deadline. Model runs
started at 21:01:56.398 and again at 21:02:15.550; final assistant output was
stored at 21:03:03.519. The phone still showed Idle with no answer roughly 53 s
after completion, while relaunch displayed it. This establishes missed live
updates, not simply slow model generation. The exact transport/recovery cause
is not established. Safe metadata is retained in
`/tmp/native-camera-owned-agent-metadata.json`.

Managed voice currently waits for completed assistant messages because the
managed watcher suppresses assistant deltas. Streaming partial model replies
requires a deliberate live-event/replay contract. Stop-time transcript persistence
can also start another agent turn; neither behavior was changed by this fix.

## Follow-up fixes and stage attribution

`fe0a85cb` fixes another reproduced voice failure: an outer durable `turn_failed`
before model startup was ignored by native and browser subscribers. Shared Rust
now returns one generic failure for the correlated handoff, and the clients leave
the working state. Tests cover a failure before the delegation receipt, replacement
handoffs, prior successful output, duplicates, unrelated turns, and retryable
states. Before-fix fixtures failed; after-fix checks passed: Rust/FFI 30, actual
browser/WASM contracts 29, Swift 31 executed with one live skip, package/type checks,
and all five Apple core builds. The old live stall is not retroactively attributed
to this bug because its durable trace was not retained before cleanup.

The native no-tools text check's slow reply was a separate model-stage delay:
accepted 20:29:57.648 UTC, model started 20:30:03.892, assistant output
20:34:30.752, and completion 20:34:30.789. Admission-to-model was 6.244 s;
the model-call interval was **266.897 s**. This does not distinguish provider
queueing, inference, and network time. The native Sol/high defaults were retained;
there is no controlled evidence here supporting a configuration change.

The shared Apple `ManagedClient.stream` recovery fixture reproduces a stream that
keeps heartbeating while a newer completion exists durably. The previous client
never recovered before the fixture deadline. The change checks durable state only
after 15 s without delivered cursor progress and closes a stale stream with a
retryable transport error. Consumers reconnect from their last delivered cursor;
the state snapshot cursor is never adopted. The real HTTP fixture verifies a
second request from that cursor and exactly one replayed completion, as well as
healthy idle streams, unavailable/malformed/wrong-agent state, cancellation, and
progress arriving during the state read. This validates recovery from the observed
class of missing update; it does not identify why the original server stream fell
behind.

Final recovery suites passed: InboxCore 71 executed with three live skips, Voice
31 with one live skip, zero failures. The final physical-phone text admission
check also passed without relaunching to obtain its reply.

The final native full personal-question fixture passed in 39.555 s with normal
20 s call deadlines. Startup was 2.149 s; greeting audio began 1.050 s after its
fixture started, without delegation. The personal question received a brief
checking acknowledgment, two actual memory tool calls, and a complete spoken
unknown answer. Decoded final-answer audio began 20.618 s after the personal fixture started
(**12.384 s after its 8.234 s recording ended**); the unknown-answer transcript
matcher fired at 20.870 s, and full speech finished at 26.728 s. These timings show that personal lookup still takes
main-agent time; the optimization removes that cost from ordinary conversation.
The hosted native main-window/content test passed. CUA's desktop capture failed
for unrelated apps too, so no speculative product window fix was retained.

Final personal lookup stage durations: dispatch acknowledgment 0.267 s,
acknowledgment to first model start 3.451 s, first model to tool calls 4.055 s,
two memory tools to last result 0.078 s, then second model to final message
2.821 s, and final message to decoded answer audio 0.702 s. The brief checking
audio started 1.594 s after the recording ended. This run used one healthy event
connection with no reconnect: the stale-stream recovery path was verified with
deterministic HTTP fixtures, not triggered by this live run.
