# Muse → Nanocodex mobile UX

## Evidence and review

User-supplied recording: `ScreenRecording_09-16-2026 22-52-32_1.MP4`.
Duration **2:15.553**, **1206 × 2622**, HEVC, nominal **60 fps**, average **59.81 fps**.

Local review bundle: `/Users/georgios/Movies/Nanocodex-UX/2026-09-16-muse/`.
Open `review.html` to play the original with chapter selection, looping, playback speed, and 1/60-second seeking. `muse-reference.mp4` is a preserved copy of the supplied original; `chapters.json` is the machine-readable index. Private screenshots and videos stay outside the repository.

Reviewed the complete sequence as timestamped overview frames, then inspected 12-fps sequences around the drawer, keyboard, attachment, destination, and settings transitions. These are visual observations of supplied footage, not a claim of independently operating Muse or measuring its input latency. Overview timestamps are approximate; dense sequences use nominal sampling times. Touch locations, haptics, refresh-rate negotiation, and source-app dropped frames are not established by this recording.

The user subsequently authorized adapting Muse's UI to Nanocodex's capabilities and using the connected iPhone. The wired device is `net`, iPhone 17 Pro. iPhone Mirroring still requires iCloud sign-in; Xcode can see the wired device. Native app builds and UI tests provide the validation path.

## Follow-up: opening from the screen edge

The previous revision supported closing by swipe but omitted edge-swipe opening.
Rightward drags starting within 28 points of the left edge now reveal the drawer;
leftward drags close it. Short, slowly released pulls return to the original state.
Horizontal gestures elsewhere stay with the conversation. Opening dismisses the
keyboard without changing the selected conversation or its draft.

The list stays stationary under the sliding conversation. Moving a newly inserted
native scroll view while the keyboard dismissed could leave its rows offscreen;
keeping its frame fixed avoids that bug. The drawer is unmounted when closed so
hidden navigation controls cannot receive input or retain search focus.

The Release phone check passed three edge-opening/left-closing round trips
in 18.870 seconds of XCTest wall time, preserving selected identity and the
untouched draft (`/tmp/nanocodex-edge-swipe-device-final.xcresult`). Here “edge
opening” means a rightward drag from the left screen edge. This is a functional
check, not a new performance benchmark. All four final simulator journeys
passed, including edge opening with the keyboard visible, a cancelled short pull,
selection/draft restoration, ordinary transcript swipes, and existing drawer/sheet
navigation (`/tmp/nanocodex-edge-swipe-final.xcresult`). The final recording is
`edge-swipe-validation-web.mp4`, also available in the local review player's source
selector. The verified Release build remains installed on the phone.
 An earlier benchmark attempt stopped
before testing gestures because existing conversation history did not finish
loading; the edge test deliberately does not depend on network history.


## Follow-up: compact bubbles and a single drawer

The next revision removes the live-preview grid and All/Running/Hidden filters.
All known conversations, including formerly hidden tabs, appear in one lazy,
searchable list. Green titles and dots identify running agents; VoiceOver still
announces status. Left swipes follow the finger and dismiss with a spring;
vertical scrolling leaves the drawer open. Selection and drafts stay mounted.

Replies now fit their content, with 12-point horizontal and 9-point vertical
padding and tighter paragraph spacing. Copy response moves into the long-press
menu and accessibility actions; code keeps its dedicated copy control. The
composer uses 4-point outer vertical padding and a 32-point send glyph background
inside its unchanged 44-point touch target. The dock has Back, Screens, New, Context.
The earlier recordings below show the first pass, including now-removed filters.

Device validation caught and fixed a swipe-release bug: a SwiftUI row Button could
select a different conversation at the end of a drag. Rows now use tap recognition
that fails during dragging, with an equivalent accessible Button representation.
The final Release journey passed on the iPhone 17 Pro in 102.227 seconds of XCTest
wall time. All three measured iterations reported zero hitches and retained the
selected conversation and original draft. Absolute physical memory was 44,844.024 /
44,581.880 / 45,515.768 kB. This tests a narrow navigation journey, not Muse parity
or end-to-end latency. Metrics: `2026-09-17-muse-compact-device-metrics.json`.
The final build is installed on the phone, with the original conversation restored.

Eight focused simulator journeys passed across the compact-UI runs: dock layout,
reply long-press/copy, stopping agents without filters, left swipe with draft retention,
search/switch/drafts, sheets, largest accessibility text, and rich thinking/code copy.
The final five-test suite is `/tmp/nanocodex-muse-tight-final.xcresult`; the three
other passing checks are in `/tmp/nanocodex-muse-tight-v2.xcresult` (whose old bubble
AX-wrapper assertion failed and was replaced by actual long-press/copy behavior).
All 24 shared `NanocodexUI` tests passed on the final sources. The prior first-pass
history/steering checks are recorded separately below.

`nanocodex-compact-final-cfr.mp4` records the five passing final simulator journeys.
It is the fourth source in `review.html`, with chapters for compact replies/copy,
stopping, left swipe, and draft-preserving navigation. First-pass recordings are
explicitly labeled. Fixtures and automation pauses remain visible; this is a
functional VOD, not a frame-rate or latency measurement.



## Timestamped observations

| Time | Observed in Muse | Nanocodex adaptation |
| --- | --- | --- |
| 00:00–00:03 | Compact floating composer and dock; small independent header controls; distinct soft user/assistant bubbles | Replace the persistent horizontal tab strip with a compact current-session header. Float the existing navigation controls and give context a direct entry point. |
| 00:03–00:10 | Browser work opens on a separate dark surface with explicit connection/control states | Open existing remote screens in a native sheet. Reuse viewing, control, reconnection, and keyboard behavior. |
| 00:10–00:32 | The browser remains the focus while controls and keyboard change around it | Preserve the selected remote viewer through sheet resizing; don't resize the conversation into a cramped split pane. |
| 00:33–00:46 | Rapid traversal of a long mixed-content chat with stable navigation chrome | Preserve Nanocodex's bounded history, native scroll anchoring, cached Markdown, and image visibility handling. |
| 00:46–00:48 | Composer follows keyboard dismissal; dock reappears | Retain native keyboard-driven layout and interactive dismissal. Test composer/draft continuity. |
| 00:48–00:50 | Chat slides sideways to a lightweight session list, then another conversation slides in | Add a lightweight searchable drawer. Keep the existing transcript mounted while the drawer opens. |
| 00:50–01:01 | Revisiting different conversations immediately reveals existing content in the sampled sequence | Preserve cached transcripts, independent drafts, back navigation, and exact reading anchors. |
| 01:02–01:04 | A compact attachment sheet rises over the conversation | Native attachment sheet with Photos & Videos, Camera, Files, and captured context. The real picker opens after the sheet dismisses. |
| 01:05–01:08 | Agent activity is an on-demand surface with a return transition to chat | Compact activity cards identify the current action, summarize reasoning/tool calls, and surface failures while collapsed. Expand into step state and rich details; keep generated outputs directly in chat. |
| 01:08–01:20 | Destination changes keep the dock in place; lists remain visually restrained | Keep the dock spatially stable and retain direct Nanocodex actions rather than inventing unsupported Muse destinations. |
| 01:21–01:25 | Settings presents above the session list | Native settings sheet with its own navigation stack and a clear dismissal path. |
| 01:25–01:36 | Connected/available services use native grouped lists | Preserve the existing connector implementations and account policy rather than copying consent behavior. |
| 01:36–01:47 | A connector summary precedes system authentication | Reference only. No account permissions or connector connections changed during this work. |
| 01:47–02:00 | Nested settings navigation retains the parent surface | Reuse native navigation inside settings. |
| 02:02–02:08 | Appearance settings show immediate preview changes | Future candidate; not required to reproduce the core navigation and conversation experience. |
| 02:08–02:15 | Notification/permission settings follow the same grouped structure | Reference only; preserve Nanocodex's existing policies and capabilities. |

## What makes the movement coherent

The drawer opening around 00:48 reveals intermediate horizontal positions instead of a blank replacement. The second conversation enters around 00:49.58 and reaches its final position within several 12-fps samples. The attachment sheet around 01:02.58 rises while the conversation remains visible underneath. Around 01:08.17 the destination changes but the bottom dock remains an anchor. These observations motivate short settling transitions and native presentation; they are not measurements of touch-to-first-frame latency or the exact easing function.

Implementation uses a 0.32-second, strongly damped spring for the drawer, with movement disabled under Reduce Motion. Native sheets own their interactive drag and dismissal. Conversation identity does not change when opening navigation. Search rows use roster text only: no Markdown, media decoding, miniature transcript rendering, or per-row history subscriptions. Full previews remain opt-in.

## Adaptation priorities

1. **Fast navigation without losing context.** A searchable drawer for everyday switching; live previews for inspecting parallel work. Keep selected identity, drafts, reading anchors, and background runs intact.
2. **A quiet conversation surface.** One title, soft bubbles, a compact composer, and a floating dock. Keep long technical output readable and controls accessible.
3. **Native detail surfaces.** Screens, attachments, context, and settings appear above the conversation; returning restores the same work.
4. **Prove responsiveness.** Build optimized device code, exercise real navigation without submitting prompts, and distinguish render/hitch metrics from network response time.

Nanocodex retains voice, per-agent queues and steering, Stop, remote screens, captured context, scheduled jobs, connectors, history paging, hidden conversations, live previews, and Back. It does not adopt Muse's mascot, main-chat hierarchy, unsupported goals/news destinations, or account policies.

## First-pass implementation and checks

Implementation: `apple/NanocodexInbox/InboxView.swift`, the iOS branch of `apple/NanocodexUI/Sources/NanocodexUI/ChatStyle.swift`, and the relevant native UI journeys.

The simulator Debug app and physical-device Release app build successfully. The first completed iOS 18.2 suite passed 10 of 12 journeys: sidebar/draft/sheets, optional previews, per-session reading position, queues/steering, dock placement, tool details, thinking Markdown/code, failures, generated outputs, and remote-screen zoom/dismissal. Slow creation and cancellation-before-creation checks also passed after replacing their tab-count assumptions with selected-conversation admission checks.

The expanded-activity regression is fixed and its delayed-history journey passes: expansion registers the activity anchor before new geometry arrives, and the nested timeline starts with an explicit step identity. Header and transcript hit regions are constrained independently. At the largest accessibility text size, the visible menu opens under a direct pointer tap, but iOS 18 XCTest reports its wrapper as not hittable; the test now exercises the actual Settings open/return journey instead. The final large-text Settings open/return journey passed. A final rich-thinking swipe/copy test also passed after the hit-region change. All 14 targeted journeys passed across the focused runs.

The iOS 26.5 simulator repeatedly stalled before launching the test runner. A separate iPhone 16 Pro / iOS 18.2 simulator provided functional validation; its glass fallback does not establish iOS 26 visual or performance parity.

The **finished Release build** passed the saved-account iPhone 17 Pro navigation check in 90.600 seconds of XCTest wall time. All three measured iterations reported **zero hitches** and preserved the selected conversation and original draft. Absolute physical memory readings were 42,189.768 / 42,288.072 / 42,271.688 kB. See `2026-09-16-muse-device-metrics.json`. This narrow journey covers drawer, attachment sheet, and settings; it is not a comparison with Muse, a scrolling benchmark, or a touch/network-latency measurement. There is no pre-change performance baseline.

One earlier device run was interrupted: footage shows the sidebar opening and a different conversation becoming selected outside the test steps. That run is retained as diagnostic evidence, separately from the final passing benchmark. No test prompts were sent and no account connections were changed. The final build remains installed on the phone.

### Replay artifacts

The local `review.html` now selects between three recordings:

- `muse-reference.mp4`: the preserved 2:15.55 user recording with 16 reference chapters.
- `nanocodex-device-navigation.mp4`: a 61-second real-iPhone excerpt showing the sidebar, attachment sheet, and settings; captured during the earlier interrupted run.
- `nanocodex-walkthrough-web.mp4`: a remuxed 3:15.29 recorded simulator walkthrough with seven indexed chapters, including compact activity, tool details, switching conversations, attachments, settings, and rich thinking. The original capture is retained as `nanocodex-walkthrough.mp4`. The change of demo fixture is labeled. Pauses and pointer-scroll attempts are retained; native touch scrolling and code copying passed their UI test.

The player was opened and checked in Safari: all recordings appear in the selector, chapters seek and start playback, and pause works. JavaScript syntax and video metadata were also verified. `recordings.json` and `walkthrough-chapters.json` preserve the review index. Private footage and test screenshots remain outside the repository.

### Result bundles

- `/tmp/nanocodex-muse-ui-18.xcresult`: first 12-journey suite, 10 passes and two failures subsequently resolved.
- `/tmp/nanocodex-muse-ui-final.xcresult`: rich activity, generated outputs, thinking code, and failure details after expansion-animation changes.
- `/tmp/nanocodex-muse-ui-anchor.xcresult`: slow creation and cancellation-before-creation passed; remaining failures superseded below.
- `/tmp/nanocodex-muse-ui-regressions.xcresult`: expanded activity retained its exact reading position through delayed older-history insertion.
- `/tmp/nanocodex-muse-ui-accessibility.xcresult`: largest accessibility text, sidebar search/switch, and actual Settings open/return passed.
- `/tmp/nanocodex-muse-ui-scroll.xcresult`: final native swipe through rich thinking and highlighted-code copy passed.
- `/tmp/nanocodex-muse-device-complete.xcresult`: final signed-in physical-device Release performance journey passed.

Build and UI-test logs are under `/tmp/nanocodex-muse-*`; exported evidence is in the local review bundle. `git diff --check` passes. No commit or release publication was made.

## Broader interaction inventory

The sidebar replaces the tab strip, count badge, and drag-to-scrub interaction. Cached conversation state remains an implementation detail. The optional live-preview grid is still useful for inspecting parallel agents; everyday navigation requires no miniature transcripts.

Beyond navigation, this pass brings over the restrained message palette, separate glass header controls, anchored floating dock, compact attachment choices, independent remote-screen surface, and settings that returns to the previous context. Thinking/tool activity now uses a compact summary, a current-action label during work, visible failed-call counts, and an expandable timeline with bounded detail panels. Input, result, rich thinking, and generated deliverables remain distinct.

Further reference-driven work to evaluate separately:

- Appearance: explicit theme choice and live preview, with dynamic color and large-text coverage (02:02–02:08).
- Connector detail: consistent connected/available summaries and return paths across each provider (01:25–01:47).
- Remote control: stronger distinction between observing a screen and taking control, keyboard persistence, and reconnect continuity under real network interruption (00:03–00:32).
- Motion: interactive edge-swipe drawer reveal and reversal, tested against horizontal code/table scrolling and the keyboard (00:48–00:50).
- Performance: long mixed-media scrolling, rapid session revisits, and concurrent streaming on the physical device; compare baseline captures before setting a performance target (00:33–01:01).

These are specific follow-up candidates from the supplied footage, not completed features or measured Muse implementation details.
