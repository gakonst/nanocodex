# Chat UI architecture

Use native containers and one owner for each kind of state. This screen has long,
variable-height agent output, streamed Markdown, independently expandable tools,
history pagination, and saved reading positions inside tall rows.

## Choices considered

| Approach | Useful capabilities | Decision |
| --- | --- | --- |
| SwiftUI ScrollView + LazyVStack | ScrollPosition, anchor roles, scroll-phase/geometry observation | Good native option, but ID/edge targeting is not an exact row-plus-point restoration contract for this transcript. A rewrite would still need measured acceptance tests. |
| SwiftUI List | Native reuse, selection, search, navigation and accessibility | Use for the conversation sidebar. No demonstrated advantage for exact transcript restoration. |
| UITableView + UIHostingConfiguration | Reusable one-column self-sizing rows | Viable; changing containers alone would not remove streaming/prepend/reading-intent coordination. |
| UICollectionView + UIHostingConfiguration | Stable diffable identity, cell reuse, observed SwiftUI content and native self-sizing | Retain the current foundation; remove duplicate update and layout machinery. |
| ChatLayout | Collection-view layout with position snapshots and bottom-preserving batch updates | Relevant alternative, not a complete chat/keyboard/rendering system. No dependency added without comparative runtime evidence. |
| Full chat SDK (MessageKit/Stream) | Established message/composer components | Useful references. Their message models and broader SDK behavior are not a drop-in replacement for tool/agent output. |

Sources: [Apple hosting SwiftUI in UIKit](https://developer.apple.com/videos/play/wwdc2022/10072/),
[modern collection views](https://developer.apple.com/documentation/uikit/implementing-modern-collection-views),
[ScrollPosition](https://developer.apple.com/documentation/swiftui/scrollposition),
[scroll anchor roles](https://developer.apple.com/documentation/swiftui/scrollanchorrole),
[NavigationSplitView](https://developer.apple.com/documentation/swiftui/navigationsplitview),
[ChatLayout](https://github.com/ekazaev/ChatLayout),
[Stream's message list](https://github.com/GetStream/stream-chat-swift/blob/develop/Sources/StreamChatUI/ChatMessageList/ChatMessageListVC.swift).

## Ownership

- NavigationSplitView, List and system toolbars own navigation, gestures, selection
  presentation, search and compact/regular adaptation. The model owns conversation
  identity and drafts; saved reading positions survive detail recreation.
- A bottom safeAreaInset owns composer placement. Native adjusted insets account
  for safe areas. Do not add a second composer-height or keyboard-offset observer.
- The diffable data source owns row identity and structural insertion/removal.
  Observable row content updates UIHostingConfiguration directly. Do not rebuild
  snapshots or invalidate every estimated height for each text delta. Tool activities
  carry individual content revisions so completing one tool does not update every
  sibling card in its turn. Pure live-tail appends request one native scroll
  animation; history insertion and reader gestures do not.
- The native transcript coordinator owns offsets, user-scroll phases and layout
  retention. The SwiftUI screen requests a stable row and point offset once; it
  must not calculate fractional anchors and repeatedly correct geometry. Persisted
  reading positions come from user scrolling (including the final deceleration
  report) or explicit reading/follow actions, not passive keyboard or navigation
  layout changes.
- DisclosureGroup owns tool disclosure interaction. Conversation-owned bindings
  retain expansion across cell recycling and navigation. Large field values and
  field counts are bounded before inline text measurement; the native source viewer
  retains complete content.
- Viewport bookkeeping receives only realized cell frames in viewport coordinates.
  It is not another full-history layout engine.

## Intentional application policy

Following new output and reading earlier history are different intents. Pagination
and explicit user-message navigation remain app policy. Exact restoration uses a
stable row identity plus a point displacement; text reflow after changing width or
Dynamic Type may change which text line occupies that point.

Markdown parsing, bounded caches, incremental stream projection, asynchronous
preparation and generation guards remain necessary. Bare Text is not a substitute
for block Markdown, tables and code, and cancel/restart on every token can starve
streamed rendering. Network retry and admission semantics are outside this UI
cleanup.

## Validation

Exercise long-history reuse and upward scrolling, streaming while following versus
reading, prepend/trim, tool expansion and recycling, complete output access,
composer/keyboard changes, and navigation with independent drafts. Simulator
functional tests do not establish physical-device frame times. The extreme
10,000-line full-source fixture incurred slow XCTest accessibility queries; the
functional full-source test uses an over-limit 200-line result and preview unit
tests retain extreme-input coverage. No visible-render latency claim follows from
those accessibility timings.

## Recorded stress run

Run `apple/scripts/record-chat-stress.sh SIMULATOR_UDID DERIVED_DATA OUTPUT_DIRECTORY`
with Xcode and the voice XCFramework built. Use a fresh output directory. The
script builds the UI tests, asks XCTest to retain successful-test recordings and
screenshots, then runs three repetitions of each scenario without failure retries:

- 2,000 variable-height Markdown rows: twelve upward-history gestures, strictly
  earlier visible reading positions, at most 64 retained native cell hosts at each checkpoint,
  and a return to the live tail. The log includes actual host counts and offsets.
- Expanded tool recycling: remove the tool from the realized accessibility tree,
  then return and require its expanded output to remain visible.
- 200 tool calls at a configured 50 ms interval, each running then completing
  under the same identity: require the final completed tool to be visible and
  observe no latest-message jump button during the burst and settling window.

The output contains `tests.log`, `stress.xcresult`, and exported attachments. The
interval defines fixed producer deadlines; overdue work can catch up instead of
accumulating relative sleep delay. Completion updates a row atomically. Producer
progress is logged, but the configured interval is not a measured throughput or
network rate. Host counts and accessibility assertions are sampled checkpoints;
simulator recordings are visual review evidence, not physical-device frame-time
benchmarks.
