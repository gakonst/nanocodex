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
The warning icon retains the last observed account, inventory, or local-service
warning while a refresh is checking. It clears only when a completed observation
resolves the warning; its accessibility label also identifies the warning state.

The menu shows one plain Hands list, with connected entries first and names
sorted within each connection state. Up to twenty entries appear directly in
the root menu; larger inventories use sibling pages named Hands 1–20, Hands
21–40, and so on, with at most twenty entries per page. Every returned record
remains accessible, including distinct Hands with identical names. There are no
computer, workspace, VM, screen, or offline category submenus. Copy Status
includes the full list.

Connected describes the server connection, not an execution health probe.
Screen-only entries say that a screen is advertised without claiming playback or
input works. The helper consumes the CLI's versioned `hand menu-status` JSON and
never reads account credentials itself.

Choose **Sign In…** to open the existing `nanocodex account login` flow in Terminal.
The helper creates a private executable command file containing the safely quoted
installed CLI path, opens it explicitly with Terminal, and prevents repeated
launches while that command remains open. Credentials are entered in the CLI's
existing interactive flow. The companion reconciles the login process by PID and creation time and removes
the private command directory when it exits, including after a crash.
The companion refreshes every five seconds during sign-in, when reopened, and
otherwise every thirty seconds. Opening the menu does not initiate sign-in.

**Keep Mac Awake** is on by default and controls the standalone Hand's persisted
idle-sleep setting. It changes without restarting the Hand or interrupting work.
The display can turn off and macOS can lock normally; closing the lid or choosing
Sleep can still suspend the Mac. Keeping awake uses more battery. The same setting
is available through `nanocodex hand keep-awake on` or `off`; omit the value to
inspect it. The menu refreshes the observed setting after a change.

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

Run the native accessibility journey against an existing CLI and the compiled
helper (both paths must be absolute):

```sh
python3 scripts/tests/hand-menu-bar-account-macos.py \
  --cli /absolute/path/to/nanocodex \
  --helper /absolute/path/to/nanocodex-hand-menu-bar \
  --evidence /absolute/path/to/native-menu-evidence
```

This exercises the real AppKit menu with a synthetic account, including flat
lists, identical names, the twenty-entry paging boundary, all pages of a larger
inventory, and refresh retaining accessibility item identities while open.
A delayed refresh preserves the expired-login warning; recovery clears it only
after a healthy observation arrives.
It does not install or stop the live service.

Choose **Allow Screen & Input Permissions…**, or run
`nanocodex hand permissions --guide`, to open the floating permission guide.
The icon drags the exact executable reported by the running Hand, not the menu
companion. The guide follows the visible System Settings window and hides while
that window is minimized. Accessibility is called Device Control and Data Access
on newer macOS versions. Permission checks run in the daemon without prompting;
`nanocodex hand permissions --check --json` exposes the same versioned result.

The guide advances to the next missing permission, restarts the Hand once both
are allowed, and verifies permission status before closing. If Screen Recording
remains unchecked after a drop, use **Restart Hand** to refresh the daemon's
cached permission state. Failed checks and restarts remain visible. **Done**
closes the guide without granting access. Guide-only launches exit when closed.

To check the permission CLI against a running updated daemon without changing
its permissions:

```sh
python3 scripts/tests/hand-permissions-cli.py \
  --cli /absolute/path/to/nanocodex \
  --evidence /absolute/path/to/output/permission-check
```

This exercises the shipped CLI and actual daemon transport, checks rejected flag
combinations and daemon identity, and records commands, exit codes, and output.
It does not replace the interactive macOS grant, drag, and restart journey.
