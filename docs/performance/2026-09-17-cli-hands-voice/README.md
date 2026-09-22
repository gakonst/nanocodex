# Managed CLI, connectors, Hands, and native voice — 17 September 2026 UTC

The published baseline is `adc1faa203d24e27191c766d7b04d878406ee206`.
Ordinary CLI, connector, and Hand tests used its verified macOS nightly artifact
against the production account service. Voice tests used a local development
build with the additions in this change and the matching nightly native helper.
The published baseline itself does **not** include `nanocodex2` voice support.

## Measured results

| Journey | Samples | Observed time |
|---|---:|---:|
| Fresh CLI conversation, first assistant text | 3 | median 7.25 s; range 6.09–8.94 s |
| Resumed CLI conversation, first assistant text | 3 | median 3.86 s; range 3.57–4.42 s |
| Fresh CLI conversation, complete process | 3 | median 7.76 s |
| Resumed CLI conversation, complete process | 3 | median 4.10 s |
| CLI state / retained history | 3 each | median 400 / 368 ms |
| GitHub authenticated profile tool | 3 | median 503 ms; range 498–715 ms |
| X authenticated profile tool | 3 | median 432 ms; range 415–573 ms |
| Spotify authenticated profile tool | 3 | median 302 ms; range 285–331 ms |
| SoundCloud authenticated profile tool | 3 | median 296 ms; range 279–318 ms |
| Linux native shell | 1 | 431 ms |
| Omarchy native shell | 1 | 477 ms |
| Windows native shell, successful cmd.exe retry | 1 | 535 ms |
| Local Mac file write/read, successful final call | 1 | 122 ms |
| Linux VM provisioning / guest shell | 1 | 1.75 s / 272 ms |
| Omarchy VM provisioning / guest shell | 1 | 9.36 s / 293 ms |
| Cloudflare sandbox provisioning / guest shell | 1 | 6.25 s / 770 ms |
| Mac VM provisioning / guest shell | 1 | 2.05 s / 279 ms |
| Hand catalog HTTP read | 3 | median 424 ms |
| Screen catalog HTTP read | 3 | median 424 ms |

All 12 connector reads returned HTTP 200 through the regular agent tool path.
Full connector conversations took roughly 12–16 seconds, most of which was
model work. Connector metadata discovery was 231 ms; environment calls were
570–692 ms. Nested `exec` wrapper durations are not added to their child calls.

Fresh/resumed runs used the CLI defaults: Astra, low effort, standard mode.
A repeated idempotency key did not add a turn (replay completed in 1.62 s).
Ctrl+C during a remote command left no active turn; the next prompt succeeded.
The local workspace file was checked independently on disk.

Each VM printed `VM_E2E_OK` and `Linux`. Deleting the scratch agent released
its mounts; deletion succeeded, and the subsequent live Hand/screen inventory
contained none of the test resources. Existing VMs were preserved.

## Voice implementation and runtime evidence

`nanocodex2 voice` and TUI `/voice` now use the shared managed voice protocol
and native media helper. The TUI exposes start, stop, mute, unmute, and status;
conversation changes close the previous call. Typed input suppresses outdated
spoken results. Authentication stays in the managed client. Audio stops before
bounded remote transcript/session cleanup. Agent work is not cancelled merely
because voice stops.

Media negotiation, agent admission, and initial state loading run concurrently.
A bounded agent-event reader keeps its HTTP connection attempt alive while
realtime audio/control events arrive. Polling the SSE connection directly inside
`select!` initially starved it, so a real spoken request executed on Omarchy but
its answer never reached voice. The independent reader fixed that return path;
a regression test simulates frequent realtime events during a delayed SSE
handshake. Voice history uses the shared transcript projection rather than
showing internal handoff XML.

Two subsequent prerecorded requests completed the full path: native input →
recognition → managed delegation → Omarchy shell → model answer → native voice.
The final run returned “It’s Omarchy 4.0.4, based on Arch Linux.” Its timings,
relative to voice-session startup after thread attachment, were:

| Stage | Elapsed |
|---|---:|
| Native offer | 364 ms |
| Provider SDP answer | 1,984 ms |
| Native peer/devices connected | 3,191 ms |
| Control channel and managed session ready | 4,478 ms |
| Delegation received / admitted | 11,449 / 11,697 ms |
| Final answer sent to speech | 19,004 ms |
| Native speaker energy for that answer | 19,980 ms |

The final-answer-to-speaker interval was **976 ms**, sampled by the native audio
peak meter at 100 ms intervals. This is not mouth-to-ear latency: recognition,
model work, tools, and buffering precede the final answer. Earlier greeting audio
is excluded from that interval. Warm startup after attachment was 3.53–4.48 s;
the first cold call took 9.03 s. These small cohorts do not establish a reliable
p95 or a fleet-wide speedup.

The live TUI passed typed input, `/voice`, mute, unmute, stop, and typed input
afterward. Each audio test selected BlackHole as its input, injected a known
speech fixture, and restored the original default input afterward. Native
speaker energy plus matching response transcripts were checked; a human did
not independently judge physical playback quality.

## Failures and limits retained in the results

- An explicit physical Mac path was rejected by logical namespace routing.
  Resolving the attached workspace via `environment` succeeded; macOS also
  canonicalized `/var` to `/private/var`.
- Windows initially received PowerShell syntax despite using cmd.exe. Its retry
  succeeded. Exposing native shell identity more clearly would avoid this retry.
- There is no standalone agent unmount tool. Initial create-and-unmount prompts
  therefore declined to provision. The successful tests used scratch-agent
  deletion, whose lifecycle owns mount cleanup.
- The initial short “ready” phrase was not correctly recognized. Longer spoken
  Hand requests succeeded, although proper names were imperfectly transcribed.
- A muted explicit-speech experiment produced no audio; that experimental CLI
  option was removed. Connection success alone is not a voice E2E pass.
- No browser was connected to this session. Screen discovery was measured, but
  remote video FPS/input latency was not remeasured. The earlier cross-platform
  screen benchmark remains the source for those results.
- Physical microphone, phone, cellular handoff, and Windows/Linux native audio
  devices were not exercised. Local CPU load was elevated during compilation.
- Worker measurements with a zero-duration clock value are not interpreted as
  zero-cost execution. Remote process time and tool duration are different
  boundaries; the difference is not presented as pure network latency.

## Verification and evidence

- 521 CLI unit tests passed, including voice controls, non-starving event reads,
  and shared transcript display.
- The managed crate passed 33 unit tests, 3 public lifecycle tests, and 3 voice
  transport tests. The voice tests cover account authority, durable retry identity,
  mismatched receipts, invalid paths, media negotiation, sideband events, and
  oversized outbound frames.
- Clippy reports existing `missing_const_for_fn` warnings in the unchanged voice
  protocol dependency; new managed transport and CLI warnings were addressed.
- [Sanitized measurements](measurements.json) retain samples and failures.
  Raw JSONL traces, owned scratch-agent receipts, audio fixtures, and TUI captures
  are local under `output/e2e-20260917/`. Raw connector profile payloads are not
  copied into this report.

Next priorities are standalone mount cleanup, clearer physical/logical workspace
and Windows shell guidance, lower provider/control-channel startup latency, and
fresh phone/browser recovery measurements. Native voice now has a measured
working managed-agent path to build on.
