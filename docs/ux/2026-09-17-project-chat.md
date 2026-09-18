# Project chat and persistent task threads

The [Main Thread follow-up](2026-09-18-main-thread.md) adds global routing and canonical cross-device project names. Existing local names remain intact; the device-local naming descriptions below record the original project-chat implementation.

Reference: [0xDesigner’s project chat](https://x.com/0xDesigner/status/2100679849249771839), [original walkthrough](https://x.com/0xDesigner/status/2092635269081989572), and [Boris Cherny’s quoted workflow](https://x.com/bcherny/status/2100669598995816511).

The mobile shell uses a project sidebar, a persistent master chat, compact project header, blue user bubbles, unboxed replies, and one composer. A live-task pill opens a native Tasks/Agents sheet. Tasks are durable child threads, with status and drill-down to their messages. Links beside the originating message open delegated work. Back, screens, captured context, connectors and scheduled jobs remain in the header menu.

New project creates a named home backed by a managed agent. Existing conversations remain project homes. Rename changes the navigation name; names persist locally per account. Server-owned parent/root/turn metadata groups spawned agents under their actual master across devices. Tapping a project name opens its main chat. Its separate chevron expands durable child threads in the sidebar; selecting a child opens its normal conversation. Expansion survives closing the drawer, and opening navigation from a child reveals its project automatically. Search matches thread titles and reveals matching children under their project. Ordinary subagents remain within their owning thread rather than becoming project entries. Selecting an agent in the activity sheet also opens that member conversation. Sheets retain the master conversation and its draft; task detail subscribes to bounded background history while visible.

## Simulator recording

Updated sidebar and sheet: [recording](media/project-tree-ios.mp4), [expanded project](media/project-expanded-sidebar.png), [Tasks](media/project-minimal-tasks.png), [Agents](media/project-minimal-agents.png). Recorded from the passing iOS 26.5 sidebar journey after simulator recovery.

[Watch the iPhone simulator walkthrough](media/project-threads-ios.mp4) (iPhone 16 Pro, iOS 18.2).

![Project chat, tasks, task detail, agents and sidebar](media/project-threads-ios.gif)

The clip is trimmed from the passing XCTest journey. Static captures: [chat](media/project-chat.png), [tasks](media/project-tasks-sheet.png), [task detail](media/project-task-detail.png), [agents](media/project-agents-sheet.png), [sidebar](media/project-sidebar.png).

## Persistent delegation

- `spawn_project_thread({id, title, input})` starts a separate durable managed agent and admits its initial task. The child inherits the parent's configuration and invoking turn's capabilities. Connect grants cannot use this capability; disabled delegation remains disabled.
- A stable caller-selected ID produces the same child and turn on retry. A durable account-owned relationship rejects changed input or attempts to reparent a thread. Retrying after an ambiguous admission reuses the existing turn.
- `send_project_thread({agent_id, id, input})` admits a follow-up in a directly delegated child. Stable IDs deduplicate retries and changed input conflicts. `list_project_threads` returns the latest tracked task status; `read_project_thread` accepts an optional exact `turn_id`.
- A parent-owned durable outbox retries ambiguous admissions and checks task outcomes using alarms (five seconds while running, one minute after transport failure). Terminal outcomes enqueue one idempotent internal completion turn in the parent, including after eviction. The parent reads the actual result and continues within the original task scope. This uses no model calls for polling.
- Pending outcomes are bounded to 128. Delivery retains the invoking authorization and retires on authorization-epoch changes. Export is fenced while outcomes are pending.
- Child context is explicit in `input`; no hidden transcript cloning occurs. Nested delegation stays in the same project. A project is bounded to 128 undeleted child threads.

The master handles small requests directly, uses ordinary subagents for bounded helper work, and creates persistent threads for independently progressing work that may be revisited. Follow-ups reuse the existing thread. Parallel coding threads are instructed to use isolated worktrees/branches. Personal memory already covers user preferences; project names remain device-local.

Automatic return applies to turns delegated with the project tools, including follow-ups. Messages sent directly inside a child chat are not automatically reported back to its parent. Completion events are explicitly labelled as internal task outcomes, never new human instructions. The coordinator’s final wording and delegation decisions remain model behavior.

The Tasks/Agents sheet uses compact plain rows, a segmented switcher, and minimal status text. It projects loaded task history. Unknown historical outcomes are labelled History, never assumed successful. Task projections are cached across composer edits. Generated outputs remain in the full conversation; the detail sheet displays task messages.

## Validation

The full InboxCore suite passes (186 tests, five expected skips); five focused project tests cover server lineage parsing, real task identities/statuses, partial history, and local-to-server ID migration. Backend tests cover durable project membership, nested roots, account boundaries, changed-input conflicts, retry after failed admission, and rejection of tool authority overrides. Type checking and a Worker-only Wrangler dry run pass. The broader CI also has failures in unchanged Rust code (redundant clone, Windows screen cfg, voice timing). The unrelated connector-provider catalog test fails because its expected list omits Link; the same failure was reproduced against unchanged HEAD.

The prior seven simulator tests passed (three project journeys and four drawer/menu/back-navigation regressions). The expandable sidebar adds a journey covering disclosure, direct child navigation, selected state, independent drafts, child-title search, and both minimal sheet tabs. The new tests verify: Tasks/Agents navigation retains the draft, and a named project survives app relaunch. The simulator journey uses explicitly opted-in Debug fixtures with representative master/child conversations. It verifies navigation and draft preservation; it does not claim a live model chose to delegate. Signed-device delivery and production deployment are not part of this PR.

Runtime integration checks exercise actual managed child admission, replay after lost acknowledgment, parent eviction, success/failure/cancellation wakeups, duplicate-result deduplication, retained authority, revocation, and the pending-work bound. Model execution is held at the durable retry boundary in these tests; no production model or deployment is used.

## Expandable sidebar follow-up

The final iOS 26.5 sidebar journey passes: expansion, child selection, selected state, separate drafts, collapse, child-title search, and both compact sheet tabs. The current screenshots and XCTest recording include the final flat row backgrounds. Earlier launch failures were recovered by restarting CoreSimulatorService without erasing device data; the passing run uses XCTest as the sole recording owner.

## Watching a thread's desktop

Choose **Screen** from the conversation's top-right **⋯** menu. A compact panel opens above the transcript; the header stays uncluttered and the composer remains available. Choose a desktop once for that thread. The choice is saved locally per account and thread using machine/surface IDs, never a display-name guess or a saved signaling generation. A restarted desktop is resolved through current discovery. An unavailable selection stays saved and is shown offline rather than switching to another desktop.

The panel header shows the chosen desktop, screen options, expand/collapse, and close. Expanding fills the transcript area while retaining the composer. Switching threads closes the previous stream and restores the other thread's chosen desktop when its panel is open. Viewing sends no input. **Screen options → Screen controls** opens the existing full viewer on the selected desktop, where **Take control** remains explicit. The passive connection closes while those controls are open; returning recreates it without retaining control. Backgrounding suspends viewing.

Screen discovery currently exposes account-wide desktops without reliable project-thread ownership, so this change uses explicit per-thread selection. It does not claim to automatically follow every desktop the agent chooses. The existing RemoteViewer/RemoteCanvas transports carry the live desktop pixels and agent cursor activity; this UI does not synthesize playback.

Screen validation: the native Remote package builds and its desktop-identity test passes. The final integrated simulator app/test build passes. The dock journey now passes menu entry, desktop selection, Watching state, view-only display, expansion/collapse, per-thread panel restoration, and draft preservation. It caught and fixed a SwiftUI accessibility issue where the panel identifier propagated to its controls; the panel now contains distinct accessible buttons.

[Watch the Screen journey](media/thread-screen-ios.mp4), [docked viewer](media/thread-screen-docked.png), [expanded viewer](media/thread-screen-expanded.png). The explicitly labelled local fixture uses real discovery, WebSocket JPEG delivery, and canvas rendering. It does not demonstrate a production remote desktop or an agent executing remote input. XCTest owns and retains these recordings; no parallel simctl recorder is used.

After integration with master, managed type checking and 15 focused project-thread, thread-run, and file-download tests pass. Simulator/process reliability follow-up: [PR #382](https://github.com/gakonst/nanocodex/pull/382).

The full-controls fixture journey also passes through **⋯ → Screen → Screen options → Screen controls**, including sheet expansion, canvas zoom, repeated selection, swipe dismissal, and draft restoration. It was rebuilt after adapting the older direct-button test entry point.
