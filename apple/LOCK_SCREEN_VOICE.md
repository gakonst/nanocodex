# Lock Screen voice tasks

The circular and rectangular **Speak to Nanocodex** widgets invoke `StartLockedVoiceIntent` instead of opening the app. **Record a voice task** is also available as an iOS 18+ Control Center / Lock Screen control and an App Shortcut. The inline widget retains the foreground recorder as a fallback.

## Setup and interaction

Sign in and use the foreground recorder once to grant Microphone and Speech Recognition access. Choose English or Ελληνικά there; the choice is remembered. Live Activities must be enabled. Background intents never present permission prompts or silently open the app.

Tap the widget/control to request capture in the app's background process. A compact Live Activity shows recording state and send-arrow and cancel icons. Capture continues until Send or Cancel; pauses never submit a task. Send stops audio and starts transcription of the recording. The Live Activity contains no transcript or account information. A final transcript starts a new cloud agent conversation; the activity reports Sent only after server admission. Recognition and network latency apply.

The recording intent adopts `AudioRecordingIntent` and `LiveActivityIntent`, with `openAppWhenRun = false` and `authenticationPolicy = .alwaysAllowed`. The activity starts before microphone activation and remains present during capture. Background audio is declared. None of these settings circumvents iOS permission or device-lock rules: a denied background microphone start reports failure without opening the app. This flow requires a physical-device test on the target iOS version before claiming unlock-free operation.

The coordinator owns the recorder independently of app scenes and refreshes recording activity freshness while capture continues. Before releasing the microphone it requests a finite background completion allowance. Delivery uses the recording UUID as the persisted message ID and server idempotency key. Errors preserve account-scoped recovery text or the ordinary pending delivery entry. Cancellation before submission and stale callbacks cannot admit unfinished speech. Once submission starts, delivery may be unconfirmed after cancellation/timeout; retries must retain the same message ID.

## Verification

Locked capture uses a dedicated temporary directory with `completeUntilFirstUserAuthentication` protection. The recorder prepares its output before the same protection is applied to the actual file, so protection does not depend on a placeholder surviving preparation. Audio remains encrypted and accessible after the first unlock following reboot, including when Send closes the recorder and speech recognition reopens the file. Completion, cancellation, and failed preparation delete the file; the next capture reaps orphan recordings, including files from the previous temporary-root layout.

Automated checks cover directory/output protection ordering, failed preparation/protection cleanup, capture callback fencing, and model delivery behavior. The filesystem test adapter models file replacement; it cannot validate iOS Data Protection itself. Simulator builds do not establish actual microphone availability while locked.

Physical-device acceptance requires:

- Pre-grant permissions while unlocked, then lock the phone and cover Face ID. Confirm the device remains locked before and after tapping the circular widget, rectangular widget, and Control Widget.
- Repeat with the app suspended, terminated, and after reboot plus the first device unlock. Do not interpret a foreground-started recording continuing after lock as a successful cold start.
- Verify the microphone indicator and Live Activity appear, Send/Cancel work, and the app never comes to the foreground.
- English: “Create a new agent thread and explain what this app can do.” Greek: “Δημιούργησε ένα νέο νήμα και εξήγησε τι μπορεί να κάνει αυτή η εφαρμογή.” Verify the final transcript and exactly one thread/turn.
- Test silent capture, concurrent taps, interrupted audio, a disconnected headset, denied permissions, disabled Live Activities, unavailable recognition, account switching, offline delivery, and app termination during delivery.
- Verify unavailable capture fails visibly without opening the app. Verify recovery does not automatically resubmit, and retries preserve the original idempotency key.

Platform documentation:
- https://developer.apple.com/documentation/appintents/audiorecordingintent
- https://developer.apple.com/documentation/activitykit/displaying-live-data-with-live-activities
- https://developer.apple.com/documentation/appintents/appintent/authenticationpolicy

## Device permissions

Settings → Device access requests Contacts, Location When In Use, and Photos individually. It displays limited/full contact and photo access, approximate/precise location, denial and restrictions, and refreshes after returning from iOS Settings. The iPhone Hand exposes read-only contact search, photo metadata search/details and current location tools; each checks current iOS authorization and never prompts. Limited access stays limited. Photo attachment via the existing system picker continues to work without library permission. Agent creation and turn-admission requests attach an optional recent location snapshot (coordinates, timestamp, accuracy and approximate flag) to client context. Acquisition is bounded to two seconds and failure omits the snapshot rather than blocking submission. The backend validates range and freshness and labels it client-reported context, never authority. No contacts or photo-library contents are included automatically in prompts.
