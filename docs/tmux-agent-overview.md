# tmux agent overview

The optional Python 3 popup lists every pane in every session on the current tmux server. Managed `nanocodex2` terminals publish pane-local status and their latest user prompt every two seconds. The popup joins that metadata to titles and activity from `nanocodex2 list`; ordinary panes retain their command and terminal title. No pane output is scraped and the popup makes no model calls.

Add this opt-in binding to `~/.tmux.conf`, replacing `/absolute/path/to/repo` with your checkout path (tmux 3.2 or newer):

```tmux
bind-key O display-popup -E -w 90% -h 85% 'python3 /absolute/path/to/repo/scripts/tmux-agent-overview.py --client "#{client_name}"'
```

Reload your configuration when ready, then press your tmux prefix followed by Shift-O. Arrow keys select a pane; Enter switches the originating client to it across sessions and windows. Type to filter title, status, prompt, or location; Backspace edits the filter; Esc closes. Nothing is installed automatically. You can also run the script directly inside tmux. Use `--nanocodex /absolute/path/to/nanocodex2` if the binary is not on PATH.

The current pane list refreshes each second and account summaries every five seconds in a background worker. Missing credentials or network errors retain local metadata and ordinary pane navigation. Pane metadata expires after ten seconds, so exited or killed terminals stop advertising stale agent activity. Prompts are bounded to 512 characters, terminal control characters are removed, and navigation passes validated pane/session targets as process arguments. Metadata is visible to other processes with access to your tmux server; it is never persisted by this script. This overview covers the current tmux server, not other sockets or machines.

Checks:

```sh
python3 -m unittest discover -s scripts -p test_tmux_agent_overview.py
cargo test -p nanocodex-managed presentation_contract_tests
cargo check -p nanocodex2-bin --bin nanocodex2
```
