# Nanocodex for iPhone and iPad

A native SwiftUI app for iPhone and iPad. One managed agent per card:
review the latest update, steer its current turn, then move to the next agent.
The interface uses ChatGPT-style neutral surfaces, native typography, a rounded
composer, and right-aligned user message bubbles, with Nanocodex naming and
the agent card deck, review actions, and live steering controls.
Appearance follows the system light/dark setting, including cards, the composer,
and voice controls.
The native Mac app lives in [`macos/`](../macos/README.md). It owns the tiled
workspace, agent activity menu bar, and automatic background Mac Hand. This
project targets iPhone and iPad only; Mac Catalyst and the Designed for iPad
Mac destination are disabled. It connects directly to the managed HTTP/SSE
contract without a bundled Node runtime.

The local `apple/NanocodexUI` package owns the shared chat palette, Markdown
block rendering, and copy feedback. Both Apple apps render headings, lists,
links, quotes, tables, and code using Foundation’s Markdown parser. Copy
controls work on complete responses and individual code blocks. Thinking blocks
use the same Markdown renderer, including code fences highlighted locally with
HighlightSwift in light and dark mode. Unsupported languages remain readable
as plain code. Desktop pane
arrangement, tiling, navigation shortcuts, and per-agent state remain owned by
the existing workspace.
Inbox cards use the same renderer as the conversation and preserve the full
available reply so Markdown blocks are not cut in the middle.
Unchanged Markdown stays behind an equality boundary, so typing, scrolling, and
another row's streamed updates do not reparse completed messages. The
`ChatMarkdownParse` Points of Interest signpost measures actual parsing work.

Generated attachments appear directly in iOS cards and conversations, outside
collapsed Activity. Tool text, memory payloads, and command diagnostics stay
inside Activity; only assistant replies supply conversation text. The shared
`ChatGeneratedOutput` parser combines raw and structured tool results, including
emitted `input_text`/`input_image` blocks and MCP images, audio, video, and resources.
Its default attachment-only policy applies to nested content blocks and resource
previews too. Images use bounded, cached thumbnails; audio/video have native
playback controls; provided files can be opened or shared. Embedded text resources
retain a complete downloadable file. HTML and SVG remain files. Unsupported
device-local resource identities show an unavailable message, and Activity hides
embedded binary data. Result parsing and image decoding stay outside view bodies,
and repeated inner/outer tool outputs share a stable content identity.

## App identity

The App Store Connect listing is [Centaur by Paradigm](https://appstoreconnect.apple.com/apps/6809176380),
while the installed app is named **Nanocodex**. The product is `Nanocodex.app`;
the internal Xcode target and Swift module remain `NanocodexInbox`. Existing
bundle IDs and the App Group stay unchanged so this updates the current app
without creating another install.

- Main bundle: `xyz.paradigm.centaur`
- Share extension: `xyz.paradigm.centaur.share`
- Shared App Group: `group.xyz.paradigm.centaur`
- Apple Developer team: `C3Q4NN5ZQ8`
- App icon: the official [Centaur standalone mark](https://centaur.run/brand),
  with source and rendering instructions in `Brand/README.md`.

The Xcode project, scheme, and Swift package names retain their existing internal
names. Keep the bundle IDs and App Group aligned when configuring signing.

## Run

Open `apple/NanocodexInbox.xcodeproj`, select the `NanocodexInbox` scheme, then
choose an iPhone or iPad simulator. Requires iOS 17 or later.
Choose a development team in Signing & Capabilities to run on a physical device.
Local package `InboxCore` owns the native protocol adapter, event projection,
cursor ordering, and inbox policy. PhoneNumberKit provides country calling codes
and international phone-number parsing. Local package `NanocodexVoice` owns the
native WebRTC conversation and managed voice protocol, ported from the previous
native client, with the existing WebRTC dependency.

Sign in with the same phone number and six-digit SMS code used on the webpage.
The native SMS flow is ported from the earlier iOS client. It uses the existing
managed service to verify the code and issue a device credential, saves that
credential in Keychain, then closes its temporary account session. Retries retain
the verified session; changing number or entering the Debug-only demo revokes any unused
credential. The country defaults to the device region and can be changed; enter
a local number or paste an international one. Complete pasted, autofilled, or
typed codes submit automatically once, with an explicit retry after errors.
Resend cooldowns and errors are handled in the app.
The HTTPS service origin can be changed under **Advanced**. Drafts and seen
positions are stored on this device, scoped to that connection.
Release builds restore the saved account or show SMS sign-in and use the
managed-agent API for all agent work. They ignore `--demo` and have no demo entry
point or sample-data fallback. Debug builds retain **Explore the demo** and the
`--demo` launch argument for existing CI fixtures, with explicitly labeled sample
agents and simulated actions.

## Interaction

| Action | Result |
| --- | --- |
| Swipe left | Revisit after a new update; keep the agent in All |
| Swipe right | Mark this update seen and advance |
| Undo swipe | Restore the previous card and its seen/later state; available even with an empty inbox |
| Drag down while typing | Interactively dismiss the keyboard in cards and conversations, keeping the current agent and draft |
| Pull up and release | Fill the new-thread indicator to start an agent; pull back to cancel. Use the bottom edge of the card when the preview is long enough to scroll |
| Long-press card → Previous agent | Return to the previous agent |
| Tap card | Read messages, reasoning, and expandable tool details; keep composing while reading history |
| Plus → Camera / Photos & Videos / Files | Take a photo or attach up to four photos/videos; preview or remove attachments before sending |
| Send / ⌘Return | Submit one durable follow-up; queue behind current work and dismiss the iPhone/iPad keyboard |
| Steer now on queued message | Cancel the unfinished turn ahead of it so the follow-up can start |
| Voice | Start an interactive spoken conversation with this agent; minimize the panel to keep talking |
| Stop turn | Immediately cancel the selected turn from the send button |
| Sidebar button | Open the left sidebar to jump to an agent, create one, or open Settings |
| Sidebar → Scheduled jobs | View active and paused jobs across the account, inspect their schedule, or open the source chat and latest run |

The left sidebar sits behind the inbox, which slides aside as a rounded, raised
panel. Scheduled jobs stays above agent history; New agent and Settings float
over its bottom edge. Navigation and filters float over card content so previews
can scroll through the full space. Scheduled jobs and Settings use full-page
navigation with a Back button. Agent updates refresh automatically without a
refresh button.

Scheduled jobs are created from chat. The native schedule browser reads the
existing per-agent triggers API and shows the prompt, cron expression, time zone,
next run, last dispatch, and last skipped occurrence. Dispatch does not imply
successful completion; open the linked conversation to read the result. Pull to
refresh or use Refresh to pick up changes, including jobs created in chat.
Schedules prefetch from the inbox using the sign-in agent list. Reads use a rolling
four-request limit and publish each agent's jobs immediately; one slow agent does
not block the others. The account summary's `may_have_scheduled_jobs` hint skips
known-empty conversations, including the new conversations created by cron runs.
Older servers without the hint still use the full scan. Cached owners always get
rechecked even if a roster snapshot says empty. Cached rows stay visible while the screen refreshes on open
or foreground, and overlapping refreshes share the same work. Existing jobs stay
visible if their owner's read fails, with an explicit warning. Account changes
cancel pending reads and clear the in-memory results. The Performance signposts
`ScheduledJobsRefresh` and `ScheduledJobsVisible` measure refresh and first rows.
When a schedule read returns 404, one fresh account roster confirms whether the
agent was deleted during discovery. Only confirmed removals become empty results;
advertised but unreadable agents and authorization failures retain their warning.

The inbox prioritizes completed/failed updates that have not been seen. Swiped
cards leave this pass through the inbox until a newer update arrives. Clearing
the deck shows **Nothing in your inbox**, using the same empty-page layout as
**Nothing running**. Agents appear after a real update is known; initial reads
run in the background without loading-placeholder cards. All and Previous
retain access to those agents.
Live changes preserve the focused card;
new work does not steal focus while typing. Drafts belong to agent IDs. Multiple
active turns get an explicit selector. Navigation never approves tools, stops an
agent, or deletes history. No approval endpoints are invented by this client.

Only the visible card streams. Other cards refresh with four concurrent reads
on a 15-second refresh loop. The stream resumes from an exact decimal cursor with
backoff after disconnect. Backgrounding detaches observation; agents continue
on the service. Foregrounding reloads history and resumes. Active-turn state
reads cannot overwrite newer streamed events. Changing accounts invalidates old
callbacks and cancels owned requests. Follow-up retries reuse the same turn ID
and idempotency key. The compact queued-message row sits flush above the input inside the composer surface and survives navigation and relaunch.
“Steer now” uses cancel-and-continue: it resolves the unfinished predecessor from
the current queue, captures that exact target at the tap, and cancels it without
resubmitting the durable follow-up. This is not in-flight runtime injection.
Stop and queued-message cancellation remain available during submission and
account refresh. Cancellation intent survives relaunch; the send button shows
progress until the exact turn is terminal, with a retry on an unconfirmed stop.
A queued message leaves the visible queue once its cancellation is durably
acknowledged, so its successor can be steered while terminal confirmation
continues in the background. Its cancellation and input remain saved until then.
A confirmed cancellation before admission fences any late submission of that ID.
Late send acknowledgements cannot undo cancellation. Failed sends retain input
and retry the same identity.

The voice protocol, prompt, first-utterance memory lookup policy, transcript
reducer, and retained frame queue live in `nanocodex-voice-protocol`. Browsers
consume its WASM build; Swift calls the same Rust code through a small C ABI.
Before opening/building either Apple project, install the Rust Apple targets
and build the generated (untracked) XCFramework:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
pnpm build:voice-core
```

Voice uses the same managed realtime conversation as the earlier native app:
WebRTC audio, spoken replies, transcripts, and delegation to the selected agent.
Audio and realtime events use the same peer connection; native voice does not
wait for a separate control WebSocket. Conversation admission and OpenAI call
creation run together, and the exact conversation context is sent as background
commentary before the microphone becomes active. WebRTC loads when the voice
session owner is created, without starting a call or capturing audio.
Voice opens a full-screen surface with a central spinner while connecting or
reconnecting. The violet and copper contour orb appears only after the peer, control
channel, and conversation context are ready. Chat, mute, and end controls float
along the bottom; reduced-motion settings disable the orb's ambient animation.
The entire startup has a 45-second deadline, including account preparation,
permission, previous-call cleanup, and network negotiation. Failures in any
startup branch appear immediately; stopped or timed-out callbacks cannot
reactivate audio. Cleanup retains the ordering of durable start/stop operations.
Both speakers' partial transcripts update directly in the active agent's normal
chat transcript, including when they overlap or a delegation request is in
flight. Returning to chat keeps the call active. Spoken rows survive stopping
until matching durable history replaces them, without adding text to the
composer or changing a typed draft. Internal voice envelopes are projected
into natural spoken text.
Microphone permission is requested on first use. Minimizing the panel preserves
the call and its original agent; mute and End voice remain available. Ending a
call, backgrounding, changing accounts, or an interruption stops native audio.
Provider credentials stay on the managed service. Existing typed drafts remain
separate. The Debug-only demo explains that interactive voice requires signing in.
Native voice automatically retries a confirmed agent egress startup timeout, up
to three attempts on the same call. Only this failure before voice admission gets
a fresh operation identity; transport retries keep their identity, and ambiguous
operations remain fenced. Ending voice cancels pending startup recovery.

Image attachments follow Codex's local-image flow: keep a prepared JPEG with the
agent's draft, then send it as inline image content through the existing managed
turn endpoint. Camera opens the native still-photo capture screen on iPhone and
iPad, requests access when needed, and supports cancelling or retaking before
attaching. Captures stay with the selected draft; they are not added to the photo
library. Photos & Videos and Files accept still images (including HEIC); preparation
runs off the main thread, corrects orientation, and fits four images within the
managed request limit. Images can be sent without text. Draft references and
queued retries survive relaunch and stay scoped to the account and agent; only
metadata goes in UserDefaults. Removed/delivered local copies are cleaned up,
while sent images remain in durable conversation history.

MP4/MOV video attachments retain their original bytes, including audio. Photos
and Files transfer movies as files, and preparation creates one local poster
without transcoding the source or adding sampled frames to the prompt. The
original clip and metadata stay with the account-scoped draft across relaunch.

Sending uploads the file in resumable 8 MiB parts through the authenticated
managed service to `/brain/attachments/<id>/original.mp4` (or `.mov`). The
agent receives the path using an ordinary text part, as with filesystem
attachments in codex-rs. Its tools can inspect the complete recording; native
media programs can run on a Hand with `/brain` mounted. Large files are not
buffered into a model request. Images retain their existing inline behavior.

Sent videos appear as playable attachments after reload. Playback downloads
the original using the account's authenticated client, passes a temporary
local URL to AVPlayer, and removes that copy when playback closes. The remote
file remains in the agent's filesystem until removed there or the conversation
is deleted. Historical frame-only messages remain readable with their existing
frame viewer. Camera remains still-photo capture.

`VideoAttachmentTests` covers original bytes, audio metadata, saved drafts,
legacy history, malformed references, and account isolation. Its live journey
(`NANOCODEX_VIDEO_LIVE=1` plus `NC_API_KEY`) verifies upload retry, the agent's
computed SHA-256 in two turns, durable history, and the downloaded file's exact
bytes and audio tracks. `NANOCODEX_VIDEO_LARGE=1` uses a 90-second, 101 MiB
fixture to exercise multipart transfer. The phone UI journey
`testLiveVideoAttachmentDraftSendAndHistory` uses
`Fixtures/VideoAudioCheck.mp4` in the app's shared Documents folder and
`NANOCODEX_VIDEO_UI_LIVE=1` plus `NANOCODEX_VIDEO_AGENT_TITLE` naming a dedicated
conversation with a READY reply; it covers Files selection, preview, draft restore,
agent checksum, keyboard dismissal, and original playback after reload.

The live event working set is capped at 512 events / 16 MiB. Earlier history is
loaded automatically as you scroll near the top, up to 2,048 events. Full history remains on the service.
The transcript preserves manual scroll position; it does not force-scroll on
every token.

## Automatic device Hand

Signing in also connects this device as an account Hand automatically. There is
no enable step. Local package `NanocodexHand` owns the native Hosted Tools
WebSocket connection and its `device_info`, `list_files`, `read_file`, and
`write_file` tools. Files live in an account-scoped directory under the app's
Documents/Nanocodex folder, exposed to agents as `/workspace`. Paths cannot leave
that directory or traverse symlinks; text reads and writes are limited to 64 KiB.
This does not provide an iOS shell or access to other apps' private data.

The Hand is enabled by default. **Make this device available as a Hand** in
Settings persists an explicit opt-out across relaunches and account switches.
Disabling closes the connection and cancels pending device calls; enabling
reconnects the same device identity and account-scoped workspace. Account changes
close the old connection and select a separate workspace.

On iOS 26 or later, sending or explicitly retrying a message requests a
`BGContinuedProcessingTask` for that durable turn. While iOS grants runtime, the
Hand stays connected after leaving the app or locking the screen. The system
shows progress and cancellation. Progress counts actual activity from that turn's
event stream; replayed events, other turns, and heartbeats do not advance it.
The work is resumable at the service; losing a connection does not resubmit it.
Completion releases runtime, and expiry/Stop persists cancellation for the exact
turn. If iOS refuses runtime, the message can still run in the foreground.
There is no permanent continued-processing task for an idle Hand.

**Run Agent Task** is an App Shortcut with an account-scoped agent picker and a
request parameter. On iOS 27, it adopts `LongRunningIntent` and
`CancellableIntent`, wraps the task in `performBackgroundTask`, reports progress,
and targets the main app process so it shares the existing Hand and account.
User cancellation stops the exact remote turn; a system timeout releases local
execution while durable cloud work remains available in the conversation. A
saved shortcut cannot silently follow an account switch or re-enable a disabled
Hand. Shortcuts can expose the action through Siri or a Shortcuts widget.
Earlier iOS versions request foreground continuation and queue the task in
Nanocodex; iOS 26 then uses continued processing. No separate widget extension is
required. The SDK 27-specific conformance is guarded by
`CENTAUR_APP_INTENTS_27`, selected by the Xcode project's SDK 27 build settings.
SDK 26 builds contain the foreground fallback, including when installed on iOS 27.
Build with Xcode 26 or later. CI builds both the stable SDK and the
[Xcode 27 preview image](https://github.com/actions/runner-images/issues/14404)
so the newer conformance is compiled as well.
See [WWDC26 App Intents](https://developer.apple.com/videos/play/wwdc2026/345/)
and [continued processing](https://developer.apple.com/documentation/backgroundtasks/performing-long-running-tasks-on-ios-and-ipados/).

Without an active task's runtime grant, leaving the app requests a short window
(at most 25 seconds, or less if iOS expires it). A registered app-refresh task can
restore the saved account and briefly reconnect while refreshing inbox content.
Its earliest requested start is 15 minutes later; iOS decides whether and when
it runs. Expiry closes the socket, and foregrounding reconnects automatically.
The catalog still reports `background_limited`. No APNs wake-on-call service or
cloud copy of device context is configured. Installation does not make a sleeping
phone immediately reachable, and force-quit ends background work. See
[Apple's background execution limits](https://developer.apple.com/forums/thread/685525).

On macOS, the [native workspace app](../macos/README.md#background-hands) owns
the background Hand and agent activity menu bar. Closing its window preserves
the Hand. Its keep-awake setting prevents idle system sleep without
keeping the display on. Its setting persists; keeping awake uses more battery.
Quitting, lid-close, or explicit system sleep can still interrupt availability.

The opt-in `testLiveHandDisableSurvivesRelaunchAndBackground` UI journey verifies
the disable preference through a cold launch, re-enabling against the real
service, and reconnection after 30 seconds in the background. It passed on a
physical iPhone on 2026-09-06. This is not evidence of an OS-scheduled wake or
tool dispatch while the screen is locked; those journeys remain unverified.

`testLiveHandContinuesUserTaskWhileBackgrounded` passed on a physical iPhone
running iOS 26.6 on 2026-09-06. The service recorded 12 successful phone file
operations after the old 25-second cutoff, up to 104 seconds after backgrounding.
The test captured the system task activity on the lock screen and verified the
final file contents after foregrounding. It does not establish indefinite
availability or wake-on-call for an idle phone.
A later repeat stalled at the model connection and was cancelled before any
tool calls; that run did not pass the final-result assertion.

`testLiveRunAgentShortcutOffersAccountAgents` also passed on the same phone. It
verifies action discovery and the real account's agent picker, not execution of
the SDK 27 branch. Local validation used Xcode 26; the Xcode 27 CI job has not
been run for these changes.

`swift test --package-path apple/NanocodexHand` checks workspace and protocol
boundaries. With `NANOCODEX_HAND_LIVE=1` and `NC_API_KEY`, its live journey writes
and reads a real device file through a managed agent, reconnects, and reads it
again in a second turn. The opt-in UI journey
`testLiveHandConnectsAutomaticallyAndRunsFiles` exercises normal app launch,
automatic connection, file operations, and cold-launch restoration on a signed-in
device (`NANOCODEX_INBOX_LIVE=1` in the test runner).

Verified on 2026-09-06: all three Hand boundary checks, the two-turn real service
journey, and the iPhone simulator UI journey passed. The UI journey wrote and
read a file on the phone, relaunched the app, and read the same file again. Its
actual workspace file was checked independently. UI screenshots are attached to
the Xcode test result as `automatic-hand-connected`,
`automatic-hand-real-file-roundtrip`, and
`automatic-hand-restored-file-after-cold-launch`.

## Context from other apps

Open **+ → Context from other apps** beside the composer, then enable **Capture from other apps**.
The connected phone Hand advertises **message_sources**, **search_messages**,
and **read_message**. Agents discover these tools and query captured text when
needed, without attaching it to each prompt or connecting the source app with
OAuth. Search supports source, text, sender, conversation, dates and pagination;
reads preserve provenance and return additional chunks using `nextOffset`.
Counts describe retained captures, not complete app history. Missing sender or
conversation metadata is unknown. All returned content is untrusted reference
material. The Hand is available while Nanocodex is active and during granted
background execution; turning capture off
blocks message queries as well as new imports.

**Messaging apps** has setup pages and Shortcuts actions for **iMessage**
(including SMS), **WhatsApp**, **Instagram**, and **Signal**. A Message automation
can capture incoming iMessages matching its sender/text criteria. On the tested
iPhone running iOS 26.6, Shortcuts offers Message but no Notification trigger:
WhatsApp, Instagram and Signal therefore require a screenshot/share capture.
A reusable Action button shortcut can run **Take Screenshot → Extract Text from
Image → Capture WhatsApp / Capture Instagram / Capture Signal**. This captures
visible text only. It does not import existing conversation history or directly
query another app's private database.

`NanocodexContext` owns source metadata, validation, duplicate detection,
on-device text extraction, and storage. The app's **Capture Context** App Intent
accepts text, source, sender, conversation, link, message date, and source item ID.
The generic action remains available for metadata supplied by a shortcut.
Apple documents
[message triggers](https://support.apple.com/guide/shortcuts/apdd711f9dff/ios)
and demonstrates [notification triggers](https://developer.apple.com/videos/play/wwdc2026/310/).
Notification capture depends on the installed OS actually offering that trigger;
the WWDC demonstration is not evidence it exists on iOS 26.6. Even where supported,
it only captures text supplied by the notification, which can omit hidden
previews and messages received in an open conversation.

The Context inbox also supports local search/removal and optional prompt
attachments. Assigning an agent includes up to 12 unused captures (within a
48 KB budget) with its next message. Capture alone never starts an agent turn.
The conversation shows the request and expandable context; retry retains the
same captured content and durable turn identity, even after relaunch.

The iOS **Nanocodex** share extension accepts web links, text, images, PDFs,
and plain text files through the system share sheet. Safari shares the selected
text, or readable page content when there is no selection, together with its
original URL. Long pages are capped at 24 KB with an explicit excerpt marker.
A URL from another app stays a URL unless that app also supplies text. Shared
captions are preserved, and duplicate page/link representations are combined.
**Capture Text from File**
provides the same extraction in Shortcuts; **Add context** also supports file
import. Image text recognition runs on-device. Only extracted text is retained;
original files are not retained or uploaded, and links are not fetched during
capture. Scanned PDFs and images without readable text are rejected. Inputs
are limited to 24 KB text, 8 MB per file, and 10 items per share. Oversized or
unsupported inputs produce an error without partially saving the batch.

The iOS app and share extension require the App Group
`group.xyz.paradigm.centaur` on the same development team/profiles.
In Xcode, select the same team for both targets and enable that group under
Signing & Capabilities. The app uses `NanocodexInbox.iOS.entitlements`.
Shared storage contains no account credentials, is excluded from backups, and
uses file protection after first unlock, atomic writes, and a process lock.
Capture and Hand queries are scoped to the connected account; sign-out and
capture toggles fence in-flight imports and invalidate query access. Demo
storage is separate. Records retain supplied provenance
without inventing a sender or thread. The store is capped at 1,000 records per
connection and 8 MB total; it reports capacity rather than dropping history.
Removing a capture does not erase content already submitted in a conversation.
Simulator runs use ad-hoc signing so both targets receive their App Group
entitlements; disabling signing only checks compilation and cannot exercise
the shared container.

`ContextUITests.testSafariShareReachesContextInbox` exercises Safari's iOS 26
share sheet, the extension preview/save, and the imported page text appearing when
the app resumes. It has been verified on a signed physical iPhone with a demo
account, including the shared App Group container. The test skips older system
share-sheet layouts.

Incoming Message automation execution and capture while locked still require
device validation. Automatic capture of WhatsApp, Instagram and Signal is not
available through a Notification trigger on the tested phone.

`HandIntegrationTests.testRealAgentQueriesCapturedMessagesThroughHand` exercises
the real managed service with synthetic on-device messages. Its prompts omit
the captured content; the agent must discover the Hand tools and read booking
codes beyond the search excerpts. It then reconnects the Hand, adds fresh
Signal context through a separate store writer, and queries it in another turn.
Local boundary tests cover all four sources, account/capture fences, malformed
queries, source aliases and Unicode pagination.

Verified on 2026-09-06: the four-source live Hand journey and reconnect passed.
On a signed physical iPhone, `testLiveShortcutsMessageCanBeQueriedThroughHand`
ran Capture Context in Shortcuts with the app terminated, reopened the account,
and asked an agent to find the newly captured text through the actual phone
Hand. The agent returned the synthetic text without it being attached to the
prompt. The four messaging setup screens also passed native UI checks. These
tests do not send messages to other people or establish incoming notification
automation behavior.

```sh
NANOCODEX_HAND_CONTEXT_LIVE=1 swift test --package-path apple/NanocodexHand --filter HandIntegrationTests/testRealAgentQueriesCapturedMessagesThroughHand
```

`ContextDeliveryTests.testCapturedContextSurvivesLiveDeliveryAndReplay` is an
opt-in native client test against the real managed service. With
`NANOCODEX_CONTEXT_LIVE=1` and an account `NC_API_KEY`, it creates its own agent,
imports synthetic Instagram/Message text through `NSItemProvider`, routes each
source, verifies the agent's answer, reconnects and replays the serialized
outbox, checks exactly two durable terminal events, and deletes the test agent.
It does not need or inspect a user's message history.

```sh
NANOCODEX_CONTEXT_LIVE=1 swift test --package-path apple/NanocodexContext --filter ContextDeliveryTests
```

## Validation

`TurnControlIntegrationTests` exercises cancellation before admission against
the managed service, including a late submission, submission replay, and repeated
Stop. It creates and deletes its own agent. Run it with an account `NC_API_KEY`:

```sh
NANOCODEX_TURN_CONTROL_LIVE=1 swift test --package-path apple/InboxCore --filter TurnControlIntegrationTests
```

```sh
swift test --package-path apple/InboxCore
swift test --package-path apple/NanocodexVoice
swift test --package-path apple/NanocodexContext
xcodebuild -project apple/NanocodexInbox.xcodeproj -scheme NanocodexInbox -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- build
```

The `Apple apps` workflow runs protocol/policy tests, builds the native Mac
workspace from `macos/` with its background Hand checks, builds the iOS app,
and drives native iPhone Debug demo journeys covering swipes, per-agent drafts, queue
recovery, cancel failures, repeated taps, relaunch, thread continuity, and voice
sheet dismissal/error handling. It attaches screenshots, simulator video, and the
full Xcode result as `native-inbox-evidence`. Demo automation does not establish
authenticated service behavior, real microphone audio, or physical-device
performance. Navigation animations are 160–180 ms; working text fades over
1.1 seconds per phase. Both respect Reduce Motion.

Physical-device performance tests use the saved account and real managed-agent
history. Set `NANOCODEX_INBOX_PERFORMANCE=1` and `NANOCODEX_INBOX_LIVE=1` in the
XCTest runner environment. For `testPerformanceInboxInteractionJourney`, also set
`NANOCODEX_PERFORMANCE_AGENT_TITLE` to the exact title of an existing real agent
with conversation history on that account. These journeys use no demo arguments,
sample agents, or mocked service responses.

`testPerformanceSavedAccountResponsiveColdLaunch` measures process-cold launch
until the app responds and separately records saved-account restoration through
the `RestoreAccount` signpost. OS and filesystem caches remain warm.
`testPerformanceInboxInteractionJourney` records CPU, memory, and hitch metrics
(where supported) while scrolling real history, editing local input, reopening
the conversation, and selecting the agent from the list. Initial account and
history loading happen before its measured interval. It restores the original
draft afterward and never sends or queues the temporary input. XCTest automation
wall time is not a UI-response latency measurement.

`testPerformanceDemoConversationRendering` is a separate deterministic simulator
check with `NANOCODEX_INBOX_PERFORMANCE=1`. It opens 80 rich Markdown replies and
measures app CPU and memory while appending the same 50-character sentence to a
local draft, with three measured iterations after XCTest's warm-up. The fixture includes
headings, inline styles, lists, quotes, and tables. It uses an isolated draft
scope and never connects to the account service. Use `ChatMarkdownParse` Points
of Interest with Time Profiler to inspect actual parsing during this journey;
simulator metrics do not represent physical-device input latency.

Thinking, tool calls, explicit progress commentary, and subagent updates share one collapsed **Activity** row per turn. Its status and step count update in place; tool failures remain visible as an issue count. Expand once for a compact, scrollable timeline, then expand a step for its notes, inputs, and results. Both levels have bounded height. Final answers and errors remain visible outside Activity; older untagged assistant text is preserved. Commands retain code formatting and structured results use readable fields. Expansion respects Reduce Motion, and new steps never grow the closed transcript.

New conversations open synchronously as local drafts. Creation runs in the background using a persisted idempotency key; Send and voice share that request. Draft text, pending messages, attachments (including imports still in progress), context selections, and keyboard focus survive the server identity arriving. A late response never changes the selected conversation. Failed creation can be retried from the composer, and unfinished drafts survive relaunch.

Verified on 2026-09-06: seven native UI checks passed, including creation delayed by 10–20 seconds, immediate send, cancellation before admission, draft and keyboard preservation, retry, navigation, and relaunch. The signed-in iPhone journey also passed against the real backend: opening, two turns, history after relaunch, and voice connect/mute/minimize/end.

Conversation scroll targets retain the visible message across prepended history and new output, and new conversations open at the latest messages. Returning to the foreground resumes the existing cursor and transcript rather than clearing the screen. Card drags lock their direction so vertical reading does not become a horizontal swipe; card changes do not crossfade.

The conversation keeps the same agent composer fixed above the keyboard while you read older messages. Sending dismisses the iPhone/iPad keyboard; ordinary submissions do not briefly insert a queue panel. Queued follow-ups and delivery errors retain their controls. With an empty draft and a running turn, the send button becomes Stop; adding text or an image restores Send in the same position. Drafts, queued follow-ups, steering, and stop controls are shared with the inbox card, so opening or closing a conversation preserves your work.

The native Debug demo suite additionally exercises long-thread reading during new output and foregrounding, older-history pagination, vertical card scrolling, repeated fast swipes, new-agent creation and immediate stopping from the send button, the empty inbox, inferred phone country codes, a multi-message queue with the keyboard open, and voice sign-in/draft preservation. Demo agents and injected failures are fixtures; this does not validate an authenticated service or physical microphone.

`VoiceIntegrationTests.testNativeManagedVoiceConnectsAndStops` is an opt-in real
service journey (`NANOCODEX_VOICE_LIVE=1` and an account `NC_API_KEY`). It creates
and deletes its own agent and checks receive-only native WebRTC, authenticated
data-channel control, background context, mute, immediate stop, and cancellation
during startup. `NANOCODEX_VOICE_TIMING=1` additionally records startup stages,
HTTP timings, and media round-trip time without SDP, payloads, or credentials.
It never captures microphone audio or claims to validate spoken interaction.
`InboxUITests.testLiveVoiceConnectsMinimizesAndStops` checks two real voice
connections, minimizing, and ending on a signed-in iPhone with
`NANOCODEX_VOICE_UI_LIVE=1`. It mutes the microphone during startup.
