# Main Thread mobile visual evidence (PR #396)

Captured 2026-09-18 from `f517a4393b6c3ead1b2fe46d1b6470ec0e1611c1`, using the actual NanocodexInbox Debug simulator app, built in an isolated worktree. The later PR commit `5cb255fd` changes a backend test only; it does not change these UI sources.

**Every image and recording here uses the app's built-in synthetic `--demo` mode.** No production account, installed desktop app, real phone, or user data was used. These are native UIKit/SwiftUI renders, not drawn mockups. No API/model execution, cross-device synchronization, or live deployment is demonstrated. In demo mode Main Thread receives the existing generic example transcript (the tab-bar conversation); it is not a real Main Thread outcome.

Devices: fresh dedicated iPhone 16 Pro and iPad Pro 11-inch (M4) simulators, iOS 18.2. This captures the supported pre-iOS-26 appearance, **not iOS 26 Liquid Glass**. Existing iOS 26 simulator failures were avoided by creating these new isolated devices. Xcode used its installed iOS 26.5 SDK. The ignored VoiceCore XCFramework prerequisite was copied from the existing mobile build; production Swift UI sources were unchanged.

## Screenshots and captions

Each `iphone-` file has a corresponding `ipad-` file below. Native screenshots retain their original resolution.

| Suffix | Exact caption / demonstrated coverage |
| --- | --- |
| `main-entry-drawer.png` | Fixture-backed native project drawer: dedicated Main Thread entry above Orbit and Ledger. |
| `main-thread-chat.png` | Main Thread opened with its own “Plan across projects” draft. Example transcript is generic built-in demo content, not a Main Thread result. |
| `project-sidebar.png` | Project list and Main Thread navigation after returning from the project chat. |
| `project-chat.png` | Orbit coordinator chat with fixture task links and the retained project draft. |
| `project-tasks-sheet.png` | Native Tasks sheet: two active fixture tasks and one completed fixture task. |
| `project-task-detail.png` | An active fixture child's task detail, opened from the task list. |
| `project-agents-sheet.png` | Native Agents sheet listing the project coordinator and fixture child agents. |
| `project-origin-thread.png` | Completed fixture result opened from its origin link in the coordinator chat; no actual delegation was run. |
| `project-registration-entry.png` | App menu exposes “Make project available across devices” for a local alias. Entry only: the action is not invoked because demo mode has no ManagedClient. |
| `project-create-form.png` | Actual native New project form containing synthetic “Evidence Project.” |
| `project-created.png` | The synthetic project has been created and selected in the native app. Demo creation persists a local alias, not a server canonical project. |
| `project-created-relaunched.png` | The new local demo project remains in the drawer after app termination/relaunch. |
| `project-created-selected.png` | Selecting that persisted project reopens its conversation and composer. |

## Short recordings

These are real `simctl io recordVideo` captures of XCTest driving the app, trimmed and re-encoded to H.264, 20 fps, 960px height. No UI content was composited or generated. Simulator recording has timing discontinuities; these clips illustrate UI interaction, **not latency or animation performance**. Start, middle and end frames were inspected after encoding. The corresponding stills show the settled views.

| iPhone | iPad | Caption |
| --- | --- | --- |
| [Main Thread](iphone-main-thread.mp4) | [Main Thread](ipad-main-thread.mp4) | Open Main, type a separate draft, navigate through the project drawer and reuse Main. Fixture-backed native UI. |
| [Child / result](iphone-child-and-result.mp4) | [Child / result](ipad-child-and-result.mp4) | Open child agent, retain separate coordinator/child drafts, inspect a completed fixture result via its origin link. |
| [Tasks / agents](iphone-tasks-and-agents.mp4) | [Tasks / agents](ipad-tasks-and-agents.mp4) | Browse Tasks, task detail and Agents, then return to the retained project draft. |
| Creation is covered by the five stills above and the passing UI journey. | [Project creation](ipad-project-create.mp4) | Registration menu entry, New project form, creation and persisted selection after relaunch. All local demo data. |

## Validation and reproduction

The existing `InboxUITests` tests at the captured commit drove the first three recordings:

- `testMainThreadKeepsIndependentDraftAndProjectNavigation`
- `testProjectTasksAndAgentsPreserveDraft`
- `testProjectChildNavigationAndOriginLinkPreserveSeparateDrafts`
- `testNamedProjectSurvivesRelaunch`

All four passed on iPhone (86.610s total). On iPad the Main/drafts, child/results and task/agent tests passed; `testNamedProjectSurvivesRelaunch` failed waiting for the `Orbit UX` task-sheet navigation bar immediately after creation. Its failure hierarchy already contained the created project title and composer, but no open sheet. This is retained as an unresolved original journey failure; it is not counted as a pass.

The evidence-only [capture patch](repro/mobile-capture.patch) adds `testPR396ProjectCreationAndRegistrationEntryEvidence`, which captures the registration entry, creates a project, terminates/relaunches and selects it again. The supplemental creation journey passed on both devices (iPad 24.723s; iPhone corrected run 29.181s). An initial iPhone capture-harness attempt tapped the menu center and opened Scheduled jobs; the corrected harness relaunches to dismiss the menu. An intermediate `test-without-building` attempt discovered zero tests and is not counted as validation. The final iPhone `test` invocation ran and passed exactly one test. The production app source was never patched.

Use a fresh isolated worktree at the source commit. Apply the patch only for capture, not to production sources. Build with:

```sh
xcodebuild -project apple/NanocodexInbox.xcodeproj -scheme NanocodexInbox \
  -destination 'platform=iOS Simulator,id=YOUR_DEDICATED_SIMULATOR' \
  -derivedDataPath .evidence-build CODE_SIGNING_ALLOWED=NO build-for-testing
```

Run each selected journey with `-parallel-testing-enabled NO`, `-only-testing:NanocodexInboxUITests/InboxUITests/TEST_NAME`, an isolated `-resultBundlePath`, and `test` (or `test-without-building` after confirming matching test discovery). Record only that dedicated simulator using `xcrun simctl io DEVICE_ID recordVideo --codec=h264 capture.mov`; stop that exact process with SIGINT. Export screenshots via `xcrun xcresulttool export attachments --path RESULT.xcresult --output-path ATTACHMENTS`. The XCTest captures use `.keepAlways`. Each demo launch uses a fresh UUID profile to isolate its synthetic state.

## Remaining mobile gaps

- No live canonical registration, cross-device project persistence, live model delegation or backend outcome delivery is shown.
- Registration is menu-entry coverage only. Demo project creation follows the existing local-demo branch.
- No physical iPhone/iPad, landscape, accessibility-size, dark-mode or iOS 26 appearance capture.
- Original iPad creation/task-sheet test failure remains documented above; supplemental persistence capture does not erase it.
