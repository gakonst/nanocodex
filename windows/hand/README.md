# Windows installer

`nanocodex-hand-setup-x86_64.exe` installs the same native `nanocodex` control
CLI and `nanocodex2` Hand used by the macOS and Linux setup flow on x86-64
Windows 10 or 11.

1. Run `irm https://nanocodex.paradigm.xyz/install.ps1 | iex`, or download and
   double-click the installer. The current per-user install does not need UAC.
2. Leave **Sign in and connect this computer now** checked.
3. Enter the account phone number and six-digit SMS code.
4. Guided setup verifies OpenAI's official Computer Use runtime, installs the
   persistent Hand, and offers the official browser extension.

Rust owns login, CUA provisioning, Hand configuration, and repair just as it
does on the other platforms. The installer only places signed binaries and a
checksum-pinned FFmpeg build, adds the installation directory to `PATH`, and
launches `nanocodex setup --refresh`. No PowerShell runner, generated C# binary,
or second Windows-only account flow is installed.

The Hand is a hidden per-user Task Scheduler job. It directly starts
`nanocodex2.exe hand` in the signed-in interactive session, restarts failures,
and starts again at login. The task definition contains no account credential;
the worker reads the same `%USERPROFILE%\.codex\nanocodex-account.json` login as
the CLI. Desktop capture and input require an unlocked, signed-in desktop and
do not bypass the Windows lock screen.

A separate per-user Task Scheduler job invokes the native Rust updater hourly.
It checksum-verifies matching `nanocodex.exe` and `nanocodex2.exe` assets,
refreshes the official upstream CUA payload, and stages service handover without
silently restarting an active Hand. `nanocodex update --auto disable` records a
persistent opt-out; `--auto enable` restores it.

The cross-platform lifecycle is:

```powershell
nanocodex setup
nanocodex hand install
nanocodex hand status
nanocodex hand start
nanocodex hand stop
nanocodex hand restart
```

The Start menu exposes setup/repair, start, stop, logs, and uninstall. Hand
identity and logs live under `%LOCALAPPDATA%\Nanocodex\Hand`; uninstall removes
the scheduled task and binaries but deliberately leaves account login and Hand
state so a reinstall does not silently change identity. Worker logs live at
`%LOCALAPPDATA%\Nanocodex\Hand\hand.log`.

The bundled FFmpeg enables H.264 screen streaming. Actual distinct frame rate
depends on the display, capture source, CPU, and network. WASAPI loopback carries
system-output audio when Windows has an active playback device; it never captures
the microphone. A missing audio source leaves video available. WebRTC requires
inbound UDP reachability or a TURN relay, and the installer preserves existing
Windows Firewall policy.

## Build

Build both Rust binaries, then run:

```powershell
.\windows\hand\build.ps1 `
  -Nanocodex .\target\release\nanocodex.exe `
  -Nanocodex2 .\target\release\nanocodex2.exe `
  -Version 0.6.5
```

Inno Setup 6 produces `dist\windows-hand\nanocodex-hand-setup-x86_64.exe`.
The build downloads a pinned, checksum-verified FFmpeg archive;
`-FfmpegArchive` accepts an existing copy for offline builds. Release automation
Authenticode-signs both Rust binaries and the installer when signing secrets are
configured.

The retired PowerShell/C# release installed a machine-wide service. Windows
must uninstall that old **Nanocodex Hand** entry once before this per-user
installer can take ownership; the installer detects it and explains the exact
migration rather than creating two competing Hands. The old uninstaller removes
its dedicated legacy credential/state. The new shared account login is retained
across future upgrades and uninstalls.
