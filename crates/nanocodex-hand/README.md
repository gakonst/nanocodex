# Nanocodex Hand

Native screen capture and input for Nanocodex Hands. This package is part of the supported Rust workspace. Platform permission prompts and capture/input APIs remain owned by the native host.

Windows uses native, bounded GDI JPEG capture and Win32 input in the signed-in user's desktop. The background Windows service launches and supervises the Hand worker in that user's interactive session because Session 0 cannot access the desktop. Locked and elevated secure desktops are not controlled by an ordinary user process.

The shared video publisher can use FFmpeg's `gdigrab` input and software H.264 at 60 Hz. Put `ffmpeg.exe` beside `nanocodex2.exe` or on PATH. If that encoder is unavailable, native JPEG capture still works without an additional executable. Run `cargo run -p nanocodex-hand --example capture_latency` from the signed-in desktop to verify native capture.

For a deployment behind nested NAT without a TURN relay, set
`NANOCODEX_SCREEN_TRANSPORT=frames-v1` on that Hand to use authenticated WebSocket
JPEG frames and input instead of WebRTC. The default remains 60 fps video when
an encoder is available. This setting applies to the shared Rust screen publisher.

For port-preserving NAT, `NANOCODEX_VIDEO_ADVERTISE_IP` advertises a reachable
host IP while keeping sockets bound to the guest's private interface. Pair it
with `NANOCODEX_VIDEO_UDP_PORTS=MIN-MAX` and explicit UDP forwarding through each
NAT layer. `NANOCODEX_VIDEO_INTERFACE` selects the capture host's network interface;
`NANOCODEX_VIDEO_IPV4_ONLY=1` restricts ICE to IPv4 UDP. These settings do not open
firewalls, forward ports, or provide a TURN relay. Keep firewall allowances scoped
to the intended viewers, executable, and UDP range. A LAN address only supports
viewers that can reach that LAN; it does not provide cellular Internet access.

Windows H.264 quality can be configured with `NANOCODEX_SCREEN_MAX_DIMENSION`
(1280–7680, default 1280) and `NANOCODEX_SCREEN_BITRATE_KBPS` (1000–100000,
default 6000). The encoder preserves aspect ratio, never upscales, and selects
an H.264 level covering the output's 60 Hz macroblock rate and bitrate. Higher
settings need enough capture/encoding CPU and network bandwidth; verify decoded
frame rate in the actual viewer after changing them.
