# Installed Apple apps — September 16, 2026

[Artifact identities and live verification](installed-app-measurements.json).

## Mac

The Release app is installed and running at `/Applications/Nanocodex.app`.
The app and bundled helper pass strict signature verification; the helper has
`com.apple.security.hypervisor`. Its Mach-O UUID matches the freshly built
integrated CLI. The helper follows standalone CLI signing policy so firmware
loading through `DYLD_LIBRARY_PATH` works consistently.

A previous helper preparation measured 33 ms copying, 393 ms signing and 123 ms
verifying. Bundling the entitlement removes the copy/sign work when that fallback
would otherwise run; verification remains. This is not a measured reduction in
complete VM mount time, and an already-signed configured GPU helper does not pay
that fallback.

This Mac's explicit GPU configuration uses a separate, relocatable GPU helper.
It was rebuilt from `e33c804a` in Release mode with `nanocodex-vm/gpu`, verified,
and installed into a new versioned directory. Only the `binary` field in
`vm.json` changed. Image, guest runtime, firmware, GPU libraries and settings
were preserved; the previous bundle and configuration remain available.

The installed app's real bundled runtime and helper passed a live journey:

- Native Hand and VM factory both connected.
- A separate CLI lease closed while the app's Hand remained connected.
- A plain-language agent request discovered this Mac, ran `uname -s` on its
  native host, created a VM on the same Mac, and ran a command in that VM.
- Results contained `Darwin`, `Linux`, and `hands_optimization_ok`.
- The disposable agent was deleted successfully; test leases closed.
- The normal installed GUI app was reopened afterward.

An initial verification attempt incorrectly forced the standard non-GPU helper
into the GPU recipe. It correctly rejected that configuration. The final journey
used the normal configuration and the newly installed GPU helper. No fallback to
an older VM binary was used for the successful final journey.

## iPhone

The signed Release app `xyz.paradigm.centaur`, version `0.1.0`, build
`2026091602`, was installed and launched successfully on the physical iPhone 17
Pro. A subsequent device query confirmed that installed build. It contains the
shared admission snapshot client, bounded pre-admission retry, and frames-v1
first-decoded-frame diagnostics.

Physical-device first-frame latency remains unmeasured. Xcode UI automation
failed before the test body with “Timed out while enabling automation mode.”
The decoded-frame cohorts in the screen report ran on the Mac over real WAN
connections to Linux; they are not phone benchmarks.
