# Mobile open-source dependencies

These integrations use free, open-source libraries without a paid SDK or service requirement. Versions are pinned in the Xcode project, `InboxCore/Package.swift`, `NanocodexUI/Package.swift`, and `NanocodexConnectEmbed/Package.swift`; the corresponding `Package.resolved` files record exact revisions.

| Library | Version | Use | License |
| --- | --- | --- | --- |
| [ChatLayout](https://github.com/ekazaev/ChatLayout) | 2.5.2 | Self-sizing chat timeline and viewport restoration | MIT |
| [MarkdownUI](https://github.com/gonzalezreal/swift-markdown-ui) | 2.4.1 | SwiftUI Markdown rendering | MIT |
| [Nuke](https://github.com/kean/Nuke) | 13.2.0 | Image loading and caching | MIT |
| [GRDB](https://github.com/groue/GRDB.swift) | 7.8.0 | SQLite-backed mobile outbox | MIT, with bundled notices |
| [DSWaveformImage](https://github.com/dmrschmidt/DSWaveformImage) | 14.5.0 | Audio waveform rendering | MIT |

MarkdownUI is a maintenance dependency; keep its renderer behind the app's adapter and review upstream maintenance before upgrading.

## Mobile conversation timeline

[`NanocodexConnectEmbed`](NanocodexConnectEmbed/README.md) owns the public
`EmbedTranscript` surface and its native adapter. It adapts host-owned stable row identities to ChatLayout's
`ChatLayoutDiffableDataSource` and `CollectionViewChatLayout`. ChatLayout owns row geometry,
self-sizing invalidation, batch-update anchoring, and estimated-to-measured position
restoration. The adapter retains conversation-specific follow/read intent, tab restoration,
visibility reporting, and native SwiftUI hosting. Hosted content is measured at the proposed
full width; estimated height must not constrain Markdown, tables, or expanded tool output.

MarkdownUI renders the streamed response through the coalesced parser adapter. Tool cards
remain native app views in the same collection, preserving progress, disclosure state,
results, and media. They do not need a second message list or a vendor chat backend.

ChatLayout's automatic self-sizing invalidation is documented upstream as experimental.
The app exercises it with changing SwiftUI content in the targeted timeline UI journeys;
package upgrades must retain those checks. UI recordings are exported as CI artifacts.

## Transitive dependencies and notices

MarkdownUI's runtime products use NetworkImage 6.0.1 (MIT) and swift-cmark 0.9.0 (`cmark-gfm` and `cmark-gfm-extensions`). cmark includes BSD and MIT component notices; its complete `COPYING` also describes separately licensed test/specification material that is not part of those runtime targets. MarkdownUI's SnapshotTesting dependency is for upstream tests, not the application product.


GRDB's standard SwiftPM product uses the platform SQLite library through `GRDBSQLite`, not its optional custom SQLite source tree. GRDB includes MIT inflection-rule notices and Swift-derived code under Apache-2.0 with Runtime Library Exception. Nuke and DSWaveformImage declare no external runtime package dependencies.

[Full third-party notices](MOBILE_DEPENDENCY_NOTICES.md) preserve the checked-out copyright, permission, and disclaimer texts, including bundled components. The app bundles this notices file for its Settings licenses link. Preserve it in source distributions and materials accompanying redistributed binaries. Refresh notices when changing package versions.

## Setup

Use Xcode 26.2 / Swift 6.2 for these pins. Follow the app's deployment target and signing instructions in [README.md](README.md#run). Open `apple/NanocodexInbox.xcodeproj` and select the `NanocodexInbox` scheme. Swift Package Manager resolves the linked products; GRDB is also declared by the local InboxCore package.

From the repository root, resolve the app packages through the shared-machine Xcode guard:

```sh
scripts/xcodebuild-guard.sh -resolvePackageDependencies \
  -project apple/NanocodexInbox.xcodeproj -scheme NanocodexInbox
```

Keep the committed resolution files with dependency updates so transitive versions remain reproducible. These libraries do not require license keys or paid accounts.

## Chat performance profiling

Run `CHAT_PROFILE_DEVICE=<existing-simulator-UDID> apple/scripts/profile-chat.sh`.
The guarded build and opt-in XCTest profiles use synthetic data and retain three
samples each after XCTest warm-up for 500-message scrolling, typing with a Markdown transcript, and
streaming Markdown with expanded tool progress. CPU and memory metrics measure the
app process; scrolling/streaming also record clock and hitch metrics. The streaming
profile measures sequential windows in one process as the document grows, while
toggling the tool disclosure and returning to the tail; samples are not identical
replays. The fixture streams for 60 seconds; later navigation windows can include
the completed response. Launch is excluded. The scrolling
profile checks that native mounted hosts remain bounded independently of 500 rows.

The script exports raw metric CSVs, videos, and XCTest results under ignored
`output/`; publish these as artifacts and summarize measurements in the PR.
Clock measurements include XCTest driver waits and the fixture's streaming delays.
Simulator results depend on host load and are not real-device FPS, memory limits,
or proof of improvement over a baseline. Debug instrumentation and recording add
cost. Some simulator runtimes omit hitch measurements; missing output is not zero
hitches. Use identical hardware/build/workloads for any before/after comparison.
