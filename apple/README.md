# Nanocodex Inbox

A new native SwiftUI app for iPhone, iPad, and Mac. One managed agent per card:
review the latest update, steer its current turn, then move to the next agent.
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
| Swipe left / Later / ⌘→ | Move the agent to the back of the deck |
| Swipe right / Seen / ⌘D | Mark this update seen and advance |
| Back / ⌘[ | Return to the previous agent |
| Open thread / ⌘O | Read messages, reasoning, and expandable tool details |
| Steer live / ⌘Return | Send direction to the displayed active turn |
| Follow-up | Submit a new durable turn, including while another is running |
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
and idempotency key. Steering is never silently changed into a new turn.

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
and drives the iPhone demo journey: preserve a draft while cycling agents, send,
filter running agents, open the thread, and swipe. The test result includes a
native screenshot. Demo automation does not establish authenticated live-service
or on-device behavior; those require an account and an Apple device/simulator.

Local verification on 2026-09-05: the core typechecks with Swift 6.0.3; all ten
protocol/policy XCTest cases pass when invoked directly with the Swift frontend.
The native UI and UI-test sources pass Swift syntax parsing, and the project
source references, shared scheme XML, and workflow YAML pass structural checks.
Swift Package Manager crashes in this Linux host's process monitoring, so the
same committed XCTest methods were invoked directly. Xcode builds, simulator
interaction, and authenticated live-service verification remain pending. The
branch push was blocked by automatic approval review; CI has not run yet.
