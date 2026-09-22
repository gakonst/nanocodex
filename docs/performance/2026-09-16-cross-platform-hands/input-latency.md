# Remote input-to-presentation latency

The native Omarchy publisher now forwards complete encoder packets without
waiting for the following H.264 access-unit delimiter. In the wired 4K test,
the final restart-safe implementation reduced median input-to-scheduled-
presentation latency by 9 ms and the 95th percentile by 19 ms.

| Capture framing | Actions | Median | 95th percentile |
| --- | ---: | ---: | ---: |
| Previous Annex-B lookahead | 20 | 92.3 ms | 107.2 ms |
| Initial F1 encoder packet lengths | 20 | 77.3 ms | 87.2 ms |
| Initial F1, second connection | 20 | 79.7 ms | 92.1 ms |
| Final C1 atomic records | 20 | 83.2 ms | 87.8 ms |

All three uncontended new-framing runs had zero decoded-video frame drops and zero video/audio
packet loss. The session retained 3840×2160 capture at 60 Hz, NVENC at a
60,000 kbps ceiling, desktop audio, and leased relative pointer input.
These are short samples on the existing wired network, not a WAN or game-scene
latency guarantee.

## Measurement

The full published account viewer connected to the real Omarchy publisher. A
temporary fullscreen GTK application alternated red and blue on Space. For each
sample, the browser sent a Space down/up pair over the actual leased reliable
control channel. A `requestVideoFrameCallback` sampled the center pixel of the
real video element and detected the changed color. The reported duration is
browser send time to the matching frame's `expectedDisplayTime`, both on the
same monotonic browser clock. It is scheduled presentation timing, not a camera
measurement of photons from a physical monitor. Samples were separated by
150 ms; the test released control and removed the temporary application.

The final C1 comparison also reduced median send-to-packet-receive time from
64.6 to 54.2 ms. This supports the encoder-forwarding path as the source of the
improvement. Original observations and summary statistics are in
[input-latency-measurements.json](input-latency-measurements.json).

One C1 run overlapped a second fullscreen/control probe and measured 99.8 ms
median. It is retained in the data as contended and excluded from the table;
the following uncontended run is the reported final C1 measurement.

A separate experiment set both audio and video receiver `jitterBufferTarget`
values to zero. Its matched color-toggle median was 98.0 ms versus the normal
92.3 ms baseline; it did not improve this setup. Production retains adaptive
browser buffering. No improvement is claimed for that experiment.

## Framing and validation

Pipe reads do not identify frame boundaries. The encoder uses FFmpeg's tee
muxer to flush packet metadata to a separate local pipe before writing the
corresponding Annex-B payload. The companion reads exactly the declared packet
length and forwards an `NCH264C1` stream of bounded records. Each record has
an unsigned big-endian 32-bit payload length; its top bit marks the last chunk
of the frame. Header and payload are written together: at most 4096 bytes on
Linux and 512 bytes on other Unix systems, within the platform's atomic pipe
write guarantee. A replacement encoder's exact header resets any unfinished
frame at a record boundary. No synchronization marker is sought inside video
payloads. Each assembled frame is bounded to 8 MiB; truncated or malformed
records fail closed. The receiver also accepts earlier `NCH264F1` packets and
the previous Annex-B stream.
Set `NANOCODEX_SCREEN_FRAME_BOUNDARIES=annexb` to select the previous encoder
output for troubleshooting. No settings are specific to one GPU or machine.

The Go race suite includes a real FFmpeg test which supplies just one raw frame
and keeps input open: that frame must arrive without another capture or EOF.
Additional tests exercise byte-at-a-time reads, oversized/truncated payloads,
malformed metadata, and legacy Annex-B handling. The staged binary also passed
an actual host NVENC 4K encode: its declared 125,005-byte frame matched the
complete H.264 payload exactly.

The shared Rust native/VM publisher also forwards explicitly framed encoder
packets. See the independently verified
[Windows encoded-frame latency results](windows-encoded-frame-latency.md).
Its fresh pipe/parser per child uses F1; the Waymote companion uses C1 because
replacement children inherit the same pipe.


Waymote replaces its encoder on resolution/configuration changes using SIGKILL
while retaining the output pipe. The initial F1 implementation rejected a
second stream header; moreover, a killed writer could leave a partial large
packet. C1 addresses both cases. A regression test actually kills a subprocess
blocked while writing a 1 MiB frame, starts a replacement stream on the same
pipe, and verifies that only the complete replacement frame is emitted. This
passes on macOS and Linux; parameter-set changes and byte-at-a-time reads have
separate coverage. A capture-forwarding error now appears in diagnostics rather
than being discarded before the generic capture-stopped message.


The deployed C1 publisher also passed a live forced-restart test. Three seconds
into a 15-second published-browser measurement, the encoder helper received
SIGKILL. Video resumed after a 1.67-second intentional restart gap, with 576
subsequent frame callbacks and the actual video element still 3840×2160. The
publisher PID remained unchanged with zero systemd restarts and one viewer
WebSocket throughout. Across the measurement, 802 video frames decoded without
drops/loss and 750 audio packets arrived without loss. Audio continued while
the encoder restarted; the browser was deliberately closed after measurement.
