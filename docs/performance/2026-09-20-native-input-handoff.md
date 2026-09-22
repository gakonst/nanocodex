# Native WebRTC input handoff

The native `RemotePeer` receive callback previously created one main-actor task
per DataChannel packet. Outgoing native relative movement was already batched,
but an incoming absolute-pointer burst still accumulated work before host input
validation. Independent tasks also did not explicitly preserve reliable callback
order in the application handoff.

The shared Mac/iOS peer now uses a locked mailbox before scheduling its consumer.
Only adjacent, validated absolute movement in the same lease generation can
replace a queued position. The highest sequence wins on the unordered motion
channel. Reliable records (including relative displacement and button/key
transitions), invalid records, and generation changes remain barriers.

The mailbox holds at most 256 records and 256 KiB of payload; an individual
packet remains capped at 8 KiB. Overflow closes the peer through its existing
connection teardown, which revokes host input. It never silently drops a reliable
transition and continues the session. One consumer drains at most 32 records
before yielding. Close discards queued input and prevents new scheduling.

## Validation

On the native arm64 Mac, this command passed **17 tests, zero failures**:

```sh
swift test --package-path apple/NanocodexRemote \
  --filter 'RemoteInputMailboxTests|RemoteMotionBatchingTests|RemotePeerTests'
```

Five new mailbox tests cover a 20,000-sample motion flood with one scheduling
request, ordered reliable barriers, unordered hover sequence selection,
generation boundaries, invalid packets, count/byte limits, bounded drain batches,
and close. Eight existing viewer batching tests cover displacement conservation,
release ordering, revocation and reconnect fencing. Four peer tests include real
loopback WebRTC video, bidirectional channels, ordered held-button relative input,
ICE restart, and microphone lifecycle without opening a physical microphone.

This is component correctness evidence. No installed app, running game or host
service was changed. Capture timestamps, hardware encoding, ICE preparation and
adaptive media jitter buffering are unchanged. No matched latency benchmark was
run, and no end-to-end speedup or live game control claim follows from these tests.

## Host audit

Rust's session release path cleared its lease but ignored the native backend's
release result, including errors and the three-second timeout. The subsequent
acquire path could therefore grant control while native held state was unknown.
It now requires a successful native release before acknowledging cleanup or
continuing acquisition. Failure closes the session; reconnect still cannot grant
control until the backend successfully releases its state. Microphone revocation
and lease invalidation occur before waiting for native cleanup.

The focused regression uses the actual local publisher/WebSocket control path
and rejects a grant for all three failure forms: an unavailable result, a backend
error, and a timed-out release. A physical device that cannot release input is
still a host/device failure; closing the session cannot prove hardware cleanup.

A remaining architectural limit is the Linux X11 guest desktop IPC loop in
`crates/nanocodex-vm/src/desktop.rs`: observe/capture, input, release and keepalive
share its synchronous handler. The publisher schedules capture independently,
but capture or a stalled IPC client can still delay commands inside that adapter.
Separating those native connections requires its own adapter change and matched
runtime validation; this bounded patch does not claim to resolve that path.

The retained Go host already checks native release before granting control and
uses a replaceable motion slot. No Go changes or rollout were needed here.

On the same native arm64 Mac, `cargo test -p nanocodex-remote --lib -j 2`
passed **52 tests, zero failures**, including the new release-failure regression.
The suite also retained coverage for lease/replay checks, nonblocking peer
preparation, microphone gating, and frame boundaries. Rust formatting and patch
whitespace checks passed. These results do not replace Linux hardware, Windows,
iPhone, installed-native-client or live WoW validation.

The updated shared runtime was also exercised in the retained Linux validation
workspace: **52 tests passed, one hardware-dependent test ignored**. Strict
Clippy for `nanocodex-remote`, all targets/features, passed with the repository's
existing `missing_const_for_fn` exception. This was a focused runtime overlay on
the previously validated Linux workspace, not a rebuild of every host binary.
