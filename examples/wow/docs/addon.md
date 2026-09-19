# WoW addon

`addon/Nanocodex` is a manual context capture addon. It displays a compact movable panel and a copyable JSON snapshot. `/nc projects` provides a native snapshot browser for project/chat selection and exports reviewed management requests to the companion. It is a manual clipboard bridge, not a network connection.

## Install

Copy the entire `Nanocodex` directory to your WoW flavor's `Interface/AddOns/` directory, for example `_retail_/Interface/AddOns/Nanocodex`. The TOC files must sit immediately inside that directory, not another nested copy. Restart the client if necessary, enable **Nanocodex WoW** in AddOns, and run `/nc show`. No dependencies or API keys are required.

Retail and Classic paths are selected by feature detection. The TOCs provide baseline interface declarations: Mainline `120000`, Era `11509`, TBC `20505`, and Mists `50500`. These are compatibility targets, not evidence of testing every patch. If your current client marks it outdated, first inspect `/dump select(4, GetBuildInfo())`; update the appropriate TOC after checking compatibility, or opt into loading out-of-date addons for a local test. Classic Beta 1.60.1 build 69913, interface 16001, was validated in the running Omarchy client.

## Controls

Drag the panel background to move it. Its position and visibility use account-wide SavedVariables. Escape or the close button hides it.

| Command | Action |
| --- | --- |
| `/nc` or `/nc toggle` | Toggle panel |
| `/nc show`, `/nc hide` | Show or hide panel |
| `/nc capture` | Capture a snapshot in memory |
| `/nc export` or `/nc copy` | Capture and open the JSON copy window |
| `/nc projects` | Browse a pasted workspace snapshot and select a chat |
| `/nc reset` | Center and show the panel |
| `/nc clear` | Clear the stored snapshot in memory |
| `/nc help` | List commands |

`/nanocodex` is an alternate prefix. Use slash commands in game; desktop Alt+Shift+N/P/V opens the companion, projects, or voice. Gameplay bindings are preserved.

In the export window, use Ctrl+A and Ctrl+C, then paste into the external companion's context input. This is a manual clipboard operation; the addon does not set the system clipboard. Review the text before sharing: it includes character/realm and target names, location, and quest titles.

## Context contract v1

The copy window contains one JSON object, UTF-8, with `schemaVersion: 1`, `source: "nanocodex-wow"`, and `addonVersion: "0.1.0"`. Optional unavailable fields are omitted, never fabricated. Containers remain objects; `quests` is always an array, including when empty. Consumers should tolerate unknown fields.

| Field | Type / semantics |
| --- | --- |
| `capturedAt` | Optional Unix epoch seconds from server time, falling back to client `time()` |
| `client` | Optional `version`, `build` strings; `interface`, `projectID` numbers |
| `character` | Optional `name`, `realm`, `class`, `classToken`, `faction` strings; `classID`, `level` numbers |
| `specialization` | Retail: optional `index`, `id`, `name`; Classic talent fallback: `name`, `talentTree`, `pointsSpent`. No fake retail spec ID |
| `location` | Optional `zone`, `subzone` strings and `mapID` number |
| `target` | Optional `exists`, `isPlayer` booleans; `name`, `class`, `classToken` strings; `level` number (may be -1 for an unknown boss level) |
| `quests` | Up to 500 visible log entries excluding headers: optional `id`, `title`, `level`, `complete` |
| `limitations` | Human-readable string describing snapshot limitations |

`complete` preserves the client API value: modern boolean, or legacy numeric quest completion status (such as 1 complete, -1 failed, nil omitted). This is not a quest history or guaranteed full quest log: collapsed Classic headers, hidden quests, and unsupported APIs can omit entries. The addon does not expand headers, select quests, or change tracking to collect data. Classic characters without points in a talent tree have an empty specialization object. On ties, the first highest-investment tree is reported.

API calls are guarded with `pcall`. Where available, `issecretvalue` filters restricted return values before comparisons, conversion, or export. Restricted target information is omitted rather than bypassed. A failed overall capture leaves the previous saved snapshot intact and asks the user to retry outside combat.

## SavedVariables and boundaries

`NanocodexWowDB.lastContext` contains the most recent snapshot table; `NanocodexWowDB.lastExportJson` contains its exact JSON string. `position` and `hidden` store panel preferences. Only one snapshot is retained. There is no automatic polling or automatic capture on logout: capture explicitly first.

WoW owns SavedVariables disk writes, normally on clean logout or `/reload`. Capturing changes Lua memory immediately, **not the on-disk file**. Crashes can lose changes. The account-wide file is normally `WTF/Account/<account>/SavedVariables/Nanocodex.lua` under the active WoW installation/flavor. An external reader must treat it as stale until WoW flushes it. Never execute that Lua file as trusted code in a companion; parse the needed data safely, or use the manual JSON copy flow.

The addon cannot open network connections, read project files, launch programs, or maintain a live file bridge. It displays project lists explicitly copied from the companion. Ask/create/rename requests are exported for review outside WoW; replies are pasted into `/nc reply`. It does not send network requests, cast spells, move the character, target units, or automate quests or combat. Clearing a snapshot only affects in-memory state until the next SavedVariables flush and cannot erase copies already exported elsewhere.

## Validation

From the repository root:

```sh
luac -p addon/Nanocodex/Context.lua
luac -p addon/Nanocodex/Core.lua
lua addon/tests/context_test.lua
lua addon/tests/ui_test.lua
```

The Lua mock test covers missing APIs, modern quest/spec capture, Classic talent and quest fallbacks, JSON escaping, restricted values, and API exceptions. It does not simulate Blizzard's secure execution environment or prove UI compatibility.

Before release, test in each supported game flavor: addon loading without Lua errors; panel drag/reset and hide/show; copy/select/scroll of a large quest snapshot; actual Ctrl+C paste; empty and selected targets; capture in/out of combat and restricted encounters; binding assignment without disturbing existing keys; and persistence after `/reload` and a clean logout. Check JSON parsing in the external companion, and check that `/nc clear` persists after reload.

## API research

Implementation checked against Blizzard-generated documentation distributed in the client UI source (public Gethe mirror): [quest APIs](https://github.com/Gethe/wow-ui-source/blob/live/Interface/AddOns/Blizzard_APIDocumentationGenerated/QuestLogDocumentation.lua), [specialization APIs](https://github.com/Gethe/wow-ui-source/blob/live/Interface/AddOns/Blizzard_APIDocumentationGenerated/SpecializationInfoDocumentation.lua), and [unit APIs / restricted identity return values](https://github.com/Gethe/wow-ui-source/blob/live/Interface/AddOns/Blizzard_APIDocumentationGenerated/UnitDocumentation.lua). These moving source references are API evidence, not an in-game test certification. The Classic global fallbacks remain subject to per-flavor testing.

## Project/chat snapshot

Click **Copy workspace for WoW** in the connected companion. In `/nc projects`, paste and import the workspace. Select a project and chat, type a request, then export it. Paste into the companion composer; account/project membership is checked before loading the draft. Press Send explicitly. Create/rename exports open a review dialog and never execute on paste.

Snapshots use `ncw1` followed by tab-separated P/T rows with percent-encoded UTF-8 fields. They are limited to 1,000 rows and 256 KiB; malformed input is rejected atomically. They can become stale. Copy a new snapshot after account changes. No credentials are included.

Use `/nc game <question>` or **Game / Copy** to ask about WoW independently of the selected work chat. `/nc ask` uses the selected project/chat, or falls back to a game question when there is no project selected. If only a project is selected, choose its chat before asking about work.

## Automatic bridge connection

The updated addon starts its chord receiver automatically after loading when combat,
keyboard focus, modifier, and reserved-binding checks allow it. Startup waits and
retries while temporarily blocked. The local automatic bridge validates two fresh,
matching pixel packets before sending input. A linked carrier is separate from an
accepted cloud request and a completed reply.

The companion displays local bridge status separately from account connectivity.
An authenticated workspace is not evidence that WoW has received a reply. Old
carrier geometry and session numbers must never be reused without validation.

The combined installer waits for the first `/reload` before starting the local
connection service. Later reloads or window-geometry changes currently stop an
active bridge with its journals retained; automatic cross-session restoration is
not yet implemented. Do not resubmit an uncertain request to recover it.
