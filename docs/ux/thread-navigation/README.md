# Thread navigation controls

Actual iPhone 16 / iOS 18.2 simulator captures from the native app using synthetic demo conversations, captured on 2026-09-20 at implementation commit 9ff78a46.

- `collapsed-tools.png`: collapse all closes the grouped tool disclosure.
- `previous-message.png`: up jumps to the first user message and disables at the beginning.
- `next-message.png`: down returns to the next user message and disables at the end.

Signed simulator app and test targets compiled successfully. Three focused XCTest journeys passed with zero failures:

- `testThreadControlsCollapseAndNavigateUserMessages` (arrows, nested collapse, thread switching)
- `testCodeModeBatchKeepsCommandsTogether` (existing disclosure/accessibility regression)
- `testUserNavigationReleasesControlsAfterHistoryWithoutUserMessages` (terminal history page releases navigation controls)

These are deterministic simulator checks, not physical-device or authenticated network validation.
