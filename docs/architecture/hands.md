# Hands

`nanocodex2 hand` is the headless machine runner. Install it once under the OS service manager; the CLI and app observe it. The native `nanocodex hand` controller uses a per-user LaunchAgent on macOS, systemd on Linux, and a per-user Task Scheduler job on Windows. If installation, start permission, or the account-scoped connection is unavailable, the CLI prints an actionable warning and continues remote work. `NANOCODEX_DISABLE_HAND=1` skips the local Hand check.

```text
OS -> Hand daemon <--- outbound WebSocket ---> AccountHostedTools <- agent
       |-- host tools
       `-- VM factory helper -> VMs published as separate mountable Hands
```

The broker durably claims each call before sending it once. The daemon owns execution; a socket only carries requests and replies. A disconnected socket loses its replies, not its admitted work. Reconnecting never replays commands or transfers old replies.

- Offline before dispatch: not started.
- Result recorded: return that result.
- Connection lost after dispatch: outcome unknown, reported as a local tool failure.

The publisher lock protects the host identity. Closing the last client leaves the daemon running. The OS service manager owns startup, restart, and shutdown. Active calls are bounded across reconnects. Lease expiry reconnects, and the daemon leaves a live VM factory running through connection outages. Rejected credentials stop the daemon; log in again and restart the service.

On macOS, the standalone daemon prevents idle system sleep by default using `/usr/bin/caffeinate -i -w <daemon PID>`. The assertion starts after exclusive publisher ownership and state opening, survives reconnects and client disconnects, and ends when the daemon shuts down. It does not keep the display awake or bypass lid-close sleep. If the helper cannot start, the daemon logs a warning and continues without sleep inhibition. Linux and Windows do not acquire this assertion.

Set `NANOCODEX_HAND_KEEP_AWAKE=0` in the standalone service environment to opt out (for launchd, use its plist `EnvironmentVariables` dictionary and reload the service when convenient). An environment variable in an observing terminal does not change an already running service. The macOS app’s `keepMacAwake` preference controls its own ProcessInfo assertion separately; it does not configure the standalone daemon.

## Install

The supported local path is `nanocodex setup`, which performs the shared SMS
login, platform CUA provisioning, and idempotent `nanocodex hand install`. Use
`nanocodex hand status`, `start`, `stop`, or `restart` on all three desktop
platforms. A remote Linux host can be enrolled without interactive auth using
`nanocodex hand install --target user@host [--port PORT]`.

The installed Rust updater runs hourly as a per-user LaunchAgent, systemd timer,
or Task Scheduler job. It stages verified matching CLI/Hand bundles and never
silently restarts a running Hand; the next explicit start or restart activates a
staged Hand update. Windows background runs also refresh the verified upstream
CUA payload.

The older machine-wide helper remains available for explicitly managed macOS or
Linux deployments. First run `nanocodex2 login` as the machine owner, then:

```sh
sudo python3 scripts/install-hand-service.py --user "$USER" --binary /path/to/nanocodex2
```

The installer creates one machine-wide launchd service on macOS or systemd service on Linux, running as that non-root user. It uses the user's saved account login and existing `vm.json` configuration. It neither copies credentials into the service definition nor requires a terminal or app to stay open. Host tools work without a GUI; desktop capture needs the platform's GUI session and permissions.

For a custom login, pass `--managed-url https://your-server` and `--account-file /absolute/path/to/nanocodex-account.json` to the installer. These select the existing login without copying its secret.

Use `sudo systemctl stop/start nanocodex-hand` on Linux. On macOS, use `sudo launchctl bootout system/com.nanocodex.hand` to stop and `sudo launchctl bootstrap system /Library/LaunchDaemons/com.nanocodex.hand.plist` to start. Remove/disable the OS service to prevent future boot startup. Windows service installation is not provided by this helper.

The Linux SSH bootstrap installs the same single daemon with its VM recipe. Older separate factory services must be removed explicitly before installing it. There is one current wire contract: publishers must send `capabilities: ["turn_metadata"]`. This fixed field preserves the existing publisher format without capability negotiation or legacy metadata fallback. There is no new process recovery API or persistence across daemon crashes.

## Execution mounts

`environment` lists authorized Hand roots; `workdir` selects where each command
runs. A mount already represents its advertised workspace: `/laptop/src` means
`src` beneath that workspace. `mount` provisions a named sandbox when native
builds, tests, or process sessions require one. `/brain` provides durable shared
scratch and the embedded shell without a native Hand.

Cloudflare sandbox processes can write their own workspace and `/brain`, and
read peer sandbox workspaces through native mounts. Connected user Hands provide
execution placement; peer filesystem access requires a conforming native adapter.
Each call captures its Hand connection. Retained process sessions remain pinned
to that Hand; reconnecting never retargets admitted work. Subagents share this
mount policy while keeping model state private. Coordinate concurrent file writes.
