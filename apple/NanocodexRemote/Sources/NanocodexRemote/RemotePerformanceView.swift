import SwiftUI

/// A compact entry point; expensive details never cover the remote controls.
struct RemotePerformanceView: View {
    @ObservedObject var viewer: RemoteViewer
    @State private var expanded = false

    var body: some View {
        Button { expanded.toggle() } label: {
            HStack(spacing: 4) {
                Image(systemName: "waveform.path.ecg")
                if let fps = viewer.performance.decodedFramesPerSecond {
                    Text(String(format: "%.0f fps", fps)).monospacedDigit()
                }
            }.font(.caption)
        }
        .buttonStyle(.plain)
        .foregroundStyle(.secondary)
        .accessibilityLabel("Stream performance")
        .accessibilityValue(viewer.performance.decodedFramesPerSecond.map { String(format: "%.0f decoded frames per second", $0) } ?? "Waiting for measurements")
        .accessibilityIdentifier("remote-performance")
        .popover(isPresented: $expanded) {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text("Stream performance").font(.headline)
                    Spacer()
                    Button("Done") { expanded = false }.font(.caption)
                }
                Grid(alignment: .leading, horizontalSpacing: 24, verticalSpacing: 8) {
                    metric("Connection ready", viewer.performance.connectionMilliseconds, unit: "ms")
                    metric("First decoded frame", viewer.performance.firstDecodedFrameMilliseconds, unit: "ms")
                    Divider().gridCellColumns(2)
                    metric("Decoded frame rate", viewer.performance.decodedFramesPerSecond, unit: "fps")
                    metric("Video received", viewer.performance.receiveMegabitsPerSecond, unit: "Mbps", decimals: 2)
                    metric("Network RTT", viewer.performance.networkRoundTripMilliseconds, unit: "ms")
                    metric("Receive buffer", viewer.performance.jitterBufferMilliseconds, unit: "ms", decimals: 1)
                    metric("Decode time", viewer.performance.decodeMilliseconds, unit: "ms", decimals: 1)
                    metric("Frames dropped", viewer.performance.droppedFrames, unit: "")
                    metric("Packet loss", viewer.performance.packetLossPercent, unit: "%", decimals: 1)
                    if let width = viewer.performance.width, let height = viewer.performance.height {
                        row("Video size", "\(width) × \(height)")
                    }
                    if let route = viewer.performance.route { row("Connection", route) }
                    if let bytes = viewer.performance.controlBufferedBytes { row("Control send queue", "\(bytes) bytes") }
                    if let bytes = viewer.performance.motionBufferedBytes { row("Pointer send queue", "\(bytes) bytes") }
                }.font(.caption)
                Text(viewer.hand?.transport == .frames
                     ? "Live WebRTC measurements are unavailable for this frame stream. Connection timings refer to this attempt."
                     : "Rates, buffer, decode and loss use the latest sample interval. RTT measures the network round trip. Input-to-photon and display presentation time are not measured. Connection timings refer to this attempt.")
                    .font(.caption2).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            .padding(20).frame(width: 320)
            .presentationCompactAdaptation(.popover)
        }
    }

    private func metric(_ label: String, _ value: Double?, unit: String, decimals: Int = 0) -> some View {
        row(label, value.map { String(format: "%.*f", decimals, $0) + (unit.isEmpty ? "" : " " + unit) } ?? "—")
    }

    private func row(_ label: String, _ value: String) -> some View {
        GridRow {
            Text(label).foregroundStyle(.secondary)
            Text(value).monospacedDigit().gridColumnAlignment(.trailing)
        }
    }
}
