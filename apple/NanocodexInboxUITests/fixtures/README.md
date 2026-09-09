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

- iOS: `NanocodexInboxUITests/RemoteScreenLifecycleUITests/testInlineScreenKeepsHistoryComposerAndSession`
- macOS: `NanocodexTests/ProtocolTests/testScreenPaneResizesWithoutReplacingConversation`

The iOS journey uses `--demo` plus `NANOCODEX_DEMO_SCREENS=1`; this loopback
service is available only in the Debug demo. Normal launches retain the
account-owned screen service. The desktop journey injects the loopback service
into an isolated model with runtime requests stubbed.
