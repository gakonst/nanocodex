# Nanocodex Connect Embed for Apple apps

`NanocodexConnectEmbed` is a composable SwiftUI presentation SDK for **iOS 17+
and macOS 14+**. `EmbedConversation` assembles a transcript, floating transcript
controls, accessories, and a composer from host-supplied native views. It adds no
colors, typography, padding, buttons, network connection, or message submission.
It requires no React, JavaScript runtime, or web view.

The host owns conversation state, Connect authorization, HTTP/SSE, submission,
delivery receipts, attachments, tool decisions, and navigation. Supply rows for
messages, tools, approvals, failures, retry controls, and any other content using
the same public row contract. No second transport or state reducer is introduced.

## Add the package

In Xcode, add `https://github.com/gakonst/nanocodex.git` and select the
`NanocodexConnectEmbed` product. For SwiftPM, pin a repository revision that
contains the root `Package.swift` and the API below:

```swift
.package(url: "https://github.com/gakonst/nanocodex.git", revision: "<full-commit-sha>")
// In the consuming target's dependencies:
.product(name: "NanocodexConnectEmbed", package: "nanocodex")
```

The repository root is the remote SwiftPM entry point. A GitHub subdirectory URL
such as `.../tree/master/apple/NanocodexConnectEmbed` is not a package URL. There
is no separately versioned SDK release tag; use an available commit or branch,
and pin a commit for reproducible builds. Access to the GitHub repository is
required; Connect authorization does not grant source access. The package pulls
its declared rendering and remote-screen dependencies, including WebRTC, even
when a consumer only uses conversation views. It is an Apple-platform package,
not a Linux SwiftUI implementation.

For a source checkout alongside the host app:

```swift
.package(path: "../nanocodex/apple/NanocodexConnectEmbed")
// In the consuming target's dependencies:
.product(name: "NanocodexConnectEmbed", package: "NanocodexConnectEmbed")
```

The local package needs the sibling `NanocodexUI`, `NanocodexRemote`, and
`InboxCore` directories. Both entry points compile the same source files. Use
Swift 6 tooling; see [mobile dependencies](../MOBILE_DEPENDENCIES.md). Installation
alone does not supply Connect credentials or authorize account APIs.

## Compose a conversation

This example works on both supported platforms. The host supplies its admission
policy and submission action. `TextField` can be replaced with a custom composer;
on iOS, `EmbedComposerEditor` exposes the existing native multiline editor.

```swift
import SwiftUI
import NanocodexConnectEmbed

struct EmbedMessage: Identifiable {
    let id: String
    let revision: Int
    let text: String
}

@MainActor
struct EmbeddedConversation: View {
    let conversationID: String
    let messages: [EmbedMessage]
    let canSend: Bool
    let onSend: (String) -> Void
    @State private var followsLatest = true
    @State private var draft = ""

    var body: some View {
        EmbedConversation(
            conversationID: conversationID,
            rows: messages.map { message in
                EmbedConversationRow(id: message.id, revision: message.revision) {
                    EmbedMarkdown(text: message.text, compact: true)
                }
            },
            followsLatest: $followsLatest,
            layout: .init(horizontalPadding: 16, rowSpacing: 12),
            transcriptOverlay: {
                if !followsLatest {
                    Button("Latest") { followsLatest = true }
                        .padding(16)
                        .frame(maxWidth: .infinity, alignment: .trailing)
                }
            },
            accessories: {
                // Host-owned status, attachments, approvals, or screen dock.
                if !canSend { Text("Sending unavailable").font(.caption) }
            },
            composer: {
                HStack(alignment: .bottom) {
                    TextField("Message", text: $draft, axis: .vertical)
                    Button("Send") { onSend(draft) }
                        .disabled(!canSend || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }.padding()
            }
        )
    }
}
```

`transcriptOverlay` and `accessories` are optional. The minimal form is
`EmbedConversation(conversationID:rows:followsLatest:) { /* composer */ }`.
The overlay is aligned to the bottom of the transcript, above the composer;
its builder controls horizontal placement. Accessories appear below the
transcript and above the composer. All slots preserve host styling.

## Identity, scrolling, and lifecycle

IDs must be unique within a conversation. Change a row's `revision` for every
rendering input, including streamed text, tool disclosure inputs, and authorization
changes. Keep the row ID stable: changing a revision updates content without
resetting local row state. `EmbedTranscript.Row` remains an alias of
`EmbedConversationRow` for existing low-level consumers. Mark header/loading/
status rows `countsAsMessage: false`; they are excluded from debug message counts
and the portable fallback's message-anchor selection.

`conversationID` scopes the entire child view tree, including transcript,
overlay, accessories, and composer. Changing it discards child-local state even
when the new conversation reuses row IDs. Host-owned bindings are preserved:
the host decides whether to retain a draft and should set `followsLatest = true`
when opening a thread at its tail. Keep that binding per conversation if restoring
reading intent. Removal releases the scroll engine and cancels child SwiftUI
`.task` work; native deferred scroll reports are invalidated. The SDK never starts,
retries, or closes the host's transport. Attach transport work to a host-owned
`.task(id: conversationID)` and implement cancellation in that work.

A vertical reading gesture suspends following. Setting `followsLatest = true`
returns to the tail; scrolling back within 24 points of the tail resumes it.
New rows and revised streaming rows then follow automatically.

`rendering: .automatic` selects the existing virtualized UIKit transcript on
iOS 18+, and a SwiftUI lazy scroll view on iOS 17 and macOS 14. Use `.scrollView`
to select the portable path on iOS 18 as well. Both preserve stable row identity
and a reading message on history prepend. The UIKit engine restores exact pixel
offsets through history and self-sizing; the portable path restores a message at
the top when rows are prepended and can lose its intra-row offset. It does not
provide the UIKit engine's exact geometry callbacks or bounded-cell guarantees.

Read `@Environment(\.embedTranscriptVisible)` for expensive row media. Native
cells report actual display visibility; the portable lazy stack reports SwiftUI
appearance, which may include prefetched rows. Pass it as `loadsThumbnail` to
`EmbedImageAttachment` or `EmbedGeneratedOutputView`. These aliases retain their
`NanocodexUI` initializers, URL handlers, image-paste callbacks, accessibility
identifiers, and native media previews.

## Advanced native transcript

The Nanocodex iPhone/iPad app uses the same package's low-level `EmbedTranscript`
and composer components to preserve its floating chrome and detailed history
controls. `EmbedTranscript` remains iOS 18+ because it exposes `ScrollPhase`.
It accepts `[EmbedConversationRow]`, an `EmbedScrollProxy`, `followsLatest`,
`EmbedTranscriptLayout`, `topInset` / `bottomInset`, and `onFrames` / `onMetrics` /
`onPhase` callbacks. Keep a separate proxy and view identity per conversation.

`EmbedTranscriptLayout` defaults to no padding, spacing, or maximum width.
Insets reserve space for floating header/composer views. `onFrames` reports
realized visible rows relative to the usable top edge;
`EmbedScrollProxy.scrollTo(_:topOffset:)` restores that same coordinate.
`onMetrics` exposes native offset, content size, viewport, and adjusted insets.
`EmbedComposerEditor` is iOS-only; macOS hosts supply a native editor.

## Screens

`EmbedLatestScreen` uses the existing `ChatLatestScreen` API. Its optional
`onWatchLive` callback lets the host opt in to a live viewer. `EmbedLiveScreen`
accepts a conversation identity, an `EmbedRemoteService`, bindings for
`EmbedScreenSelection?` and expansion, and `onClose` / `onControls` callbacks.
Changing conversation identity destroys the old viewer; expanding the same
conversation preserves its connection. The viewer suspends when inactive and
closes when removed.

Live screens use the existing `NanocodexRemote` account hand transport at
`/v1/account/hands`. A Connect grant alone does not authorize those routes.
Include the live component only when the host already has an authorized
`RemoteService`; authentication stays in that service's private request closure.
The embed is view-only; separately authorized interactive controls remain the
host's responsibility. Closing a viewer does not close a shared `RemoteService`.

## Validation

The app's SDK fixture consumes the public API with deterministic host updates,
without a network substitute. Native and forced-portable UI journeys exercise
streaming row-state retention, identity changes, draft ownership, admission,
explicit send, unmount/remount, prepend reading anchors, and return-to-tail follow.
Run them on an existing simulator through the shared-machine guard:

```sh
scripts/xcodebuild-guard.sh test \
  -project apple/NanocodexInbox.xcodeproj -scheme NanocodexInbox \
  -destination "platform=iOS Simulator,id=$SIMULATOR_UDID" \
  -only-testing:NanocodexInboxUITests/InboxUITests/testEmbedConversationNativeJourney \
  -only-testing:NanocodexInboxUITests/InboxUITests/testEmbedConversationPortableJourney \
  -only-testing:NanocodexInboxUITests/InboxUITests/testEmbedConversationNativeHistoryJourney \
  -only-testing:NanocodexInboxUITests/InboxUITests/testEmbedConversationPortableHistoryJourney \
  -resultBundlePath output/connect-embed-native.xcresult
```

Screenshots are attached to the XCTest result bundle. Existing app journeys also
cover streamed Markdown/tools, bounded mounted cells, composer draft retention,
and screen lifecycle. The Swift Connect SDK CI builds the public package and a
consumer; compilation alone is not UI-journey evidence.

On Linux, `python3 apple/scripts/prepare-xtool.py --configuration debug`
(with Pillow installed) checks the Xcode inputs and generates the real xtool
package graph. `python3 apple/scripts/test-ios-linux.py` covers packaging,
missing-package failure, and staging recovery. `build-ios-linux.sh` consumes this
graph through `apple/Package.swift`. Staging is not Swift compilation, simulator
execution, or signing; UI journeys require an Apple toolchain and runtime.
