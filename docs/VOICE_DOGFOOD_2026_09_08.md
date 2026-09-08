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
  and repeated greeting checks passed.
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
but its requested digest answer also missed the 120 s test deadline. These are
not passing end-to-end journeys. The Hand task similarly missed its live reply
window. Investigation must retain these failures rather than hide them with
larger test timeouts.

Managed voice currently waits for completed assistant messages because the
managed watcher suppresses assistant deltas. Streaming partial model replies
requires a deliberate live-event/replay contract. Stop-time transcript persistence
can also start another agent turn; neither behavior was changed by this fix.
