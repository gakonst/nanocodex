# Desktop browser workspace — September 9, 2026

Baseline: master `b1b563a0`. Work performed in an isolated worktree; unrelated
Rust, managed-worker, and tool changes were preserved. Design reference:
[Mitchell Hashimoto’s Superlogical demonstration](https://x.com/mitchellh/status/2097424868203758046),
visually inspected in the browser: compact top tabs and quiet split-pane chrome.

## Implemented behavior

Top browser tabs replace the permanent sidebar. A tab holds one agent or a
persistent tree of horizontal/vertical agent splits. Dividers resize native
editor hosts without recreating them. Each group remembers its last focused
pane; tabs reorder as groups, close as groups, and reopen with their layout and
drafts. Pane removal leaves that agent available in its own tab. Existing
account-scoped drafts, selected models, review cursors, pending messages, and
legacy layouts survive migration. History, Hands, screens, connections, and
settings remain reachable through search, the overflow menu, and shortcuts.

The background control panel previously used a variable-width text label that
grew with Hand and running-agent counts. It opened through `⌘⇧P` in the installed
baseline, establishing that the panel was being created. The new item uses a
22-point square icon; tooltip/accessibility text retain those counts. This
removes that label’s menu-bar space demand. We did not obtain a screen capture
proving that the old item was specifically hidden by the camera housing.

## Native measurements

Both comparison runs used the same machine, Debug native test host, 1200 × 840
window, fresh process, no network, and the same retained-view fixture. These are
local layout/editing costs, not process launch or server response latency. Tab
switch statistics cover 20 switches. Individual runs are evidence, not a
statistical performance guarantee.

| Measurement | Baseline | Browser workspace |
| --- | ---: | ---: |
| First native layout | 256.5 ms | 152.6 ms |
| Native editor input | 29.1 ms | 19.2 ms |
| Tab switch median | 16.2 ms | 14.8 ms |
| Tab switch p95 | 22.2 ms | 18.7 ms |
| Unchanged 800-event snapshot | 19.5 ms | 19.1 ms |
| Streaming snapshot median | 44.2 ms | 43.4 ms |

The mixed three-pane fixture measured divider resizing at 7.5 ms median and
9.6 ms p95 across 30 sampled frames, while asserting native host identity.
Screenshots cover the three-pane layout and the existing transcript, history,
queue, narrow-window, control-panel, and settings journeys.

## Real native voice

Signed native tests used the existing subscription account and synthetic speech
through BlackHole. Each disposable agent was removed and the prior input device
restored. No API-key setup or voice backend change was introduced.

At 23:38 UTC, three consecutive call starts succeeded in 2267, 1675, and 1507 ms;
cleanup took 404, 398, and 380 ms. The full speech journey at 23:40 UTC passed:
call ready 1931 ms, first user transcript 4051 ms from call start, first detected
output energy 12693 ms, first assistant transcript 13289 ms, interruption quiet
1048 ms from the interruption clip starting, stop 104 ms, restart 1557 ms. The
counting clip itself starts after call readiness and takes several seconds to
speak. Output energy is WebRTC telemetry, not a physical speaker measurement.
The replacement answer was thirteen and the follow-up answer was blue.

These tests verify call readiness, transcript delivery, output, interruption,
follow-up, stop, and restart. They do not attribute network/model delays to the
new workspace or claim an improvement in backend voice latency.

## Validation and evidence

- All 36 native protocol/hosted UI tests passed, including split geometry,
  editor identity, per-agent queue targeting, viewport retention, group ordering,
  close/reopen, focus restoration, saved layout, and legacy migration.
- All 35 desktop-runtime tests passed; focused persistence tests were rerun after
  adding remembered pane focus.
- Signed native voice ownership and full speech tests passed. Independent restart
  evidence covers three calls. Other opt-in account diagnostics were skipped.
- The signed Release build succeeded.

Raw sanitized measurements are in
[`DESKTOP_BROWSER_DOGFOOD_2026_09_09.json`](DESKTOP_BROWSER_DOGFOOD_2026_09_09.json).
Generated screenshots and full native reports remain in the isolated worktree’s
`macos/build/evidence/`; build/test logs use `/tmp/nanocodex-browser-*`.

After inspecting the baseline app and successfully opening its control panel,
computer-use automation began returning `cgWindowNotFound` for both Nanocodex
and Finder. Hosted native windows and rendering tests continued to work. This
limits claims about visibility on the user’s current desktop; it is not evidence
by itself of an application-window defect.

A direct background-popover check at 23:42 UTC could not show the panel. AppKit
reported `elementWindow(25) is lower than shield(2001)` and could not vend the
status-item scene. The unlocked-desktop check is now separate from the ordinary
hosted suite (`NANOCODEX_DESKTOP_MENU_BAR_LIVE=1`). The user was asked to unlock
the Mac. This visual background check remains pending; the failed attempt is
not counted as passed and no app lifecycle change was inferred from it.


## Installation

Installed the signed Release at `/Applications/Nanocodex.app` on
2026-09-08 at 23:47:58 UTC. Strict deep signature verification passed before and
after replacement; the installed executable matches the build artifact:
`5e5458c517da16d9bc3783d237350b13f7b47e8c9c1707382697c7588bab7eba`.
The previous bundle is retained at
`/tmp/Nanocodex-before-browser-workspace-20260909.app`.

The installed application and its bundled runtime both launched and remain
running. Normal preferences still contain the same seven open agents and no
pending messages. No account, durable conversation, or unrelated worktree data
was removed. The post-install computer-use attempt remained blocked by the
shielded desktop; visible menu-bar placement, actual mouse interaction in the
installed build, and background window reopening remain unverified.

A late narrow-window check reproduced the focused lower pane being partially
outside the viewport. The scroll view now reveals the focused pane after window
resize and child layout. The 820 × 600 hosted regression test passes, including
full visibility of that pane. The final keyboard/pane/send fixture also passes.
Tab groups retain unread/error indicators, and tab selection uses a native
button independent of the close button.

No Workers or mobile app needed deployment for these desktop-only changes.
