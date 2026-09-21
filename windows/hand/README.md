# Windows Hand

`nanocodex-hand-setup-x86_64.exe` connects a Windows 10 or 11 x86-64 PC to your
Nanocodex account.

1. Download and double-click the installer; approve the administrator prompt.
2. Leave **Sign in and connect this computer now** checked.
3. Enter your account phone number and six-digit SMS code, then close the window.

The Windows **Nanocodex Hand** service starts automatically at boot and supervises
a hidden Hand process in your signed-in Windows session. It restarts a crashed
worker, and Windows restarts the supervisor after a failure. No terminal needs
to stay open. Signing out stops the worker; signing back in reconnects it.
Desktop capture and input require an unlocked, signed-in desktop. The service
does not bypass the Windows login or lock screen.

Install while signed in to the administrator account that will use the Hand.
Entering a different administrator's credentials at the elevation prompt is not
supported. Repair preserves the configured user's identity. The protected
Program Files supervisor holds no Nanocodex account credential: the worker uses
its own `%LOCALAPPDATA%\Nanocodex\Hand\account.json` and runs in that user's
session. The Hand exposes that user's apps, files, and profile workspace to the
connected Nanocodex account, subject to the agent's normal confirmation policy.

The Start menu contains **Start or repair**, **Stop**, **logs**, and **Uninstall**
shortcuts. Start or repair also completes sign-in if it was skipped during
installation. Stop pauses the service until restarted or the next boot.
Uninstall removes the service and this user's dedicated Hand credential,
identity, and logs. Worker logs live in `%LOCALAPPDATA%\Nanocodex\Hand\hand.log`;
supervisor logs live beside the installed executable as `service.log`.

Installation provisions the official OpenAI Sky runtime for the signed-in user
with `nanocodex2 computer setup --refresh`. Setup must succeed before the Hand
starts; no custom computer-control runtime is bundled.

The installer bundles FFmpeg for H.264 screen streaming. The stream requests
60 fps; actual distinct frame rate depends on the Windows display, capture
source, available CPU, and network. A software-rendered VM does not guarantee
60 distinct frames per second. Native capture forwards each encoded packet as
soon as FFmpeg reports its size, avoiding a wait for the next frame delimiter.
`NANOCODEX_SCREEN_FRAME_BOUNDARIES=annexb` restores the previous delimiter-based
path for troubleshooting.

The WebRTC connection also carries stereo system-output audio through WASAPI
loopback when Windows has an active playback device. It never captures the
microphone. Use the viewer's sound control to enable playback. VMs need a virtual
audio output device; a missing or failed audio source leaves video available.

WebRTC needs inbound UDP reachability or a TURN relay. For a VM behind NAT, see
the [shared video network settings](../../crates/nanocodex-hand/README.md).
Configure Windows Firewall for the installed `nanocodex2.exe`, the selected UDP
range, and the intended viewer networks. Windows can create an explicit Block
rule when its initial firewall prompt is cancelled; that rule takes precedence
over later Allow rules. Review any such rule before enabling a scoped allowance.
The installer preserves existing firewall policy.

## Build

Build `nanocodex2.exe` from the main workspace, then run:

```powershell
.\windows\hand\build.ps1 `
  -Nanocodex2 .\target\release\nanocodex2.exe `
  -Version 0.6.1
```

Inno Setup 6 produces `dist\windows-hand\nanocodex-hand-setup-x86_64.exe`.
The build downloads a pinned, checksum-verified FFmpeg archive; `-FfmpegArchive`
accepts an existing copy for offline builds. The inbox .NET Framework compiles
the small service supervisor during installation. Release automation signs the
payload and installer when the repository's Authenticode secrets are configured.

For existing non-administrator deployments, calling `setup-hand.ps1` directly
without `-Service` retains the per-user scheduled-task installation path. The
consumer installer and its shortcuts always select the Windows service.

For a VM integration check, run `test-service.ps1` in elevated PowerShell after
sign-in. It kills the worker and supervisor separately, verifies both recover,
and checks stop/start removes stale workers. It preserves the account and leaves
the service running. Reboot persistence additionally requires a real VM reboot
and a post-login check of `setup-hand.ps1 -Service -Action Status`.
