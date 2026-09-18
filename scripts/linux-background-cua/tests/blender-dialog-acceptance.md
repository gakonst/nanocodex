# Blender background dialog acceptance — 2026-09-18

Base: 9e9c9dd4386848bbe24c1ca4e1f43731697d5cab. Scope: native Wayland newly mapped toplevels belonging to a reserved independent lane's client. Both window.openEarly and window.open set noInitialFocus only while a live bound reservation exists, the lane is usable, and neither primary keyboard nor pointer belongs to that client. Explicit primary focus, unrelated clients, and same-client lane exclusion remain intact. No linux_background.rs changes.

## Verified

Isolated lab: /srv/nanocodex/workspace/background-cua/blender-acceptance-v5 on Omarchy; runtime directory /dev/shm/bv5. Blender 4.5.3 native Wayland; Hyprland 0.56.2. Exact compiled/runtime ABI hashes matched:
efb50993780079460b0cbed1363e2166a2de1d9f_aq_0.15_hu_0.14_hg_0.5_hc_0.1_hlg_0.6.

Recovered plugin source matched the worktree byte-for-byte (input_experiment.cpp SHA256 ccdf9c8aaec90956be07bf71d28974c6dfcfa18f6e9b42ce830b8d566205a594). Isolated v3 build completed; resumed incremental build and both CTest cases passed. Plugin SHA256 774e1212996021d553d07076a1b03c100f682cbc8e4e231ce38f2b73637ef194.

Used corrected function-key-fix/current-nanocodex-computer, SHA256 66da0d5959f4d61b3893e5c92ec67de210e286edd79441626d0f697005a92421. Its current-build.log reports successful release build; current-tests.log reports eight focused runtime tests passed. The stale earlier runtime was not used for these runs. No shared Cargo target build was started.

Public CUA getApp / pressKey / click / drag / typeText / getScreenshot actions passed:
- Cube transform, orbit, pan, text entry and F2 rename.
- Ctrl+Shift+S Save As mapped a same-client dialog while primary keyboard focus and cursor stayed on the foreground GTK entry.
- A second dialog lane was refused with agent_target_busy.
- js_reset released the first window reservation. Public listApps discovered the exact dialog address; getApp bound its full identity.
- Public CUA clicked the screenshot-observed Cancel button and the dialog disappeared.
- Reset and rebind to the main Blender window; a subsequent transform passed.
- All 143 concurrent primary keystrokes reached the foreground entry, which retained focus; primary cursor position was unchanged.
- Explicit compositor primary focus to Blender succeeded; subsequent background input refused primary_target_busy.
- A newly launched unrelated client received normal initial focus.

The Blender Python observer reads scene and view state (plus disables the startup splash); it does not perform the claimed input actions. hyprctl handles fixture placement, primary focus setup and state observation. The foreground keyboard helper supplies controlled primary-seat keystrokes.

## Limits and retained failed runs

In v3, Escape was acknowledged for the exact dialog but did not dismiss it. Do not claim Escape dismissal works. The verified dismissal is a CUA click on Cancel. In v4, Cancel worked but the follow-up transform assertion failed because its click deselected the objects; v5 explicitly selects them before transforming. Failed receipts remain in their original lab directories.

This proves the covered Blender workflow with a GTK foreground fixture, not actual WoW-in-world coexistence on the installed user compositor. Root owns that integration. XWayland dialog behavior and every possible toolkit popup are not covered.

## Evidence and cleanup

Final receipts: /srv/nanocodex/workspace/background-cua/blender-acceptance-v5/{summary.json,receipts.json,run.py,observe.py,desktop-dialog.png,capture-23-0.png,cleanup.json}.
Durable copies: /brain/outputs/blender-dialog-acceptance/.
Reproduction requires the existing isolated-lab tools and fixture layout used by start-lab.py; run.py uses the corrected current runtime and the captured env.json. Screenshot coordinates are specific to that fixture layout.

All owned fixture processes terminated. Plugin status before compositor teardown showed all eight lanes unreserved, no active leases, no held buttons/keys, no drag, and no pointer/keyboard focus. v3/v4/v5 owned nested Hyprland and labwc processes were stopped. No installed desktop plugin unload, desktop restart, live dialog, or root Cargo-target mutation occurred.
