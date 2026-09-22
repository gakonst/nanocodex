# Voice E2E — September 16, 2026

## Result

**Real prerecorded speech was sent through WebRTC and recorded responses were decoded. Voice is not an unconditional pass.** Across seven browser calls, six produced received audio energy; all seven recognized the input request, but one output caption was wrong. The original silent warm call remains a failure. A separate native receive-only test connected successfully but its explicit text-to-speech/context command produced no audio within 15 seconds.

The final three-call audio cohort reached media readiness in **2,278 /1,272 /1,614 ms**. Its first received audio arrived **1,035 /1,464 /1,256 ms after the scheduled input-audio end**. One warm call accepted speech **869 ms before durable admission completed**, confirming that listening can proceed independently of agent preparation.

This is transport/library evidence, not a physical microphone, speaker, installed-app tap, or iPhone UI measurement. No production source, deployment, installed app, real user session, or microphone permission was changed.

## Actual audio evidence

Input: [“Please say the word ready.”](voice-audio/input-please-say-ready.wav), a 1,251.917 ms WAV injected into the outgoing WebRTC audio track. The provider's input transcripts confirm the words arrived. The final cohort starts injection immediately on the client's media-ready callback, after decoding the fixture.

| Final cohort | Media ready | First output caption after scheduled speech end | First received audio after scheduled speech end | Output caption | Recorded response |
| --- | ---: | ---: | ---: | --- | --- |
| Fresh agent/call |2,278 ms|845 ms|1,035 ms|`Ready.`|[Listen](voice-audio/response-1.wav), [full capture](voice-audio/response-1-full.webm) |
| Warm restart1 |1,272 ms|1,183 ms|1,464 ms|`eight.`|[Listen](voice-audio/response-2.wav), [full capture](voice-audio/response-2-full.webm) |
| Warm restart2 |1,614 ms|816 ms|1,256 ms|`Ready.`|[Listen](voice-audio/response-3.wav), [full capture](voice-audio/response-3-full.webm) |

All three full WebM/Opus recordings decoded successfully into 24 kHz mono PCM. Peaks were 0.24–0.40 of full scale, with 340–380 ms of 20 ms windows above RMS 0.003. Listening clips retain 200 ms before and 350 ms after those voiced windows; full captures preserve the timing context. [Audio verification](voice-audio-verification.json).

The second call's **output caption** is incorrect despite its correct input transcript. The recording has valid nonzero audio, but its spoken semantic content was not independently transcribed or listened to by this automated run. Do not convert that caption mismatch into a claim that the recorded word was independently verified as “eight.”

### Clock boundaries

The final cohort records `performance.now()` immediately after decode and before `AudioBufferSourceNode.start`, plus the AudioContext scheduled start/end and actual `onended` callback. First received energy is polled every 20 ms. The callback-based end→audio observations are 1,038 /1,467 /1,259 ms, within 4 ms of scheduled-end values. Audio render quanta and polling still limit precision; these are not sample-exact acoustic measurements.

Output transcript timestamps are absolute offsets from session start in the raw evidence. The table subtracts both speech-start offset and input duration. No speech-relative/audio value is compared directly with an absolute transcript offset.

## Original four calls — retained, including failure

These calls waited for the complete start promise before injecting speech; media readiness is recorded separately. Their speech clock began before asynchronous fixture decoding, so speech-end-relative values below are **approximate**. They must not be silently combined with the improved clock boundary.

| Call | Media ready | First caption after approximate speech end | First audio after approximate speech end | Outcome |
| --- | ---: | ---: | ---: | --- |
| Fresh agent1 |2,788 ms|1,216 ms|1,365 ms|`Ready.` +audio |
| Warm restart1 |1,629 ms|954 ms|1,136 ms|`Ready.` +audio |
| Warm restart2 |1,626 ms|2,772 ms|No energy within 15s|`Ready.` caption;5,441 inbound bytes, zero reported decoded energy |
| Fresh agent2 |2,165 ms|698 ms|1,129 ms|`Ready.` +audio |

The silent call recognized the spoken input and produced the expected caption, while sending 27,409 outgoing bytes. It was not a failure to submit the fixture. Provider silence, decoding, and the original harness remain possible explanations; that original call did not record remote audio or detailed receiver track state. The later recording cohort passed audio reception, but does **not** erase the failure or isolate its cause.

## Native Apple client

Existing `VoiceIntegrationTests` and `VoiceLatencyTests` used the production Apple `VoiceSession`/WebRTC implementation, compiled in the test configuration. They did not start microphone capture. Native readiness uses `ContinuousClock`; diagnostic stage differences below use the client's wall-clock log and are labeled accordingly.

| Native sample | Observed media ready | Client calls HTTP duration | Managed calls handler | Approx. call response→media-ready |
| --- | ---: | ---: | ---: | ---: |
| Fresh integration (`spruce`) |5,215 ms|4,303 ms|4,077 ms|711 ms|
| Latency agent, call1 (`cove`) |4,218 ms|2,909 ms|2,790 ms|1,297 ms|
| Same agent, call2 |2,653 ms|1,265 ms|1,029 ms|1,373 ms|
| Same agent, call3 |1,864 ms|1,200 ms|837 ms|643 ms|

The managed requests are tied to the exact scratch agent paths; repeated calls within each agent are matched by request order. Server request IDs and native stages are retained in [measurements](voice-measurements.json). Native HTTP metrics identify request wait/connection reuse; the native test does not capture the voice-session ID needed for the same full inner-relay correlation as the browser.

- All four native connections reached provider `session.started`; control updates received acknowledgements. The three-call latency test passed. Stop released the native peer, and the early-start cancellation case did not revive the stopped call.
- **Native explicit-speech test failed:** after appending context, `speak("Voice is ready.")` sent `session.context.append` on the `speakable` channel. The provider acknowledged `session.context.appended`, but no output transcript or received audio bytes appeared before the 15 s deadline. The printed 15.47 s timer is a timeout observation, **not** first-audio latency.
- This native fixture is a text/context command, not spoken microphone input. It does not establish that native spoken-input replies fail. Native prerecorded PCM injection has no existing test hook; adding a new capture mechanism was outside this measurement-only pass.

## Where startup time went

All **seven browser calls** have exact request/session-ID correlation across managed, egress and inner relay logs. [Component events and additive decompositions](voice-components.json) preserve the evidence. These components describe **connection startup**, not speech-response inference.

| Component | Median | Observed range | Meaning |
| --- | ---: | ---: | --- |
| Provider create-call response wait |513 ms|338–550 ms|After request upload, before upstream response headers |
| SDP response→media-ready |472 ms|388–1,336 ms|Peer/control/provider readiness on client |
| Credential broker round trip |176 ms|165–190 ms|Egress credential RPC; inner method timers reported 0 ms |
| Relay container boundary residual |122 ms|119–227 ms|Relay total minus inner fetch; not proven activation time |
| Client/frontdoor residual |66 ms|52–105 ms|Client call span minus managed named stages |
| Relay socket preparation |41 ms|16–293 ms|Before request send; socket was not reused in these samples |
| Request upload |32 ms|2–96 ms|Request send→upload completion |
| Ownership RPC |23 ms|9–40 ms|Managed ownership check |
| Managed→egress residual |6 ms|4–370 ms|Variable boundary remainder |
| Client work before call request |8 ms|5–525 ms|Warm calls~5–8 ms; fresh contexts 360–525 ms |

Every observed relay reported **already running**. These are not cold-container measurements. The long fresh-client intervals coincide with20 ms timer gaps of 356–521 ms. The original cohort started under a 1-minute Mac load average of 313; the final cohort began around 39. The workstation was shared, and no unrelated processes were stopped. Do not present the later cohort as an optimization experiment.

All browser call authentication used the existing short-lived access snapshot and reported 0 ms. Full SQL auditing was **disabled**: the deployed `NANOCODEX_PERFORMANCE_TRACE` binding was absent. The tail retained `managed.performance` aggregate read counts where emitted, but this run cannot supply complete SQL statement/row counts or attribute the broker's 176 ms round trip to SQL. Worker synchronous timers reporting0 ms are not proof of zero CPU time.

Close promises took 0.92–3.46s in browser calls, including durable cleanup; this is separate from local media cessation. All five scratch agents returned 404 after deletion. That verifies API invisibility, not completion of every deferred storage cleanup alarm.

## Provenance and limits

- Integration source: `68a7f4edc39dfdd35388270160c0ecf935848658`.
- Managed Worker: `e1877ce4-0170-4de6-9cf9-ccfc003b7ece`.
- Egress: `d8f1b3e5-2ae9-439b-a587-cd39224eeb92`.
- Account/relay: `83d1e7ce-0db2-4637-9de8-8e9fd5f6968f`.
- Deployment receipts were unchanged across both voice windows. Existing WASM/native-core artifact hashes and the WASM build's original dirty-source attestation are recorded explicitly; this run did not rebuild those artifacts or claim a pristine new WASM build.
- [Manifest, deployment receipts, cleanup and SQL setting](voice-manifest.json); [measurements](voice-measurements.json); [server components](voice-components.json); [audio verification](voice-audio-verification.json).

No native Codex voice reference was rerun in these windows. No p95/p99, controlled cold-container, barge-in, physical microphone, mobile UI, or overall voice-health sign-off is supported. Raw sanitized tails and reusable harnesses remain under `output/system-e2e-20260916/voice/` ; collection is now stopped.
