# Agent thread notifications

Nanocodex publishes a native notification for an observed unread completion or
failure of a conversation previously observed running, while the app is in the
background. Running work and changing progress excerpts do not notify. Each
outcome revision can notify once, even if its text later changes or the app
relaunches. Historical unread conversations do not notify on first launch.

Each card contains its conversation title, status, and a bounded excerpt.
Message inputs, reasoning, and raw tool arguments/results never appear.
Tapping a card selects that conversation. Clearing it does not stop the agent.
iOS controls grouping using a stable account-and-conversation identifier;
new outcomes replace the same request and use active interruption without sound.

`AgentNotificationController` serializes notification updates and requests
notification permission after running work is observed in the foreground.
`AgentNotificationLedger` persists identifiers and content hashes, not message
bodies. Clearing a notification suppresses further updates for that phase,
including after relaunch. A new terminal outcome can notify again.
Verified removal, reviewed/deferred outcomes, and sign-out remove corresponding
notifications. Unchecked cards during account restoration retain their receipts.
Notifications from another account are removed when the account changes.

Links carry account scope and agent ID. They open only a conversation present
in the current account, including after restoration. Opening a notification
cannot send, stop, or approve work. The notification delegate is installed during
app initialization so taps can be retained while saved-account restoration runs.

## Freshness and migration

Regular agent streams pause when the app backgrounds. Notifications depend on
locally observed outcomes, not guaranteed background updates. There is no APNs
token registration or server publisher in this change. Reliable changes while
the phone remains locked or the app is terminated require that delivery path.
Previously delivered running notifications are removed on the next update.

The earlier aggregate Live Activity is no longer started. The controller ends
any surviving aggregate activities at launch, on every foreground activation,
and during notification updates, independently of account restoration. The existing widget extension and
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

The native UI journey uses explicitly enabled demo fixtures to verify running
threads remain silent across foreground/background transitions and warm/cold
URL routing still works. Ordinary demo journeys do not create notifications.
Simulator evidence does not establish physical-device delivery or APNs behavior.
