# Nanocodex WoW

> This isolated integration source includes an automatic addon/desktop bridge with incremental replies. See [integration validation](docs/bridge-integration-20260919.md) for tested behavior and live gaps. The copy/paste instructions below describe the manual fallback; they do not establish live carrier delivery.

A WoW addon and local desktop companion for advancing Nanocodex agents while playing. The companion offers projects and persistent threads, separate game conversations, lore and progressive hints, build/PvP/PvE guidance, and voice dictation. New game conversations use Luna. Existing agents keep their configuration.

## Install the automatic connection update

Run `bash scripts/install-all.sh "/path/to/the/running/WoW/client"` from the gaming desktop user's terminal. When it prompts, enter `/reload` in WoW, then return to the installer and press Enter. The installer updates the desktop companion, addon, and automatic connection service together. The addon starts its guarded chord receiver after loading, and the companion discovers the session and pixel geometry without manual calibration commands. `/nc bridge off` saves an explicit opt-out.

The companion reports account and WoW connection states separately. Automatic discovery waits when WoW is unfocused or its carrier is obscured. Ambiguous input stops with its journal retained; it is never silently retried. The initial installer starts discovery only after the updated addon has loaded. Later session or window-geometry changes stop the bridge and retain its journals for reconciliation; seamless reconnection across reloads is not implemented. Current live proof covers cloud-to-dispatcher streaming; visible in-game delivery is still being validated.

## Run on Omarchy

Requires Python 3.10+, Chromium, and the Nanocodex CLI. From the **gaming desktop user**, run:

```sh
bash scripts/install-desktop.sh
bash scripts/install-addon.sh "/path/to/World of Warcraft/_retail_"
/opt/nanocodex/current/nanocodex2 login
~/.local/bin/nanocodex-wow
```

The installer copies the app to `~/.local/share/nanocodex-wow`, starts a user systemd service at `http://127.0.0.1:17840`, and adds an application launcher. It adds only unoccupied Alt+Shift+N/P/V Hyprland bindings, preserving existing shortcuts. Re-run it after updates. No root access is required.

| Shortcut | Action |
| --- | --- |
| Alt+Shift+N | Open/focus companion and message composer |
| Alt+Shift+P | Open/focus project selection |
| Alt+Shift+V | Start/stop dictation in the companion |
| Enter / Shift+Enter | Send / newline |
| `/nc` in WoW | Toggle addon panel |
| `/nc export` in WoW | Copy current context into the companion |

Use `/nc show` in WoW. Desktop Alt+Shift+N/P/V opens the companion, projects, or voice without overwriting gameplay keys. See [addon documentation](docs/addon.md) for installation, supported client API branches and context format.

## Account and projects

Sign in with the official CLI **as the same desktop user running the companion**. Connection settings explains the command and checks connectivity. The backend uses the documented private CLI credential store; it never serves credentials to the browser. No account keys belong in this repository.

The backend reads real account-owned agents and project membership from Nanocodex. Existing conversations can be resumed. A new unassigned conversation is created atomically with its first message. Selecting a project without a thread resumes the project root; it does not silently spawn a child task. Game conversations are kept separate from selected work threads.

The skill in [skills/wow-assistant/SKILL.md](skills/wow-assistant/SKILL.md) can be used in a standard Nanocodex thread. The desktop companion is an independent client, not a modification of the Nanocodex core.

## Voice

Local speech recognition uses an optional whisper.cpp runtime; see [voice setup](docs/voice.md). Recorded speech becomes an editable draft and is never sent automatically. Browser dictation is a labelled fallback and may not work in Linux Chromium. Read-aloud depends on installed browser/system voices. Microphone permission and an actual microphone are required for live use.

## Development

```sh
python3 server.py
python3 -m unittest discover -s tests -v
lua addon/tests/context_test.lua
lua addon/tests/ui_test.lua
```

The server binds loopback only, validates Host/Origin, limits request sizes, and uses no third-party Python packages. External text is rendered as text, not HTML. Sends are not automatically retried after an uncertain result.

## Boundaries

This is a manual companion: no combat automation or memory reading. Addon context must be copied explicitly; SavedVariables are written by WoW on logout/UI reload. Current build advice requires the agent to verify edition/patch and consult sources. Local tests do not prove in-game compatibility, live account connectivity, or microphone quality.

## Change keybinds and HUD settings

Use the **Settings** tab to write requests such as “bind map to Shift+M”, “unbind Shift+M”, “set UI scale to 85%”, or “enable enemy nameplates”. These common requests are planned locally and work without a Nanocodex login. The plan includes a preview command, an explicit apply command, and undo. Copy the command into WoW chat. The addon checks allowed actions and values, reports displaced bindings, and defers protected changes during combat. It does not execute arbitrary Lua from an AI response.

The supported setting list is deliberately explicit in the UI. Complex HUD redesigns can be discussed with the game assistant; a chat answer is not evidence that a setting was changed.

Browser regression checks (optional development dependencies):

```sh
bun install
# with server.py running at port 17841, or set WOW_TEST_URL
bun test:browser
```

## Manual fallback in-game use

The original project targeted Classic Beta 1.60.1 (interface 16001); this isolated integration has not been validated live. Install into the client that is actually running, then `/reload`.

1. `/nc show` opens the native question/context panel. `/nc ask your question` captures a question and character context.
2. Copy the selected JSON, open the companion with Alt+Shift+N, and paste into its composer. Choose a mode and Send. This uses the account key on the local server.
3. Click **Copy reply for WoW**. Back in WoW, `/nc reply` opens the long-reply input; paste and click **Show answer**.

This first bridge is explicit copy/paste, not live networking inside WoW. Projects and voice currently run in the desktop companion.

To import an existing account key privately, run `python3 ~/.local/share/nanocodex-wow/scripts/connect-account.py` as the desktop user. Input is hidden and passed to the official CLI over stdin. Refresh the companion afterward. Never put an account key in Lua or paste it into game chat.

## Project and chat management

The companion supports creating real empty Nanocodex conversations, local project grouping and rename, chat rename, close/restore, chat search, per-chat session drafts, earlier history, and active-turn stop/steer. Account conversations and messages are shared with Nanocodex; names, local grouping and closed state are stored privately on this computer, scoped to the account origin and credential. These local organization changes do not sync to mobile. New chats use Luna; existing agents keep their settings.

**Copy workspace for WoW** exports a snapshot for `/nc projects`. Select a project/chat in game, compose a request, copy it to the companion, review, and send. Create/rename actions imported from WoW always require explicit review. The addon has no live networking; this is not full mobile feature parity (attachments, approval cards, and realtime voice are not implemented). A local account key remains necessary to verify real account operations.
