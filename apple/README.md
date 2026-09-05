# Nanocodex Inbox

A new native SwiftUI app for iPhone, iPad, and Mac. One managed agent per card:
review the latest update, steer its current turn, then move to the next agent.
The interface uses ChatGPT-style neutral surfaces, native typography, a rounded
composer, and right-aligned user message bubbles, with Nanocodex branding and
the agent card deck, review actions, and live steering controls.
The visual and durable-event vocabulary follows the existing macOS client in
PR #256. This app connects directly to the current managed HTTP/SSE contract;
it does not require that PR or a bundled Node runtime.

## Run

Open `apple/NanocodexInbox.xcodeproj`, select the `NanocodexInbox` scheme, then
choose an iPhone simulator or My Mac. Requires iOS 17 / macOS 14 or later.
Choose a development team in Signing & Capabilities to run on a physical device.
There are no third-party dependencies. Local package `InboxCore` owns the native
protocol adapter, event projection, cursor ordering, and inbox policy.

Connect using your Nanocodex account API key and the HTTPS origin of the managed
service. The key is stored in Keychain; drafts and seen positions are stored on
this device, scoped to that connection. Phone sign-in from the separate desktop
runtime is not yet ported. **Explore the demo** (or the `--demo` launch argument)
opens explicitly labeled sample agents with simulated actions and no network.

## Interaction

| Action | Result |
| --- | --- |
| Swipe left | Move the agent to the back of the deck |
| Swipe right | Mark this update seen and advance |
| Long-press card → Previous agent | Return to the previous agent |
| Tap card | Read messages, reasoning, and expandable tool details |
| Send / ⌘Return | Submit one durable follow-up; queue behind current work |
| Steer now on queued message | Cancel its captured predecessor so the follow-up can start |
| Mic | Dictate into an agent-scoped draft; slide the sheet down to keep it |
| Stop turn | Confirm cancellation of the selected turn |
| Stack button | Jump directly to any agent or create one |

The inbox prioritizes completed/failed updates that have not been seen. Running
agents remain available in the inbox. Live changes preserve the focused card;
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
“Steer now” uses cancel-and-continue: it cancels the captured predecessor without
resubmitting the already durable follow-up. This is not in-flight runtime injection.
Cancellation acknowledgement is not completion; the row stays until execution or
a terminal event. Failed sends retain input and retry the same identity.

Voice uses Apple Speech and the microphone, with permission requested on first use.
A partial transcript becomes an editable draft, never an automatic send. Dismissal,
backgrounding, and errors stop audio capture. Existing typed text is preserved.
Demo voice uses a labeled transcript fixture; real microphone and recognition
permissions require physical-device validation.

The live event working set is capped at 512 events / 16 MiB. Earlier history is
loaded explicitly, up to 2,048 events. Full history remains on the service.
The transcript preserves manual scroll position; it does not force-scroll on
every token. No local Hands or new execution environment are started here.

## Validation

```sh
swift test --package-path apple/InboxCore
xcodebuild -project apple/NanocodexInbox.xcodeproj -scheme NanocodexInbox -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO build
xcodebuild -project apple/NanocodexInbox.xcodeproj -scheme NanocodexInbox -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=NO build
```

The `Apple Inbox` workflow runs protocol/policy tests, compiles both platforms,
and drives native iPhone demo journeys covering swipes, per-agent drafts, queue
recovery, cancel failures, repeated taps, relaunch, thread continuity, and voice
sheet dismissal/error handling. It attaches screenshots, simulator video, and the
full Xcode result as `native-inbox-evidence`. Demo automation does not establish
authenticated service behavior, real microphone recognition, or physical-device
performance. Animation durations are 160–180 ms and respect Reduce Motion.

Tool activity uses readable action names and statuses, with expandable labeled inputs and results. Commands retain code formatting; nested objects and arrays become labeled fields rather than JSON. Unknown tools use the same presentation. Agent replies have no redundant speaker label; reasoning is collapsed behind Thought process.

Conversation scroll targets retain the visible message across prepended history and new output, and new conversations open at the latest messages. Returning to the foreground resumes the existing cursor and transcript rather than clearing the screen. Card drags lock their direction so vertical reading does not become a horizontal swipe; card changes do not crossfade.

The recorded demo suite additionally exercises long-thread reading during new output and foregrounding, older-history pagination, vertical card scrolling, repeated fast swipes, new-agent creation and stop confirmation, an empty Running filter, invalid account input, a multi-message queue with the keyboard open, and stopping/reviewing voice input. The full simulator recording contains every test, including failure/retry paths and relaunches. Demo agents and injected failures are fixtures; this does not validate an authenticated service or physical microphone.
