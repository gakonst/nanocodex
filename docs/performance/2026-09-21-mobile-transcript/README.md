# Native virtualized mobile transcripts

Long chats now use a UICollectionView with self-sizing UIHostingConfiguration
cells instead of an eager SwiftUI transcript stack. Stable row IDs and per-row
presentation revisions let the diffable data source reconfigure changed content
while UIKit recycles offscreen hosts. Activity groups are split into native rows,
so expanding a large code-mode batch does not construct every child in one host.

The native coordinator retains a visible row and its viewport point offset across
updates, self-sizing, and viewport changes. Following the live tail is separate
from reading earlier output. Explicit navigation retains its target until direct
interaction; a no-op animated request immediately returns to idle. Disclosure
state lives with the conversation, independently of recycled cell state.

Only realized cells report geometry. Native visibility drives generated-media
loading, including cells entering the viewport after reuse. Photo galleries keep
the local-original preview path, square multi-photo layout, and single-image
aspect ratio from #468. The composer remains outside the recycled transcript.

Raw scroll metrics and reading positions live in non-observable reference storage.
User-message positions are indexed by the background projection for logarithmic
navigation lookup. History retention is updated when visible semantic row IDs or
history revisions change. Ordinary pixel offsets do not publish a full row-frame
map or write raw metrics into SwiftUI state.

Retained-row comparison, media preparation and card application run together off
MainActor. Publication rejects changed row/card inputs and unchanged rows do not
invalidate the transcript projection. Sidebar presentation uses lightweight
snapshots and drawer gestures retain their initial direction.

## Reproduction

The app's UI tests use synthetic data. `NANOCODEX_DEMO_RENDER_ROWS` selects up to
2,000 Markdown messages; `NANOCODEX_RENDER_COUNTER=1` enables the native hosted-cell
counter. It counts weakly tracked cell objects with hosting configurations,
including configurations held offscreen by UIKit, rather than just visible cells.

- `testNativeTranscriptBoundsMountedCellsFor500Rows` and
  `testNativeTranscriptBoundsMountedCellsFor2000Rows` require the same ceiling of
  64 retained hosts while scrolling and returning to the tail.
- `testNativeTranscriptPreservesExpandedToolAfterCellRecycling` requires the tool
  to leave the accessibility tree, then return expanded after jumping to latest.
- Existing streaming, pagination, drawer, keyboard, tool expansion, generated-media
  and local-photo journeys exercise the native collection view.
- `python3 apple/Tests/InboxModelTests/test_row_geometry.py` validates the geometry
  index with variable-height rows and viewport boundaries.
- `swift test -c release -j 2 --package-path apple/InboxCore --filter
  TranscriptPublication` checks publication preparation and stale-input rejection.

## Measurement scope

The original warm 8,000-event publication microbenchmark measured median main-actor
preparation of 4.239 ms before versus 0.0129 ms after, with 4.060 ms worker time
(40 iterations on a shared Apple Silicon Mac). This measures one preparation
stage, excluding UI rendering, scheduling and frame presentation; it is not an
on-device FPS claim.

Native hosting is bounded by the working set, but the retained data source and
projection metadata still scale with loaded history. Projection/reconciliation
can scan that history when its revision changes. A single very large message or
individual tool result is still one self-sizing cell. This change does not claim
constant total memory, physical-device frame-time improvements, or deployment.

## Validation on September 21, 2026

Eighteen distinct focused UI scenarios passed on iOS 18.2 across the validation
runs: 500/2,000-row hosting bounds, recycled disclosure state, code-mode grouping,
global tool expansion, drawer anchors, user-message navigation across history,
keyboard/send anchors, conversation switching, delayed history/media insertion,
foreground restoration, generated media, three/four-photo galleries,
portrait/panorama sizing, live-tail following and same-response streaming anchors.
The final changed self-sizing path was rechecked against 2,000 rows, delayed
history insertion and live-tail following.

The streaming test holds a paragraph in view while the same message grows for a
minute. On iOS 18.2 the recorded cell height grew from 1,655.3 to 4,688.7 points,
while content offset stayed at 946.3 points and the paragraph remained within the
four-point position tolerance. Explicit native self-sizing invalidation is needed
for asynchronous Markdown growth; preserving only the cell ID is insufficient.

The geometry executable passed. Both functional publication-preparation tests
passed; the opt-in microbenchmark was skipped in this validation run. Changed
files also passed `typos` and `git diff --check`.

Seven distinct focused scenarios also passed on iOS 26.5: drawer anchors,
2,000-row hosting bounds, portrait and four-photo layouts, repeated global tool
expansion, delayed history insertion and same-response streaming anchors. The
final self-sizing change was rechecked with the four-photo gallery and streaming
anchor test on that runtime.
