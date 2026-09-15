# Desktop cleanup: measured iterations

The desktop now has one Layout menu, a compact navigation footer, and a simpler
new-conversation screen. Duplicate split buttons, the repeated New split footer,
starter chips, filler copy, and the single-conversation pane header were removed.
Split layouts keep their handles and per-pane controls. Review, rename, focus,
and split commands remain available, including the restored keyboard shortcuts.

The tab strip uses a lazy stack. A value-based rendering boundary keeps draft
and transcript updates out of unchanged tabs' controls, material, hover, and
accessibility trees. Workspace grouping builds one leaf lookup per projection;
Inbox sorting builds ranks once instead of searching arrays in every comparison.

## Measurement loop

Measured on the same Apple Silicon Mac, macOS 26.3.1, using the native Debug test
host. The 120-tab fixture selects twelve distant conversations and edits each
through its real AppKit composer, checking the selected editor and saved draft.
Times include synchronous selection/editing, layout, and drawing. The settling
interval is excluded. These are local UI-work measurements, not network latency
or release-build frame-rate claims.

| Iteration | 120-tab switch median | Edit/layout median |
| --- | ---: | ---: |
| Baseline | 206.2 ms | 99.5 ms |
| Lazy tabs and control cleanup | 158.8 ms | 72.6 ms |
| Unchanged tab rendering isolated | 116.3 ms | 24.8 ms |
| Final, including single-pane cleanup | 93.0 ms | 24.6 ms |

The final run reduced these medians by 55% and 75%. The 120-tab switch p95 moved
from 283.2 to 143.5 ms; edit/layout p95 moved from 103.6 to 26.6 ms.

The existing small-workspace fixture moved from 24.3 to 17.7 ms median tab work,
21.7 to 8.5 ms for its editor input sample, and 200.0 to 109.6 ms for its first
native layout. Protocol snapshot processing was approximately unchanged
(55.3 versus 57.0 ms for the long-stream median); this pass targeted UI work.

Raw measurements are under `macos/build/evidence/`:

- `native-many-tabs-cleanup-{before,after,second,final}.json`
- `native-performance-cleanup-{before,after,second,final}.json`
- `native-chat-cleanup-final.png` and `native-transcript-cleanup-final.png`
- `native-screen-pane.png` and `native-screen-pane-narrow.png`

## Validation

The final native protocol suite ran 49 tests: 48 passed, with only the live
account journey skipped. It includes retained editors and reading positions,
queues, history navigation, grouped tabs, mixed splits, drag/resize behavior,
rapid Escape/v/h, writing immediately after navigation, and the local screen
transport fixture. No account credentials or live agent turns are required.

Reproduce the UI benchmarks with `TEST_RUNNER_NANOCODEX_PERFORMANCE_PHASE` set to
a new label and run `ProtocolTests/testManyTabsKeepNavigationAndDraftsResponsive`
and `ProtocolTests/testNativeWorkspaceRenderingAndInteractionLatency` using the
Nanocodex Xcode scheme. Run the screen fixture as documented in
`apple/NanocodexInboxUITests/fixtures/README.md` for the screen journey.
