# Native Mac fixture capture

Actual Nanocodex macOS Debug app built from archived commit `f517a4393b6c3ead1b2fe46d1b6470ec0e1611c1`. The fixture uses the implementation's AppModel, ContentView, toolbar, ProjectsView, tabs and transcript views. It replaces account/runtime responses in the existing `NANOCODEX_NATIVE_UI_FIXTURE` hook, seeds synthetic messages and calls real navigation methods. It does not run a model or backend delegation.

The build has a distinct bundle identifier, an isolated `NANOCODEX_DESKTOP_DATA` directory, and disabled desktop activation. Account services and Hand sharing are not started. No installed app, real account, production service or desktop input is used. The Mac had no advertised screen-control capability, so capture uses AppKit view bitmaps inside this isolated process.

| File | Caption |
| --- | --- |
| `00.png` | Native Main Thread toolbar entry in an isolated fixture app. |
| `01.png` | Main Thread opened through the real AppModel method; synthetic welcome transcript. |
| `02.png` | Repeated Main Thread opening reuses its existing tab. The fixture asserts the same active tab ID and unchanged tab count. |
| `03.png` | Implemented Projects sheet, listing two synthetic server projects. |
| `04.png` | Selecting Orchard website opens its coordinator; selected thread ID is asserted. Transcript text is seeded. |
| `05.png` | Main Thread result presentation using explicitly synthetic text. No project delegation or result delivery occurred. |
| `native-view-recording.mp4` | Continuous samples of the actual native views while the fixture drives the above sequence, including the Projects sheet. 76 frames sampled at 10 Hz, encoded to H.264 at 1280×860 with aspect-preserving white padding. It is an in-process view recording, not a desktop screen recording or pointer-driven UI test. |

The raw screenshot dimensions are preserved. The app's large window contains considerable whitespace; the sheet bitmap is captured at its own native bounds. The video switches to the attached sheet while it is visible. No UI is recreated or rendered from a mock image. Screenshots and sampled video frames were visually inspected. Timing is fixture-driven and is not performance evidence.

Reproduce from this repository with `NANOCODEX_VOICE_ARTIFACT=/path/to/NanocodexVoiceCore.xcframework bash docs/ux/media/main-thread-396/macos/capture.sh`. The script archives the source into a new isolated directory, applies the capture-only `prepare.py`/`Fixture.swift`, builds it, captures PNGs, and encodes the sampled frames. It does not modify production UI source. `capture.log` records the model assertions and capture dimensions.

Limitations: no pointer/keyboard journey, live backend, genuine delegation, server registration or project creation. The implemented Mac Projects sheet only lists/selects coordinators; it has no create/register form. Seeded Main Thread text is presentation coverage only, and does not prove those actions work.
