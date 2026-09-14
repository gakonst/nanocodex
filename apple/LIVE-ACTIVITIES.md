# Agent thread notifications

Nanocodex publishes one native notification for each observed running
conversation when the app goes into the background. Each card contains its
conversation title, last known status, and a bounded excerpt of current-turn
commentary. Tool types provide a fallback when there is no commentary.
Delivery issues and queued follow-up counts come from the local outbox;
message inputs, reasoning, and raw tool arguments/results never appear.

Tapping a card selects that conversation. Swiping to clear it only dismisses
that thread's notification; it does not stop the agent. iOS controls the
notification stack, preview visibility, and grouping, using a stable
account-and-conversation thread identifier. Updates replace the same request
rather than accumulate one notification per progress event. Ongoing status
notifications use passive interruption level and no sound. Observed unread
completion or failure can replace a tracked running notification, with its
response excerpt or failure reason. Historical unread conversations do not
produce a notification flood on first launch.

`AgentNotificationController` serializes notification updates and requests
notification permission after running work is observed in the foreground.
`AgentNotificationLedger` persists identifiers and content hashes, not message
bodies. Clearing a notification suppresses further updates for that phase,
including after relaunch. A new turn or terminal outcome can notify again.
Verified removal, reviewed/deferred outcomes, and sign-out remove corresponding
notifications. Unchecked cards during account restoration retain their receipts.
Notifications from another account are removed when the account changes.

Links carry account scope and agent ID. They open only a conversation present
in the current account, including after restoration. Opening a notification
cannot send, stop, or approve work. The notification delegate is installed during
app initialization so taps can be retained while saved-account restoration runs.

## Freshness and migration

Regular agent streams pause when the app backgrounds. Accordingly, ongoing
notifications say **Running when last checked** and **Open for current status**.
These are local snapshots, not guaranteed background updates. There is no APNs
token registration or server publisher in this change. Reliable changes while
the phone remains locked or the app is terminated require that delivery path.

The earlier aggregate Live Activity is no longer started. The controller ends
any surviving aggregate activities, and the existing widget extension and
ActivityKit attributes remain available for that migration. The bounded
`AgentActivitySnapshot` projection supplies the per-conversation excerpts.

Apple documents [notification grouping](https://developer.apple.com/documentation/usernotifications/unmutablenotificationcontent/threadidentifier)
and [dismissal callbacks](https://developer.apple.com/documentation/usernotifications/unnotificationdismissactionidentifier).

## Validation

```sh
swift test --package-path apple/InboxCore
xcodebuild -project apple/NanocodexInbox.xcodeproj -scheme NanocodexInbox \
  -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- build
xcodebuild -project apple/NanocodexInbox.xcodeproj -scheme NanocodexInbox \
  -destination 'platform=iOS Simulator,name=iPhone 16 Pro' \
  -only-testing:NanocodexInboxUITests/AgentNotificationUITests \
  CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- test
```

`AgentThreadNotificationTests` covers independent receipts, historical outcome
suppression, dismissal persistence, new turns, late callbacks, restoration,
removal, and queue/privacy projection. `AgentActivityTests` covers the underlying
attention policy, excerpts, payload bounds, and account-scoped links.

The native UI journey uses explicitly enabled demo fixtures to verify separate
notifications, swipe-to-clear, persistence, and warm/cold URL routing.
Native notification activation is checked separately because the iOS 26
test simulator ignored synthesized XCTest taps on these notification cards.
Native accessibility activation was verified to select the matching forecast
conversation on the lock screen; the screenshot is saved under
`output/agent-notifications/final/native-tap-opened.png`. XCTest does not preserve demo environment variables for a cold system
notification launch, so cold routing is exercised using the same URL explicitly.
Ordinary demo journeys do not create notifications. Simulator evidence does not
establish physical-device delivery or APNs behavior.
