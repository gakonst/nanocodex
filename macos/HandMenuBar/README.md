# Hand menu bar

The macOS CLI installs a small menu-bar companion when preparing or installing
the local Hand. The Nanocodex desktop app is not required. The helper is compiled
with AppKit at CLI build time and shipped inside the CLI; users do not need Xcode
or Swift installed.

The menu shows the existing per-user Hand service, with Start, Stop, Restart,
Refresh Status, and Open Hand Log controls. “Hand running” means the local service
has a process; it does not assert that its account connection is healthy. Pending
sign-in and stopped services have separate states. The helper never signs in,
creates a Hand identity, publishes a screen, or reads account credentials.

Its separate Aqua LaunchAgent starts the icon when the user logs in. Quitting the
menu bar leaves the Hand service running. `nanocodex hand menu-bar` restores the
icon. Closing the desktop app and exiting the CLI do not stop the menu helper.

To build only the helper for development:

```sh
mkdir -p output/hand-menubar
xcrun swiftc -O -framework AppKit macos/HandMenuBar/main.swift \
  -o output/hand-menubar/nanocodex-hand-menu-bar
```

The production installer supplies the absolute path to the installed CLI with
`--cli`. Status and controls use that CLI's `hand` subcommands. Mutating requests
are never automatically retried after failure; Refresh Status reconciles the
service's observed state.
