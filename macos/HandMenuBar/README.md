# Hand menu bar

The macOS CLI installs a small menu-bar companion when preparing or installing
the local Hand. The Nanocodex desktop app is not required. The helper is compiled
with AppKit at CLI build time and shipped inside the CLI; users do not need Xcode
or Swift installed.

Click the Hand icon to see the companion, local service, account, and the Hand
inventory separately. A fresh installation shows the companion running and the
account signed out; the local service can wait for sign-in. A local process does
not establish account or server connectivity. Account verification, expired
sign-in, network errors, and denied access each have distinct states. A failed
or pending refresh never displays an old inventory as currently available.

Hands use friendly names and are grouped into computers and workspaces,
virtual machines, and screens when the server supplies those classifications.
Tool connections appear under Computers & workspaces. Connected describes the
server connection; it does not claim an execution health probe. Disconnected
devices remain visible with their connection state. Screen-only
entries show that a screen is advertised without claiming playback or input
works. Large inventories continue in the More connections submenu. Copy Status
includes every safe summary. The helper consumes the CLI's versioned
`hand menu-status` JSON and never reads account credentials itself.

Choose **Sign In…** to open the existing `nanocodex account login` flow in Terminal.
The helper creates a private executable command file containing the safely quoted
installed CLI path, opens it explicitly with Terminal, and prevents repeated
launches while that command remains open. Credentials are entered in the CLI's
existing interactive flow. The companion reconciles the login process by PID and creation time and removes
the private command directory when it exits, including after a crash.
The companion refreshes every five seconds during sign-in, when reopened, and
otherwise every thirty seconds. Opening the menu does not initiate sign-in.

Start, Stop, Restart, Refresh Status, and Open Hand Log control or inspect the
independent local Hand service. Mutating requests are serialized and never
short-cancelled or automatically retried after failure; Refresh Status reconciles
the observed state. Read-only status commands have a twenty-second deadline.

The icon starts near the right edge on first launch so it stays clear of the
notch on crowded menu bars. Later launches preserve the position you choose.
Its separate Aqua LaunchAgent starts the icon when the user logs in. **Quit Hand**
stops the local Hand service, including its VM factory, and closes the menu only
after Stop succeeds. If stopping fails, the menu stays open with an error so its
state can be refreshed. A slow status refresh does not delay Quit. Quit is
unavailable while another service action or Terminal sign-in is in progress. `nanocodex hand menu-bar` restores the icon; choose Start Hand to resume
a stopped service. Closing the desktop app and exiting the CLI do not stop the
menu helper.

To build only the helper for development:

```sh
mkdir -p output/hand-menubar
xcrun swiftc -O -framework AppKit macos/HandMenuBar/main.swift \
  -o output/hand-menubar/nanocodex-hand-menu-bar
```

The production installer supplies the absolute path to the installed CLI with
`--cli`. The helper requires schema version 1 of `hand menu-status`. An older CLI
or malformed response produces an unavailable status and disables service actions
until a valid observation succeeds.
