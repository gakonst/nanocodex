# Mobile and desktop dogfood — 2026-09-09

Follow-up to `VOICE_DOGFOOD_2026_09_08.md`, based on master `1a9e7c65`. Device: connected iPhone 17 Pro, Release build, signed-in subscription account. Times below are UTC on September 8 (September 9 locally). Unrelated CLI/TUI and remote-screen work was preserved.

## Confirmed fixes

- Mobile refreshes no longer wait for a whole four-agent batch. Completed reads publish immediately; vacant slots refill, with focused/voice/pending/running work first. Every roster agent remains covered: roster timestamps do not reliably version replies/settings. Overview history records only applied event cursors. Unchanged refreshes no longer republish the inbox deck: an actual Combine fixture measured 178 unchanged response publications before, zero after (179 including the final priority pass before, zero after). Actual deletion/priority changes still publish once, with navigation/review history preserved. This does not measure physical redraw counts or explain device heat.
- Repeated unchanged cancellation receipts back off from one second to a maximum of fifteen seconds. Foreground/relaunch revalidates immediately, terminal events finish immediately, and persisted Stop intent never expires. Task ownership prevents an old waiter from unregistering its replacement; exact terminal IDs are checked.
- iOS continued-processing task notifications retain conversation identity, refresh initially untitled conversations from existing roster metadata, and report Completed/Failed/Stopped/Paused accurately. Late expiration is fenced to the original run. Restored task outcomes and unread attention use lifecycle events, not subsequent internal activity.
- Desktop draft model/reasoning/Fast choices survive full quit/relaunch and are used for first submission. Reviewed historical failures no longer leave stale orange/Needs attention indicators or obscure a new running turn.
- Account HTTP relay aborts upstream requests when callers disconnect after completing the upload. Egress audit retains the numeric upstream rejection status. These changes do not establish the cause of earlier voice startup timeouts.

## Phone refresh and polling measurements

Same 75-second foreground observation, temporary opt-in URLSession metrics; no request headers, query strings, or bodies collected. Hook removed from final product source. These are observed network completions rather than rendered frame timings.

| Measurement | Baseline 22:12:10 | Refresh pool 22:20:06 | Pool + backoff 22:27:40 |
|---|---:|---:|---:|
| First 100 distinct state reads | 42.266 s | 15.851 s | 12.187 s |
| First 180 distinct state reads | 71.405 s | 43.346 s | 35.763 s |
| Exact stuck-turn GETs | 53 | 55 | 7 |
| State reads completed | 227 | 371 | 489 |
| History reads completed | 168 | 176 | 174 |

Account size grew from 180 to 183 observed agents during owned test creation. Network conditions varied; the table does not isolate a percentage speedup or show fewer total requests. Refilled scheduling completes more work per window. The controlled HTTP fixture isolates the batching defect: first callback **486.5 → 25.6 ms**, last fast agent **535.0 → 75.7 ms**, all nine IDs covered, maximum four agent operations, no post-cancellation callbacks/additional reads. Initial operations can each issue state and history requests.

## Voice and text evidence

Physical phone greeting at 22:22:02.709:

| Milestone | Timestamp UTC | Relative latency |
|---|---|---:|
| Tap | 22:22:02.709 | — |
| Voice ready | 22:22:08.463 | 5.754 s from tap |
| Speech fixture ended | 22:22:13.978 | — |
| Input transcript | 22:22:14.049 | 0.071 s after speech |
| Realtime turn created | 22:22:14.052 | 0.074 s after speech |
| Reply transcript | 22:22:14.856 | 0.878 s after speech |
| Native audio output started | 22:22:15.309 | 1.331 s after speech |

`/calls` returned 201 in 5.150 s, including 5.132 s waiting for response; that path dominated startup. Audio timing is the native playout diagnostic, not an external microphone recording. Greeting needs no main-agent memory delegation. Earlier memory-grounding checks remain documented in the prior report.

Desktop same-agent start/stop/restart passed three times: startup **2.135 / 1.611 / 1.741 s**, cleanup **0.385 / 0.395 / 0.348 s**, default deadlines, no speech fixture. Input device restored afterward.

Two live text checks exceeded their 90-second UI reply deadlines, and read-only durable metadata showed the response was not yet available. Queued cancellation/steering reached the replacement model call, which completed after **239.872 s**. A fresh simple reply request entered the model stage **0.514 s after admission** and completed after **276.021 s** in that stage. `model.call.started` precedes model factory/connection work, so this does not establish provider-only latency. No model transport change or production timeout increase was justified. Actual compiled-WASM cancellation fixture verified a fresh replacement WebSocket and completion, request received 30 ms after cancellation.

## Retained cancellation blocker

The repeatedly polled retained turn cannot hydrate runtime revision 353: required Cloudflare durability chunks are missing. Alarms continue retrying; this is not an alarm that silently stopped. Numeric capacity showed 13,056,000 bytes (51 full 256,000-byte chunks) retained. Runtime now reports expected/found chunk counts without payloads.

Real Cloudflare SQLite tests validated 13.6 MB/54-chunk prior state, 14.6 MB/58-chunk replacement, failure at the observed 51-chunk boundary, atomic rollback, subsequent success, and stale-owner fencing. These did not reproduce the corruption cause. No matching local backup was found. The retained conversation was not deleted, reset, or falsely marked cancelled; repair requires verified recoverable data.

## Validation and remaining limits

Core tests, Release builds, actual task-runtime fixtures, hosted AppKit lifecycle checks, and live phone flows were used. The phone passed voice greeting, repeated tab/reply retention, navigation/attachment menus, scheduled-job loading and relaunch, and all three idle observations. The two text deadlines above remain recorded as failures rather than being relabeled successes after later completion.

Mobile notification validation concerns iOS continued-processing task UI. Neither Apple client currently implements APNs/local-notification delivery or notification-tap routing; this pass does not claim those features exist.

Combined review found and fixed an overview interaction: pending controls are reconciled with the applied history before its cursor advances. An actual-method fixture failed before and passed after for completed/failed/cancelled outcomes, exact agent/turn identity, replay, and state-only safeguards.

Focused totals: InboxCore 89 tests (3 skipped, zero failures); native protocol 35 passed; desktop runtime 34 passed; account web 123 passed; browser/managed voice 29 passed; egress 12 focused tests plus typecheck; compiled-WASM cancellation 4 passed; managed admission 8 and cancellation projection 25 passed. Relevant Release builds and strict signatures passed. Task-runtime fixtures verified Completed/Failed/Paused/Stopped, dynamic title, and remote Stop only for explicit cancellation.

The new physical system-notification probe did **not** pass: two baseline attempts ended with a killed XCTest runner; the candidate reached background runtime but could not locate its title in Notification Center. The screenshot shows a grouped stack of older generic Nanocodex task failures and iOS charging paused for high temperature. Repeated device testing was paused for cooling. A subsequent thermal-guarded probe reported serious thermal state (2) and skipped in 18 ms without opening the app; the app process was then closed. This does not establish that the current notification title is absent inside the group. Actual task-runtime title/outcome fixes are validated, but OS grouping/terminal presentation remains a physical verification limit.

Apple's continued-processing API invokes expiration for both system expiry and user Stop and exposes only a success boolean for completion. OS-owned terminal UI can say Task Failed even when the app's detail is Stopped. This pass does not falsely report unfinished work as successful to suppress that UI. See [Apple's continued-processing documentation](https://developer.apple.com/documentation/backgroundtasks/bgcontinuedprocessingtask).

Desktop installed app was inspected in the real UI: connected account, restored tabs/settings/history, search, task control panel, and an owned task's review count changing 3→2 with its row becoming Idle. Installed executable SHA256: `97b071e3a5fad559d11db13838c95dbd644613d8e51c90dfdb665aaf159b9f75`.

Measurement artifacts: `/tmp/nanocodex-idle-{before1,after1,backoff1}-measurements.json`, `/tmp/nanocodex-round2-phone-voice-measurements.json`, `/tmp/native-dogfood-report.md`, `/tmp/nanocodex-pending-cancellation-root-cause.md`. Device screenshots remain local because system Notification Center contains unrelated private notifications. Backend release `88d5acc2` completed, account last at 22:47:08 UTC: egress `da14aa68-a00e-44ea-8ec2-a3f15b6b8132`, managed `d5cd6a0a-1556-4b9c-b6f8-d433d167dd09`, account `23d2b8ce-af19-4b99-bfb7-5194bc4ab0b3`, all 100% with exact deployment messages. Served browser/WASM greeting, provider handoff, terminal failure and duplicate checks passed at 22:47:49. The later inbox-deck change is iOS behavior. Independently committed remote-screen work on shared master is outside this report's validation claims. Final device receipts are kept in `/tmp/nanocodex-mobile-round2-release.json`.
