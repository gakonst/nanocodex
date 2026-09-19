# Lock Screen voice tasks

Add **Nanocodex → Speak to Nanocodex** from the iPhone Lock Screen widget picker. Circular, rectangular, and inline widgets launch `nanocodex://voice/new`.

The widget opens the app, with system authentication if required. Microphone capture runs in the foreground app, not the widget extension. Sign in and grant microphone and Speech Recognition permissions on first use. Choose English or Ελληνικά in the recorder; the language selection is remembered. Apple Speech recognition availability depends on the device, locale, and network.

Speak your request, then pause to finish. A final transcript starts a new agent thread through the normal persisted delivery queue. Cancel stops capture. An interruption or recognition failure must preserve the text for review rather than automatically sending an unfinished request.

## Device acceptance checks

These require a signed build on a physical iPhone; simulator builds and unit tests cannot establish microphone accuracy or Lock Screen authentication behavior.

- Add each widget family, lock the phone, tap, unlock, and verify recording starts without another microphone tap after setup.
- First use: grant permissions; also exercise microphone denial and Speech Recognition denial.
- English: “Create a new agent thread and explain what this app can do.” Verify final text and exactly one new thread.
- Greek: “Δημιούργησε ένα νέο νήμα και εξήγησε τι μπορεί να κάνει αυτή η εφαρμογή.” Verify accents, final text, and exactly one new thread.
- Switch languages, reopen from the widget, and verify the choice persists.
- Stay silent, cancel, lock the phone mid-sentence, switch apps, and interrupt with a call: no unfinished request should be automatically sent.
- Disable connectivity during capture and during delivery. Verify understandable errors, retained transcript, and normal queued-message retry without duplicate threads or turns.
- Launch while signed out and during a cold start. Verify capture waits for a usable account and never submits into a different account.
