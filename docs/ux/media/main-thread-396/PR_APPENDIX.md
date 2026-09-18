## Screenshots and demo recordings

[Full evidence index: 36 screenshots, 9 short recordings, exact captions, reproduction and coverage gaps](https://github.com/gakonst/nanocodex/blob/feat/main-thread-integrated-20260918/docs/ux/2026-09-18-main-thread-evidence.md).

**All evidence is local and fixture-backed, captured from the implemented app UI at `f517a439`. No deployed flow, live model, backend delegation or cross-device registration is claimed.** Web uses the complete account app with synthetic API responses. Mac uses an isolated native fixture build and in-process view recording; iPhone/iPad use fresh dedicated iOS 18.2 simulators and native UI tests.

| Full web app: seeded Main Thread result presentation | Native Mac: implemented Projects sheet |
| --- | --- |
| <img src="https://raw.githubusercontent.com/gakonst/nanocodex/feat/main-thread-integrated-20260918/docs/ux/media/main-thread-396/web/02-main-thread.png" width="520" alt="Full web app with synthetic Main Thread transcript"> | <img src="https://raw.githubusercontent.com/gakonst/nanocodex/feat/main-thread-integrated-20260918/docs/ux/media/main-thread-396/macos/03.png" width="360" alt="Native Mac synthetic Projects sheet"> |

| iPhone: Main Thread entry above projects | iPad: native demo project creation |
| --- | --- |
| <img src="https://raw.githubusercontent.com/gakonst/nanocodex/feat/main-thread-integrated-20260918/docs/ux/media/main-thread-396/iphone-main-entry-drawer.png" width="270" alt="iPhone fixture project drawer"> | <img src="https://raw.githubusercontent.com/gakonst/nanocodex/feat/main-thread-integrated-20260918/docs/ux/media/main-thread-396/ipad-project-create-form.png" width="360" alt="iPad fixture New project form"> |

Recordings:

- [Web navigation and seeded transcript](https://github.com/gakonst/nanocodex/blob/feat/main-thread-integrated-20260918/docs/ux/media/main-thread-396/web/navigation.webm): Main Thread open, project selection, same fixture identity reused.
- [Native Mac view recording](https://github.com/gakonst/nanocodex/blob/feat/main-thread-integrated-20260918/docs/ux/media/main-thread-396/macos/native-view-recording.mp4): entry, open/reuse, project picker/selection, seeded result. Programmatic fixture navigation; no desktop input.
- iPhone: [Main Thread](https://github.com/gakonst/nanocodex/blob/feat/main-thread-integrated-20260918/docs/ux/media/main-thread-396/iphone-main-thread.mp4), [child/result](https://github.com/gakonst/nanocodex/blob/feat/main-thread-integrated-20260918/docs/ux/media/main-thread-396/iphone-child-and-result.mp4), [tasks/agents](https://github.com/gakonst/nanocodex/blob/feat/main-thread-integrated-20260918/docs/ux/media/main-thread-396/iphone-tasks-and-agents.mp4).
- iPad: [Main Thread](https://github.com/gakonst/nanocodex/blob/feat/main-thread-integrated-20260918/docs/ux/media/main-thread-396/ipad-main-thread.mp4), [project creation/relaunch](https://github.com/gakonst/nanocodex/blob/feat/main-thread-integrated-20260918/docs/ux/media/main-thread-396/ipad-project-create.mp4), [child/result](https://github.com/gakonst/nanocodex/blob/feat/main-thread-integrated-20260918/docs/ux/media/main-thread-396/ipad-child-and-result.mp4), [tasks/agents](https://github.com/gakonst/nanocodex/blob/feat/main-thread-integrated-20260918/docs/ux/media/main-thread-396/ipad-tasks-and-agents.mp4).

Capture validation: four original iPhone UI journeys passed, plus the supplemental creation/relaunch journey. Three original iPad journeys and the supplemental creation/relaunch journey passed; the original iPad creation test failed waiting for its task sheet and remains documented. Mac built successfully and asserted tab reuse/project selection. Web retained no console/page errors.

Remaining gaps: mobile registration is menu-entry-only; demo creation is local, not canonical/server registration. Web/Mac have no dedicated create/register form in this implementation. Result content is seeded, not executed. No physical-device or iOS 26 Liquid Glass capture. Recordings are not performance evidence.
