# Nanocodex for iPhone and iPad

A native SwiftUI app for iPhone and iPad. Each managed agent has an independent
conversation in a searchable drawer. The compact conversation header, blue user
bubbles, unboxed replies, and rounded composer keep the conversation in focus.
Use the compose button to start another conversation. Back, per-conversation screens,
and captured context are available in the app menu. Queue and steering controls
remain with each conversation.
Nanocodex retains per-agent drafts, steering, voice, and remote screens.
Appearance follows the system light/dark setting, including conversations, the composer,
and voice controls.
The native Mac app lives in [`macos/`](../macos/README.md). It owns the tiled
workspace, agent activity menu bar, and automatic background Mac Hand. This
project targets iPhone and iPad only; Mac Catalyst and the Designed for iPad
Mac destination are disabled. It connects directly to the managed HTTP/SSE
contract without a bundled Node runtime.
Its authenticated native Hand can also bridge [Bluetooth Low Energy devices](../docs/bluetooth-le.md), including typed [Flipper Zero](../docs/flipper-zero.md) RPC tools without custom Flipper firmware.

The local `apple/NanocodexUI` package owns the shared chat palette, Markdown
block rendering, and copy feedback. Both Apple apps render headings, lists,
links, quotes, tables, and code using Foundation’s Markdown parser. Copy
controls work on complete responses and individual code blocks. Thinking blocks
use the same Markdown renderer, including code fences highlighted locally with
HighlightSwift in light and dark mode. Unsupported languages remain readable
as plain code. Desktop pane
arrangement, tiling, navigation shortcuts, and per-agent state remain owned by
the existing workspace.
The sidebar lists all known conversations, including conversations previously hidden
by the old tab interface. Rows are created lazily and search covers the whole roster.
Roster and state checks continue for background work; transcripts load when opened.
The composer grows up to six lines, then scrolls; its expand button opens a larger
editor sharing the same draft and attachments.
Paste a copied screenshot or photo into either editor to attach it without opening
the picker. Image providers enter the same account-scoped draft flow as Photos and Files;
they do not replace your typed message. Plain text continues to use native text
editing. With the phone Hand enabled, sending images retains their originals and
previews in its account-scoped workspace and sends small path references. The
agent uses that phone’s `view_image` tool to inspect them; the phone must be
connected. Composer and history thumbnails read the retained local previews,
including after draft cleanup and relaunch. Videos and images sent with the Hand
disabled continue to use authenticated cloud uploads.
Image attachments appear in a compact, trailing-aligned grid above the message
text, with square crops for multiple images and preserved proportions for a single
image. Tapping opens the full original. The composer uses a horizontally
scrollable 120-point thumbnail strip without filename captions. Thumbnail decoding starts
when the view appears, preserves EXIF orientation, and shares a bounded cache
so scrolling does not repeatedly blank and decode the same images.


The current conversation stays mounted while the session drawer opens. The drawer
uses lightweight roster summaries and search, without tabs, preview grids, or status
filters. The drawer uses the shared neutral sidebar palette and system sans-serif typography.
Titles stay neutral, with a small green dot for running agents and explicit status
labels available to VoiceOver. Titles, status and current-work text scale with Dynamic
Type. Search and compose sit at the top; Settings stays at the bottom.
Selecting a row restores that agent's draft and reading position. Swipe right from
the left 28 points of the screen to open the drawer; swipe left to close. The
conversation follows the finger and settles with a short spring over the stationary list. Vertical scrolling
keeps it open. Previously hidden conversations are available in this same list.
Reply bubbles hug their content with 12-point horizontal and 9-point vertical padding.
Long-press a reply to copy it; code blocks retain their own copy controls. The composer
uses 4-point vertical padding around controls with 44-point touch targets.
Unchanged Markdown stays behind an equality boundary, so typing, scrolling, and
another row's streamed updates do not reparse completed messages. Parsing runs on
a background actor with a bounded cache; streamed changes coalesce for 32 ms,
and cancelled parses cannot replace newer content. The
`ChatMarkdownParse` Points of Interest signpost measures actual parsing work.

Commentary and reasoning summaries appear inline in chronological order. Each
tool call has a compact card with expandable input and result details. Short shell
commands remain readable in full; oversized commands use a bounded preview. Full
large source opens in a scrollable, read-only native viewer with a copy control,
so encoded payloads and long blank runs cannot stretch the conversation. Syntax
highlighting is skipped for sources larger than 16 KiB.
Generated attachments appear directly after their originating tool card.
The thread controls above the composer collapse all tool disclosures, including nested
Code Mode tools and JavaScript. Individual tools can be reopened, and their choices
remain independent when switching threads. Up and down arrows move between user
messages, fetching earlier or later history when necessary. Manual scrolling resets
the arrow position; the separate latest-message control resumes following responses.
All controls have 44-point targets and VoiceOver labels.
Tool text, memory payloads, and command diagnostics stay inside that disclosure. The shared
`ChatGeneratedOutput` parser combines raw and structured tool results, including
emitted `input_text`/`input_image` blocks and MCP images, audio, video, and resources.
Its default attachment-only policy applies to nested content blocks and resource
previews too. Images use bounded, cached thumbnails; audio/video have native
playback controls; provided files can be opened or shared. Embedded text resources
retain a complete downloadable file. HTML and SVG remain files. Unsupported
device-local resource identities show an unavailable message, and tool details hide
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
choose an iPhone or iPad simulator. Requires iOS 18 or later.
Choose a development team in Signing & Capabilities to run on a physical device.
For phone-triggered signed builds and self-updates, see
[Phone-driven Nanocodex delivery](../docs/iphone-delivery.md).
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
On cold launch, the last tab's history request overlaps the authenticated account
list. History is published only after the account list confirms that tab still
exists. Other conversations and scheduled-job prefetch wait for the initial
focused history request, leaving bandwidth available for the first conversation.
Switching tabs, changing accounts, or backgrounding cancels owned history reads.
Normal account, conversation, preview, and reconnect loading uses spinners;
authentication failures retain the sign-in action.

Release builds restore the saved account and last selected tab, or show SMS sign-in, and use the
managed-agent API for all agent work. They ignore `--demo` and have no demo entry
point or sample-data fallback. Debug builds retain **Explore the demo** and the
`--demo` launch argument for existing CI fixtures, with explicitly labeled sample
agents and simulated actions.

## Interaction

Use **Remote screens** from the inbox or a conversation to watch a published
Hand desktop and take control. On iPhone and iPad, screens open in a native sheet
with grouped screen cards. Drag the sheet to expand it, pinch the screen to zoom,
and tap **Done** or swipe down to return to the conversation with your draft intact.
Resizing keeps the selected screen connected; closing releases its session.
Remote typing controls are behind the keyboard button after taking control.
The iPhone/iPad and native Mac app share the
`NanocodexRemote` WebRTC viewer, including video, pointer, keyboard, and control
leases. Desktop-enabled factory VMs publish automatically; the screen list
refreshes while open. Shell-only VM images have no graphical desktop. Cloudflare
sandbox desktops require the managed desktop feature flag and use authenticated
JPEG screen updates instead of WebRTC video.

Backgrounding releases control and pauses the viewer while retaining its selected
screen. Returning reconnects with the current publication generation. Transport
failures retry with backoff for up to 90 seconds, then offer **Reconnect**. Reconnection resumes viewing;
take control again to send input. Closing the screen cancels recovery. On phones,
tap to click, drag to move, use two fingers to scroll, and use the text field or
Return/Tab/Esc controls below the video.

| Action | Result |
| --- | --- |
| Header → Conversations | Search and switch agents while preserving each agent’s draft and queued messages |
| Dock plus | Open a new agent immediately and start composing |
| Swipe right from the left edge | Open the conversation drawer, including while composing |
| Swipe drawer left | Return to the current conversation without losing the draft |
| Drag down while typing | Interactively dismiss the keyboard while keeping the current conversation and draft |
| Scroll a conversation | Read the full history, reasoning, and expandable tool details while keeping the composer available |
| Attachment plus → Camera / Photos & Videos / Files | Take a photo or attach photos/videos; preview or remove attachments before sending |
| Send / ⌘Return | Submit one durable follow-up; queue behind current work and dismiss the iPhone/iPad keyboard |
| Steer now on queued message | Inject the queued input into the active turn through the steering API |
| Voice | Start an interactive spoken conversation with this agent; minimize the panel to keep talking |
| Stop turn | Immediately cancel the selected turn from the send button |
| Header menu → Account settings | Manage the account and device Hand in a dismissible sheet |
| Header menu → Scheduled jobs | View active and paused jobs across the account, inspect their schedule, or open the source chat and latest run |

The compact header shows the selected conversation, its running indicator, a
Conversations button, and the app menu. Below the composer, the floating dock
groups Back, Remote screens, new-agent plus, and captured context.
Back returns to the previous conversation, retaining its draft and reading position.
The sidebar owns conversation switching. The drawer uses a short, damped horizontal transition and respects
Reduce Motion. Glass controls use opaque surfaces with Reduce Transparency.

Attachments open in a native sheet, then hand off to the existing photo, camera,
or file picker after dismissal. Settings has its own navigation stack in a sheet;
remote screens use medium/large sheet detents and retain the selected viewer while
resizing. These surfaces leave the conversation and its draft in place. The app
menu keeps Scheduled jobs and Connectors accessible. Horizontal swipes inside the
transcript and upward pulls do not switch agents or create conversations.
Streamed responses follow the bottom while you are reading the latest output. Scrolling
back pauses following and preserves your reading position. A small circular down-arrow
above the composer returns to the latest messages and resumes following, including
when newer history must first load. Each tool call appears as a compact inline card with expandable details. Scheduled jobs and Settings use full-page
navigation with a Back button. Agent updates refresh automatically without a
refresh button.

Scheduled jobs are created from chat. The native schedule browser reads the
existing per-agent triggers API and shows the prompt, cron expression, time zone,
next run, last dispatch, and last skipped occurrence. Dispatch does not imply
successful completion; open the linked conversation to read the result. Pull to
refresh or use Refresh to pick up changes, including jobs created in chat.
Schedules prefetch after the opening conversation history using the sign-in agent list. Reads use a rolling
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

When no conversation is available, the empty page offers an action to start one.
The sidebar sorts by your last sent message, newest first, and retains its order while replies
arrive. Searching keeps the current conversation selected.
Live changes preserve the selected conversation;
new work does not steal focus while typing. Drafts belong to agent IDs. Multiple
active turns get an explicit selector. Navigation never approves tools, stops an
agent, or deletes history. No approval endpoints are invented by this client.

The selected agent receives live updates. Other agents refresh in the background.
The stream resumes from an exact decimal cursor with
backoff after disconnect. Backgrounding detaches observation; agents continue
on the service. Foregrounding reloads history and resumes. Active-turn state
reads cannot overwrite newer streamed events. Changing accounts invalidates old
callbacks and cancels owned requests. Follow-up retries reuse the same turn ID
and idempotency key. The compact queued-message row sits flush above the input inside the composer surface and survives navigation and relaunch.
“Steer now” reads the queued turn's exact admitted input, durably cancels only
that queued follow-up, and posts its payload to the captured active turn's
`/steer` endpoint with a stable `message_id`. The active turn keeps running;
the runtime applies steering at its model boundary. Cancellation of the queued
source prevents the same input from also executing as a later turn.
`/withdraw-steer` handles withdrawal with the same identity. A false response
means withdrawal was not confirmed, never that the active turn should be stopped.
Steering transfer phases and accepted markers survive relaunch. An uncertain
POST is retained as unconfirmed and is not automatically retried: identified
steering rejects duplicate identities, and `run.steered` does not identify the
client message. Accepted steering is shown in conversation with a steering label;
preparation/errors remain in the queue. Terminal-target rejection retains input.
Stop and queued-message cancellation remain available during submission and
account refresh. Cancellation intent survives relaunch; the send button shows
progress until the exact turn is terminal, with a retry on an unconfirmed stop.
A cancelling follow-up stays in the queue until terminal confirmation. Once its
cancellation is durably acknowledged, its successor can be interrupted for;
the cancelled request is retained in history as a cancellation, not a sent bubble.
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

Voice settings on iPhone, iPad, and Mac save the built-in voice, pace, speaking
style, spoken-update preference, and acknowledgement preference. Changing them
during a call reconnects voice while preserving the agent's work. Existing
saved voice choices migrate to these settings. Rust builds the subscription
session and validates all preferences; `VoiceSession.speak`, `appendText`, and
`appendContext` expose the same retained control queue as browser consumers.

Image attachments preserve their original files, including full resolution and
metadata. Camera opens the native still-photo capture screen on iPhone and iPad;
captures remain with the selected draft and are not added to the photo library.
Photos & Videos and Files transfer originals as files. ImageIO creates a separate
small, correctly oriented JPEG preview without recompressing the original. The
picker does not impose a selection count. Images can be sent without text.
Account-scoped draft references and queued retries survive relaunch; only metadata
goes in UserDefaults. Removed/delivered local copies are cleaned up.

MP4/MOV video attachments also retain their original bytes, including audio.
Preparation creates one local poster without transcoding the source or adding
sampled frames to the prompt.

Sending streams original files in resumable parts (normally 8 MiB, scaled for large files) through the authenticated
managed service to `/brain/attachments/<id>/original.<extension>`. A separate JPEG
preview is uploaded alongside each image original. The agent receives filesystem paths
in an ordinary text part; image tools can inspect the JPEG preview, and tools or
native media programs on a Hand can inspect the complete original. File bytes
are not embedded in a model request or conversation history. History images load
authenticated previews through the account's HTTP cache, with immutable private
cache headers.

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

The live event working set is bounded by payload size: 16 MiB in the focused
conversation and 8 MiB in overview previews, always retaining the newest event.
Token count does not truncate a live reply. Earlier history is
loaded automatically in either direction as you scroll, without a history-length cutoff. The working window can exceed its memory target to preserve a visible or unfinished turn. Full history remains on the service.
The transcript preserves manual scroll position; it does not force-scroll on
every token.

## Automatic device Hand

Signing in also connects this device as an account Hand automatically. There is
no enable step. Local package `NanocodexHand` owns the native Hosted Tools
WebSocket connection and its `device_info`, `list_files`, `read_file`, and
`write_file` tools. Files live in an account-scoped directory under the app's
Documents/Nanocodex folder, exposed to agents as `/workspace`. Paths cannot leave
that directory or traverse symlinks; text reads and writes preserve full UTF-8 files. The Hosted Tools caller controls the result budget; the transport follows Cloudflare’s 32 MiB WebSocket message limit.
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
Completion releases runtime. Expiry releases local observation and device tools;
it never cancels the durable cloud turn. Only an explicit in-app Stop or a
Shortcut user-cancellation request can persist cancellation for that exact turn.
If iOS refuses runtime, cloud work continues and device tools require foreground time.
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

## Read photos through the phone Hand

With Photos read access already enabled in device permissions, the connected
phone Hand can use `search_photos` to find accessible asset IDs and `read_photo`
to inspect one image directly. Limited access stays limited to the selected
library. The tool does not prompt for permission or download iCloud-only assets.
It returns an oriented JPEG inspection image, bounded to 2048 pixels and 512 KiB,
and saves that rendition under the phone workspace's `photos/` directory. The
original remains in Photos. Code Mode can display the returned MCP image with
`image(result.content[1])`; the phone's text-only `read_file` is not an image reader.
The phone must be connected and have foreground or iOS-granted background time.
For durable original files available while the phone is offline, attach with
Photos, Files, or Paste and send the message through the R2-backed upload flow.

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
attachments. Assigning an agent includes its unused captures with the next message. Capture alone never starts an agent turn.
The conversation shows the request and expandable context; retry retains the
same captured content and durable turn identity, even after relaunch.

The iOS **Nanocodex** share extension accepts web links, text, images, PDFs,
and plain text files through the system share sheet. Safari shares the selected
text, or readable page content when there is no selection, together with its
original URL. Selected text or page text is retained in full.
A URL from another app stays a URL unless that app also supplies text. Shared
captions are preserved, and duplicate page/link representations are combined.
**Capture Text from File**
provides the same extraction in Shortcuts; **Add context** also supports file
import. Image text recognition runs on-device. Only extracted text is retained;
original files are not retained or uploaded, and links are not fetched during
capture. Scanned PDFs and images without readable text are rejected. Native Vision handles
image decoding for recognition. Capture imposes no additional text, file, PDF-page,
or batch-count admission caps. Unsupported or unreadable inputs produce an error
without partially saving the batch.

The iOS app and share extension require the App Group
`group.xyz.paradigm.centaur` on the same development team/profiles.
In Xcode, select the same team for both targets and enable that group under
Signing & Capabilities. The app uses `NanocodexInbox.iOS.entitlements`.
Shared storage contains no account credentials, is excluded from backups, and
uses file protection after first unlock, atomic writes, and a process lock.
Capture and Hand queries are scoped to the connected account; sign-out and
capture toggles fence in-flight imports and invalidate query access. Demo
storage is separate. Records retain supplied provenance
without inventing a sender or thread. The JSON store retains captures without
count or byte admission caps; reading and writing still processes its snapshot.
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

`RemoteScreenLifecycleUITests.testPublishedScreenSurvivesRepeatedPresentation`
uses the app's saved account and an explicitly selected published screen. Set
`NANOCODEX_TEST_REMOTE_MACHINE_ID` and `NANOCODEX_TEST_REMOTE_SURFACE_ID` in the
XCTest runner environment. It opens, selects, returns to the screen list,
reselects, and dismisses the viewer four times, including one background/foreground
resume. It checks that viewing resumes without acquiring control or sending
input, and retains screenshots from the first and last cycles. This covers
the UIKit canvas teardown that previously published session state during
SwiftUI invalidation and could abort the app. Renderer teardown now only
detaches the renderer; the dashboard owns closing the session.

`InboxUITests.testRemoteScreenControlAndReconnect` uses the phone's saved account
and an explicitly selected disposable VM with a focused terminal. Set
`NANOCODEX_TEST_REMOTE_ORIGIN` (HTTPS) and `NANOCODEX_TEST_VM_MACHINE_ID` in the
XCTest runner environment. It creates a
unique file through remote keyboard input, backgrounds with an unsent draft,
checks selection/control/draft recovery, reconnects after relaunch, and opens
the same screen from an existing agent's conversation. Screenshots retain the
decoded video and terminal readback. With `NANOCODEX_TEST_REMOTE_RESTART=1`, wait
for `REMOTE_RESTART_READY` in the test log, then stop and restart that VM's
factory; the test checks automatic recovery without reselecting the screen.

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
and drives native iPhone Debug demo journeys covering tabs, per-agent drafts, queue
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

`testPerformanceCurrentSessionDrawerAndSheets` measures the currently selected saved
conversation while opening/dismissing the sidebar, attachments, and settings. It
preserves the selected identity and draft, sends no messages, and records three
CPU/memory/hitch iterations after XCTest's warm-up. The final Muse-inspired UI
passed this check on iPhone 17 Pro with zero hitches in all three iterations; this
is a narrow navigation result with no Muse or pre-change baseline. See
[the reference study and validation notes](../docs/ux/2026-09-16-muse-dogfood.md).

`testPerformanceSavedAccountResponsiveColdLaunch` measures process-cold launch
until the app responds and separately records saved-account restoration through
the `RestoreAccount` signpost. OS and filesystem caches remain warm.
`testPerformanceInboxInteractionJourney` records CPU, memory, and hitch metrics
(where supported) while scrolling real history, editing local input, opening and dismissing
the overview, and finding the agent in the searchable overview. Initial account and
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

Thinking summaries, progress commentary, subagent updates, and final answers remain visible inline in chronological order. Each tool call occupies its own compact, single-line card showing its action, subject, and state. Tap a card to expand its inputs and results in the main conversation scroll. Commands retain code formatting and structured results use readable fields. Generated attachments and secure Vault forms remain visible outside the tool disclosure. Cards keep their identities when results arrive, and explicit accessibility expansion values accompany each control.

Images and videos open in native Quick Look, including original uploads, draft attachments, and generated media. Original files download only when opened; the conversation uses bounded thumbnails with stable loading heights. Generated media has independent, stable transcript rows; it is projected with its history page so earlier outputs within the same turn cannot arrive as a second layout insertion. Scrolling toward earlier messages prefetches one cursor-bound page within two viewports of the top, without inserting it or moving the reader. Crossing the load boundary reuses that request and presents the page without the live-stream batching delay. Geometry updates reuse an item index and only recalculate history retention when the visible selection changes. Reconnect controls do not change the transcript viewport, and history insertions cannot reverse the inferred swipe direction. One history insertion consumes the direct scroll direction that triggered it; deceleration and bounce-back do not establish a new paging direction. Expanded tool cards retain their reading position while earlier work arrives. Legacy sampled video frames stay grouped inside one attachment and open as a native preview collection. Attachment descriptors, echoed user history, and image-inspection results do not become generated replies. Remote screens use UIScrollView/AppKit magnification: pinch to zoom, pan locally while watching, or use two fingers to pan a magnified screen while controlling its pointer with one finger.

The focused media journey is `InboxUITests/testNativeMediaPreviewZoomPlaybackAndDraftRestoration`. The screen fixture journey is `RemoteScreenLifecycleUITests/testScreenCardZoomDismissalAndDraftRestoration`; start `NanocodexInboxUITests/fixtures/remote-screen.mjs` and pass `TEST_RUNNER_NANOCODEX_SCREEN_FIXTURE=1` to xcodebuild.

New conversations open synchronously as local drafts. Creation runs in the background using a persisted idempotency key; Send and voice share that request. Draft text, pending messages, attachments (including imports still in progress), context selections, and keyboard focus survive the server identity arriving. A late response never changes the selected conversation. Failed creation can be retried from the composer, and unfinished drafts survive relaunch.

Verified on 2026-09-06: seven native UI checks passed, including creation delayed by 10–20 seconds, immediate send, cancellation before admission, draft and keyboard preservation, retry, navigation, and relaunch. The signed-in iPhone journey also passed against the real backend: opening, two turns, history after relaunch, and voice connect/mute/minimize/end.

Conversation scroll targets retain the visible message across prepended history and new output, and new conversations open at the latest messages. Returning to the foreground resumes the existing cursor and transcript rather than clearing the screen. Conversation scrolling preserves the selected agent; navigation uses the searchable sidebar.

The conversation keeps the same agent composer fixed above the keyboard while you read older messages. Sending dismisses the iPhone/iPad keyboard. Sending to an idle conversation immediately displays the message and local attachment previews in the transcript, even while conversation creation or admission is pending. The bubble retains its identity through acknowledgement and execution; unconfirmed delivery shows Retry and Cancel beside that message. Follow-ups waiting behind another turn appear once in the queue above the composer; execution evidence promotes them into the conversation. API-accepted steering appears with an explicit steering label. The queue follows server order across devices and relaunch; messages whose content has not loaded retain a placeholder and queue position. Cancelling and retrying keep the same identity. “Steer now” injects the input through the active turn’s steering API without stopping that turn. With an empty draft and a running turn, the send button becomes Stop; adding text or an image restores Send in the same position. Drafts, queued follow-ups, steering, and stop controls belong to the selected agent. Switching conversations or opening the drawer preserves that work.

Verified on 2026-09-17: eight focused compact-UI simulator journeys passed across
runs, plus 24 shared rendering tests. The Release iPhone 17 Pro test preserved the
conversation and draft through three measured drawer-left-swipe, attachment, and
settings rounds, with zero reported hitches. The drawer uses tap recognition that
fails during a drag, preventing swipe release from selecting a row. See the UX
notes and compact-device metrics above for scope, recordings, and limitations.

Verified on 2026-09-12: focused simulator checks cover immediate first-send rendering during delayed creation, stable bubbles through delayed failure and Retry, cached tabs without reload, startup restoration, and reading position during streaming. Transcript grouping is computed with the conversation revision rather than each scroll update. Row geometry does not publish per-pixel view updates, and history navigation follows native scroll events without an additional drag recognizer. The phone uses a fully measured native stack for its bounded history window, avoiding feedback between estimated lazy heights and scroll restoration. Native visibility events load and release generated image thumbnails as they enter and leave the viewport. Native size-change anchoring follows streamed output. Image previews apply media validation directly without encoding the complete image into JSON first. Desktop thread loading uses the system progress indicator, and first-send failures remain beside their original bubble. These fixture checks do not measure physical-device network latency.

Verified on 2026-09-08: focused iPhone/iPad checks cover browser tabs, searchable live previews, the All/Running filter, app-menu navigation, independent drafts, keyboard placement, point-based reading restoration, slow creation, cancellation, and retry. The signed-in iPhone 17 Pro completed a real reply, relaunched, and retained both messages through three round trips to other tabs. InboxCore passed 68 tests with three skips. The top tab strip, bottom Back/+/overview/screens/menu bar, Back draft restoration, and activity-sorted overview were checked in iPhone and iPad simulators. These tab checks do not establish voice latency or microphone performance.

The native Debug demo suite additionally exercises long-thread reading during new output and foregrounding, older-history pagination, conversation scrolling without swipe navigation, tab switching and draft isolation, live overview updates, plus-button creation and immediate stopping from the send button, the empty inbox, inferred phone country codes, a multi-message queue with the keyboard open, and voice sign-in/draft preservation. Demo agents and injected failures are fixtures; this does not validate an authenticated service or physical microphone.

`VoiceIntegrationTests.testNativeManagedVoiceConnectsAndStops` is an opt-in real
service journey (`NANOCODEX_VOICE_LIVE=1` and an account `NC_API_KEY`). It creates
and deletes its own agent and checks receive-only native WebRTC, authenticated
data-channel control, background context, mute, immediate stop, and cancellation
during startup. `NANOCODEX_VOICE_TIMING=1` additionally records startup and cleanup
stages, HTTP timings, event types, playback state, and audio transport statistics
without SDP, transcripts, payloads, or credentials.
It never captures microphone audio or claims to validate spoken interaction.
`VoiceLatencyTests.testManagedStartupLatency` separately repeats three real
receive-only connections with `NANOCODEX_VOICE_LATENCY=1` and `NC_API_KEY`.
It reports time to the native active state, checks data-channel acknowledgements,
and removes its validation agent. Enable `NANOCODEX_VOICE_TIMING=1` to split out
call HTTP, peer, backend, and task-admission timing. These measurements exclude
microphone capture and do not establish physical iPhone audio latency.
`InboxUITests.testLiveVoiceConnectsMinimizesAndStops` checks two real voice
connections, received test-phrase audio, minimizing, and ending on a signed-in
iPhone with `NANOCODEX_VOICE_UI_LIVE=1`. The first call activates the microphone;
the second is muted during startup and must still receive test-phrase audio.

### Mobile work scheduling

Focused conversations, overview streams, history pagination, and background roster
refreshes use `TranscriptPreparation` to build transcript rows and measure JSON
payloads off the main actor. One projection per observed stream batches arrivals;
new frames received during a projection schedule the next batch. Account and tab
observation tokens fence every result. Cached tabs retain prepared rows and byte
counts, so switching tabs does not synchronously rebuild history.

Draft and queue writes use an ordered background preferences queue. Send and Stop
await earlier writes before issuing their durable command, reconnect waits before
restoring the account, and backgrounding gives outstanding saves execution time.
SwiftUI rendering and the final model mutations stay on the main actor.

Live transcript projection processes each new event once per reading window;
older-history pagination and retained-prefix changes rebuild the projection.
Inactive tabs share a 24 MiB serialized-payload budget (at most eight tabs),
in addition to the focused reading window. iOS memory warnings
release inactive tab caches without removing service history or drafts.

### Native voice delivery and latency

The Mac, iPhone, and iPad apps share `NanocodexVoice` and the rebuilt
`NanocodexVoiceCore.xcframework`. Its C ABI selects the same client-managed Rust
handoff policy as the browser/WASM SDK: completed finals only, input-generation
fencing, caption-confirmed delivery, and text recovery for unconfirmed or
superseded answers. Typed sends, steering, and cancellation fence speech in the
owning conversation. Recovered results remain visible after stopping and settle
when durable history contains them; account changes clear retained text.

Media negotiation, durable admission, and event subscription run concurrently.
The apps pass their known conversation cursor, avoiding a state GET before event
subscription. Microphone activation waits for the secure peer, data channel, and
backend readiness; provider handoffs queue until admission and event consumption
are ready. The Connecting spinner ends at the media boundary, while the startup
deadline remains active until task setup completes. A denied admission closes
the call. Startup sends no workspace/history context. Completed speech frames go
directly to the ordered RTC data channel without a Swift task or flush delay.
A media connection timeout gets one fresh call after cleanup and preserves mute.

iOS configures WebRTC voice processing before creating audio tracks, requesting
48 kHz and 10 ms device buffers. Actual device rates/buffers remain OS-controlled;
WebRTC retains its adaptive jitter buffering and echo/noise/gain processing.
Timing diagnostics and the opt-in receive-only voice integration test report
startup and request-to-first-observed-audio time. These checks do not measure
microphone-to-audible-response latency or establish identical latency to Codex.


## Spotify connection

Open **Connectors → Spotify → Connect Spotify**. Spotify opens in an
in-app Safari view; ncspot is the application name on its consent screen. The
phone receives Spotify's loopback callback and forwards the authorization code
to the encrypted Nanocodex broker. Agents use the resulting account connection
without access to credentials. Disconnect is available beside each linked account.
No desktop helper or pasted callback URL is required. The listener binds only
127.0.0.1:8989 and is short-lived; if another connection attempt occupies that
port, close it and retry. Keep Nanocodex open until authorization completes.

The focused `SpotifyLoopbackTests` exercise the actual socket and callback
validation. `SpotifyLoopbackUITests` exercises Safari returning to that listener
on iOS with a nonsecret fixture; real Spotify authorization is a separate live gate.


SoundCloud uses the same foreground phone flow with its own fixed listener at
`http://127.0.0.1:8788/callback`. **Connect SoundCloud** is available under
Connectors (also linked from Account Settings); `nanocodex://connect/soundcloud`
opens that connector in the list. The app receives only the code and state. App credentials,
PKCE, tokens, and refresh remain in the broker. SoundCloud app registration is a
one-time deployment setup; ordinary users authorize their own accounts on the phone.

## Native touch gamepad

Open a controllable live screen and choose its game controls. Linux Wayland
hosts that advertise a working virtual gamepad show two analog sticks, a D-pad,
ABXY buttons, shoulders, triggers, stick clicks, Back and Start. Other hosts keep
the keyboard/mouse controls. The phone sends complete controller snapshots over
the ordered input channel, including a 30 Hz heartbeat while an input is held.
Stop, leaving controls, losing control, and backgrounding release held inputs;
foregrounding requires taking control again.

The Linux screen publisher must explicitly enable `NANOCODEX_VIRTUAL_GAMEPAD=1`
and have access to `/dev/uinput`. It creates a virtual Xbox-compatible controller
and advertises support only after device creation succeeds. Grant access to the
publisher's user narrowly; do not make the device world-writable. If input stops
arriving for 500 ms, the host releases the virtual controls. Games still need to
support the resulting Linux/Wine controller device; showing controller-themed
UI alone does not establish that a game receives its input.

The loopback fixture in `NanocodexInboxUITests/fixtures/remote-screen.mjs`
records synthetic input without injecting it into the OS. Start it before
running `RemoteScreenLifecycleUITests` with
`TEST_RUNNER_NANOCODEX_SCREEN_FIXTURE=1`. Its native gamepad test checks touch
states, neutral release, Stop, exit and background recovery; live game input
requires a separate test on the configured desktop.

The native controller is designed for landscape play. Its movement/camera thumb
zones stay near the edges on larger displays, leaving the middle of the stream
clear. iOS 26 uses native Liquid Glass in a shared effect container; older iOS
versions use system material. Reduce Transparency uses opaque controls, and
stick updates do not inherit layout animations. WoW hints describe default
bindings, not a live reading of the game's configuration. The LT/RT indicator
tracks the selected crossbar; spell assignments and customized bindings remain
owned by the game. All controls keep at least 44-point touch targets.
The default hints were checked against WoW Forever 1.60.1.69913's exported
[face-button handlers](https://github.com/Gethe/wow-ui-source/blob/70ef1b2fd78061a73f886c4a1e79dc5b5cff6d5e/Interface/AddOns/Blizzard_GamepadActionBars/MainActionBarFrame.lua#L287-L409),
[modifier legend](https://github.com/Gethe/wow-ui-source/blob/70ef1b2fd78061a73f886c4a1e79dc5b5cff6d5e/Interface/AddOns/Blizzard_Gamepad/UI/PersistentInputLegend/GamepadPersistentInputLegend.lua#L326-L403),
and [crossbar selection](https://github.com/Gethe/wow-ui-source/blob/70ef1b2fd78061a73f886c4a1e79dc5b5cff6d5e/Interface/AddOns/Blizzard_GamepadActionBars/PageUnit.lua#L566-L617).
Shoulder targeting, shoulder swapping/toggle settings, HUD focus and user
rebinding can change the displayed default roles; the overlay always sends
physical gamepad input and lets WoW resolve those settings.
