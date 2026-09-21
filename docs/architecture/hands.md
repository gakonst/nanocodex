# Hands

`nanocodex2 hand` is the headless machine runner. Install it once as an OS boot service; the CLI and app observe it. On macOS and systemd Linux, the CLI checks the installed service and requests a noninteractive start when needed. It never invokes sudo, restarts a running publisher, or creates an app-owned daemon. If installation, start permission, or the account-scoped connection is unavailable, the CLI prints an actionable warning and continues remote work. `NANOCODEX_DISABLE_HAND=1` skips the local Hand check.

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

## Install

First run `nanocodex2 login` as the machine owner. Then install the built binary:

```sh
sudo python3 scripts/install-hand-service.py --user "$USER" --binary /path/to/nanocodex2
```

The installer creates one machine-wide launchd service on macOS or systemd service on Linux, running as that non-root user. It uses the user's saved account login and existing `vm.json` configuration. It neither copies credentials into the service definition nor requires a terminal or app to stay open. Host tools work without a GUI; desktop capture needs the platform's GUI session and permissions.

For a custom login, pass `--managed-url https://your-server` and `--account-file /absolute/path/to/nanocodex-account.json` to the installer. These select the existing login without copying its secret.

Use `sudo systemctl stop/start nanocodex-hand` on Linux. On macOS, use `sudo launchctl bootout system/com.nanocodex.hand` to stop and `sudo launchctl bootstrap system /Library/LaunchDaemons/com.nanocodex.hand.plist` to start. Remove/disable the OS service to prevent future boot startup. Windows service installation is not provided by this helper.

The Linux SSH bootstrap installs the same single daemon with its VM recipe. Older separate factory services must be removed explicitly before installing it. There is one current wire contract: publishers must send `capabilities: ["turn_metadata"]`. This fixed field preserves the existing publisher format without capability negotiation or legacy metadata fallback. There is no new process recovery API or persistence across daemon crashes.
