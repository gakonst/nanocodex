# Lock Screen voice tasks

The circular and rectangular **Speak to Nanocodex** Lock Screen widgets invoke `StartLockedVoiceIntent` without opening the app or asking to unlock. The inline accessory is not supported because it cannot run the recording intent. **Record a voice task** is also available as an iOS 18+ Control Center / Lock Screen control and an App Shortcut. Pre-grant Microphone and Speech Recognition once in the app; iOS still decides whether a locked background microphone start is available on a given device/OS. A failed activity shows a privacy-safe cause, and retry remains on the Lock Screen. Physical-device validation is required before claiming the original failure is fixed.

## Setup and interaction

Sign in and use the foreground recorder once to grant Microphone and Speech Recognition access. Choose English or Ελληνικά there; the choice is remembered. Live Activities must be enabled. Background intents never present permission prompts or silently open the app.

Tap the widget or control, or invoke the Shortcut, to start microphone capture and streaming transcription in the app's background process. A compact Live Activity shows recording state and send-arrow and cancel icons. The recorder starts immediately after pre-granted permissions are checked; once an utterance ends, the final transcript automatically starts a new cloud agent conversation. The Send control explicitly ends audio and submits the final transcription sooner; Cancel stops without sending. The Live Activity contains no transcript or account information. It reports Sent only after server admission. Recognition and network latency apply. iOS may interrupt or deny a locked microphone start; that must be observed on an actual device.

The recording intent adopts `AudioRecordingIntent` and `LiveActivityIntent`, with `openAppWhenRun = false` and `authenticationPolicy = .alwaysAllowed`. The activity starts before microphone activation and remains present during capture. Background audio is declared. None of these settings circumvents iOS permission or device-lock rules: a denied background microphone start reports failure without opening the app. This flow requires a physical-device test on the target iOS version before claiming unlock-free operation.

The coordinator owns the recorder independently of app scenes and refreshes recording activity freshness while capture continues. Before releasing the microphone it requests a finite background completion allowance. Delivery uses the recording UUID as the persisted message ID and server idempotency key. Errors preserve account-scoped recovery text or the ordinary pending delivery entry. Cancellation before submission and stale callbacks cannot admit unfinished speech. Once submission starts, delivery may be unconfirmed after cancellation/timeout; retries must retain the same message ID.

## Meeting listening from the Lock Screen

Add the circular/rectangular **Listen to a meeting** widget to the Lock Screen, or its Control Widget to the Lock Screen or Control Center after granting Microphone and Speech Recognition access in the app. Starting it uses a separate background `AudioRecordingIntent` and Live Activity; it does not open the app or require an unlock after the device's first unlock since reboot, subject to iOS microphone policy. A single **Finish & send** action stops the microphone, waits for bounded transcription segments, and starts a new cloud thread with the transcript. **Discard** cancels it without sending. The Live Activity shows recording duration/status but never the transcript or account. Speech recognition uses short requests rather than a single meeting-length request; if recognition fails or is interrupted, only the available partial transcript can be recovered. It does not capture other apps' protected audio. Long locked sessions and cold starts require physical-device testing; a simulator build does not prove them.

## Verification

The legacy explicit-send file recorder uses a dedicated temporary directory with `completeUntilFirstUserAuthentication` protection. That recorder prepares its output before the same protection is applied to the actual file, so protection does not depend on a placeholder surviving preparation. The foreground quick-voice recognizer and the locked auto-send flow stream audio without retaining a recording file. The legacy recorder deletes its file on completion, cancellation, and failed preparation; the next capture reaps orphan recordings, including files from the previous temporary-root layout.

Automated checks cover legacy directory/output protection ordering, failed preparation/protection cleanup, streaming capture callback fencing, and model delivery behavior. The filesystem test adapter models file replacement; it cannot validate iOS Data Protection itself. Simulator builds do not establish actual microphone availability while locked.

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
