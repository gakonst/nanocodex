# Native desktop and Liquid Glass pass

The desktop interface uses SwiftUI and AppKit throughout. Conversations now have
browser-style horizontal tabs or a native vertical sidebar, switched from the
window toolbar and persisted with the workspace. The sidebar provides native
selection and keyboard navigation. Tabs support closing, reordering, grouped
panes, and creating a separate tab by dropping a pane onto the tab area.

The Inbox / Running / All strip is removed. New workspaces show All; filtering
remains available in the toolbar layout menu and keyboard commands. Window
actions use the native toolbar and SF Symbols. Selected horizontal tabs and
action buttons use system glass; pane headers use an opaque surface. Transcripts
and streamed screens remain clear content.

Settings uses native grouped forms for General, Appearance, and Shortcuts,
including tab orientation and zoom. Hands uses grouped forms and native actions.
Screen controls retain navigation, control/release, and reconnect actions. The
v/h splits, pane navigation, drag, resizing, browser history, tab overview, and
screen-pane workflows remain available.

macOS 26 uses system Liquid Glass; macOS 14–15 retain native material and bordered
controls. Custom glass becomes opaque with Reduce Transparency, and motion
respects Reduce Motion. The interface does not embed a web view or Electron
renderer. The agent runtime still uses the bundled Node process in
`js/desktop-runtime`; it has not been rewritten in Swift.

## Research and implementation guidance

System components, semantic colors, and glass confined to controls follow
Apple's guidance:

- [Adopting Liquid Glass](https://developer.apple.com/documentation/TechnologyOverviews/adopting-liquid-glass)
- [Applying Liquid Glass to custom views](https://developer.apple.com/documentation/SwiftUI/Applying-Liquid-Glass-to-custom-views)
- [Build an AppKit app with the new design](https://developer.apple.com/videos/play/wwdc2025/310/)

Apple documents that a navigation detail can draw beyond its safe area beneath
the sidebar (141222137). The workspace therefore needs an explicit native
hosting boundary inside the measured detail area.
[macOS Tahoe 26 release notes](https://developer.apple.com/documentation/macos-release-notes/macos-26-release-notes)

Skills researched and consulted:

- [OpenAI SwiftUI Liquid Glass skill](https://github.com/openai/plugins/blob/main/plugins/build-ios-apps/skills/swiftui-liquid-glass/SKILL.md): availability, modifier order, and grouped effects; iOS examples were checked against Apple's macOS SDK.
- [SwiftUI Pro](https://github.com/twostraws/SwiftUI-Agent-Skill): native controls, accessibility, stable view identity, and avoiding unnecessary updates.

No third-party UI framework was added. The macOS 26.5 SDK was used while retaining
the macOS 14 deployment target.

## Window motion and measurements

The conversation/screen split lives in a retained `NSHostingView` with
`safeAreaRegions = []`. This isolates it from navigation's pane expansion beneath
the toolbar and sidebar. The measured detail geometry sets the workspace frame;
zoom applies only inside that frame, leaving native tabs and navigation intact.
The first message stays visible and the reading column stays centered when the
sidebar opens.

Pane drop geometry uses a retained reference, so resizing updates hit testing
without publishing a second content update. Repeated drag cancellation and
unchanged drop previews no longer publish model changes. Same-frame pane layout
preserves an active transition; a changed frame takes over for live resizing.
Native editors remain mounted through window and pane resizing.

Keyboard-navigation and editor lookup now use explicit single-pass loops.
The previous recursive `lazy.compactMap(...).first` evaluated successful child
searches repeatedly, multiplying work as the native hierarchy deepened. A
deterministic regression builds 16 ancestors and asserts that each ancestor's
subviews are read once. Test helpers use the same correction; mixed accessibility
and native-view searches also retain visited object identities across both
paths to avoid duplicate traversal. Profiling identified these repeated
searches outside the timed frame operations as a cause of excessive hosted-test
elapsed time. Shorter harness runtime does not establish a corresponding UI
speedup or compositor frame rate.

The latest Debug fixture moves and resizes a hosted AppKit window containing a
conversation, then repeats resizing with two panes. Median synchronous work on
macOS 26.3.1:

| Operation | Before | Latest browser-tab build |
| --- | ---: | ---: |
| Window movement | 0.40 ms | 0.51 ms |
| Window resizing | 14.77 ms | 14.94 ms |
| Two-pane window resizing | 25.89 ms | 21.96 ms |
| 120-tab selection | 84.96 ms | 61.36 ms |
| 120-tab editor update | 23.58 ms | 18.84 ms |

These timings include the frame change and immediate layout/drawing. They
exclude asynchronous work during settling, Core Animation presentation, and
WindowServer compositing. The hosted test also does not reproduce the app
Scene's unified-toolbar setup or an interactive titlebar drag. These results
establish neither compositor frame rate nor an improvement across all metrics.
Raw results are `macos/build/evidence/native-window-motion-before.json` and
`native-window-motion-browser-final.json`. The many-tab comparison uses
`native-many-tabs-glass-before.json` and `native-many-tabs-browser-final.json`.

Earlier glass-pass measurements remain in the `native-many-tabs-glass-*` and
`native-performance-glass-*` evidence files. They measured the earlier tab UI,
not the final vertical/horizontal implementation. Equatable tab and response
controls still isolate unchanged controls from draft publications.

## Validation and limits

For the latest changes:

- The final native Protocol run executed 56 tests: 55 passed, with only the
  optional menu-bar check skipped. It completed in 46.3 seconds and includes the
  connected screen fixture, saved layouts, review filtering, retained drafts,
  keyboard lookup, and pane navigation.
- Focused motion, screen-pane, orientation, and zoom checks passed on the final
  native hosting boundary. These cover retained content and editor identity,
  not presentation frame rate.
- The column regression now uses a full-size content view and unified native
  toolbar. It passed sidebar-centering checks at widths 820, 1200, and 1600.
  Direct CUA review confirmed the first message and centered column in the real
  window with vertical tabs.
- Live CUA sidebar arrow navigation and switching away from and back to a draft
  passed. A rapid Escape/v sequence exposed a focus handoff that could lose v;
  sidebar Escape now focuses navigation synchronously. Its no-delay v/h
  regression passes and checks both split directions.
- The XCTest UI target compiles, but its runner could not launch the copied
  preview app ("does not have a process ID"). This is not a UI-test pass.

The earlier pass separately passed 61 native tests with six optional checks
skipped, all 11 focused remote-viewer tests, and an iOS simulator build. Its live
Hand journey completed file operations, restart, a second durable turn, and
queued-message steering, then cleaned up its fixture. Those results precede the
latest browser-tab and window-layout changes.

`pnpm build:macos` passed for the final changes. The Apple Silicon app is
`macos/build/Build/Products/Release/Nanocodex.app`; its code signature verifies
with `codesign --verify --deep --strict`. The installed app was not replaced.
The final test result is
`macos/build/Logs/Test/Test-Nanocodex-2026.09.12_16-25-24-+0300.xcresult`.
