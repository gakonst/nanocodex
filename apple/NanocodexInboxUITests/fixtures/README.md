# Embedded screen fixture

Run from the repository root after `pnpm install`:

```sh
pnpm exec node apple/NanocodexInboxUITests/fixtures/remote-screen.mjs
```

This serves a view-only screen on `127.0.0.1:18965` through the real discovery
and JPEG WebSocket transport. It does not use account credentials or execute
remote input. Regenerate the image with:

```sh
swift apple/NanocodexInboxUITests/fixtures/screen.swift apple/NanocodexInboxUITests/fixtures/screen.jpg
```

With the fixture running, set `TEST_RUNNER_NANOCODEX_SCREEN_FIXTURE=1` when
running Xcode tests. The focused journeys are:

- iOS dock: `NanocodexInboxUITests/InboxUITests/testThreadScreenDockPreservesDraftAndThreadNavigation`
- iOS full controls: `NanocodexInboxUITests/RemoteScreenLifecycleUITests/testFullScreenZoomDoneAndDraftRestoration`
- macOS: `NanocodexTests/ProtocolTests/testScreenPaneResizesWithoutReplacingConversation`

The iOS journey uses `--demo` plus `NANOCODEX_DEMO_SCREENS=1`; this loopback
service is available only in the Debug demo. Normal launches retain the
account-owned screen service. The desktop journey injects the loopback service
into an isolated model with runtime requests stubbed.

`FrontiersMerchSample.mp4` is a four-second, low-resolution excerpt of the user-requested Paradigm Frontiers merch launch video, used solely to exercise Quick Look and the share sheet in the output-link UI test. The full video stays in private Brain outputs, not the app bundle.

## Memories browser

The Memories journeys use the existing Debug Simulator `StartupFixtureProtocol`
transport fixture in `DemoContent.swift`. They launch the production app shell,
account client, expandable tree and file reader with a new synthetic profile per
test. No fixture views or live account credentials are involved. This replaces
the remote memory service at URLSession's protocol boundary; it does not exercise
a TCP connection, the deployed Worker, or real account authorization.

Run on macOS from the repository root with the normal app build prerequisites
(including the shared voice core) installed. Set `device` to one existing iPhone
or iPad Simulator UDID; run destinations sequentially:

```sh
mkdir -p output/memories-ui
scripts/xcodebuild-guard.sh \
  -project apple/NanocodexInbox.xcodeproj -scheme NanocodexInbox \
  -configuration Debug -destination "platform=iOS Simulator,id=$device" \
  -derivedDataPath output/memories-ui/build \
  -resultBundlePath "output/memories-ui/memories-$(date +%Y%m%dT%H%M%S).xcresult" \
  -only-testing:NanocodexInboxUITests/InboxUITests/testMemoriesExpandReadAndRecoverFailedPages \
  -only-testing:NanocodexInboxUITests/InboxUITests/testMemoriesRootPaginationSharedFilesAndEmptyFolder \
  -only-testing:NanocodexInboxUITests/InboxUITests/testMemoriesEmptyLibraryAndEmptyFile \
  -only-testing:NanocodexInboxUITests/InboxUITests/testMemoriesByteLimitedReadUsesSmallerWindowBeforeContinuing \
  CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- test
```

The journeys assert collapse/re-expansion, nested and root page continuation,
failed-page recovery without lost or duplicate content, distinct personal/shared
files with the same basename, empty states, and smaller read windows after the
service's byte limit. Synthetic server pages are intentionally small so the
continuation controls remain reachable on a phone. The byte-limit case returns
an oversized initial window and accepts a smaller one before continuation.

Screenshots are kept as XCTest attachments. The simulator application's
`Documents/startup-requests.jsonl` records each memory request's path, cursor,
line offset, requested limits and response status, with no credential headers.
After a run, copy that file from the data container reported by
`xcrun simctl get_app_container "$device" xyz.paradigm.centaur data` into the
run's ignored evidence directory. The trace spans the test process launches.
Run the four methods explicitly with the command above; the bounded PR selection
in `apple-inbox.yml` does not currently include them. Compiling the test target
alone is not a UI pass.

## Attachment library and contained screen dock

The focused demo journeys are:

- `InboxUITests/testAttachmentLibrarySheetPreservesDraft`: opens and dismisses
  the Library sheet twice in light and dark appearances, checks that camera,
  recent-photo strip, See all, Add files, and Add videos are reachable, and
  verifies that text and existing photo attachments survive dismissal.
- `InboxUITests/testThreadScreenDockContainsRowsAcrossAppearanceAndSelection`:
  checks long-name rows at XXXL text size in light and dark appearances, selects
  a live fixture screen, changes the selection, and hides/reopens the dock.
  Row bounds, the full VoiceOver name, and continued composer access are asserted;
  screenshots retain visual evidence for text contrast and clipping review.

Run the loopback fixture above, then use an existing Simulator UDID as `device`
with the repository's normal native build prerequisites:

```sh
TEST_RUNNER_NANOCODEX_SCREEN_FIXTURE=1 scripts/xcodebuild-guard.sh \
  -project apple/NanocodexInbox.xcodeproj -scheme NanocodexInbox \
  -configuration Debug -destination "platform=iOS Simulator,id=$device" \
  -derivedDataPath output/attachment-screen-ui \
  -resultBundlePath output/attachment-screen-ui/journeys.xcresult \
  -only-testing:NanocodexInboxUITests/InboxUITests/testAttachmentLibrarySheetPreservesDraft \
  -only-testing:NanocodexInboxUITests/InboxUITests/testThreadScreenDockContainsRowsAcrossAppearanceAndSelection \
  -only-testing:NanocodexInboxUITests/InboxUITests/testThreadScreenDockPreservesDraftAndThreadNavigation \
  test
```

The result bundle retains screenshots at sheet presentation/dismissal and dock
selection/reopening. Demo mode intentionally cannot import new attachments.
The existing opt-in `testLiveImageAttachmentPickersDraftAndHistory` covers native
See all, Files, and Videos picker cancellation with a nonempty draft, then imports
its supplied image fixture through Files. `testLiveCameraCaptureAndCancel` covers
camera capture and cancellation on a physical phone. These require their documented
live-account/fixture environment variables and are not enabled by the command above.
Recent-photo thumbnail selection/import and native photo/video filtering still
need a seeded Photo-library device journey; sheet layout coverage does not establish
those behaviors.
