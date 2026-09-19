#if os(macOS)
import SwiftUI
import Combine
import CoreImage
import WebRTC

/// One small image per screen, kept only in memory. Only one watch-only peer is
/// opened at a time; it closes after its first frame or a bounded timeout.
@MainActor
final class RemoteScreenPreviews: ObservableObject {
    struct Preview { let image: CGImage; let updated: Date }
    @Published private(set) var images: [String: Preview] = [:]
    @Published private(set) var unavailable = Set<String>()
    @Published private(set) var loading: String?
    private var current: RemoteViewer?
    private var run = UUID()

    func stop() { run = UUID(); current?.close(); current = nil; loading = nil }

    func refresh(service: RemoteService, hands: [RemoteHand]) async {
        stop()
        let token = run
        let identities = Set(hands.map(\.identity))
        images = images.filter { identities.contains($0.key) }
        defer { if run == token { stop() } }
        while !Task.isCancelled, run == token {
            for hand in hands {
                guard !Task.isCancelled, run == token else { return }
                loading = hand.identity
                let viewer = RemoteViewer(recoveryWindow: .zero)
                current = viewer
                let renderer = RemoteThumbnailRenderer()
                var track: RTCVideoTrack?
                let video = viewer.$track.sink { next in
                    track?.remove(renderer); track = next; next?.add(renderer)
                }
                let frames = viewer.$frame.sink { frame in
                    if let frame { renderer.accept(frame) }
                }
                await viewer.connect(service: service, hand: hand)
                // Never acquire control or update immersive focus for previews.
                for _ in 0..<60 {
                    if Task.isCancelled || run != token || renderer.image != nil { break }
                    do { try await Task.sleep(for: .milliseconds(100)) } catch { break }
                }
                track?.remove(renderer); video.cancel(); frames.cancel(); viewer.close()
                guard !Task.isCancelled, run == token else { return }
                if let image = renderer.image {
                    images[hand.identity] = Preview(image: image, updated: Date()); unavailable.remove(hand.identity)
                } else { unavailable.insert(hand.identity) }
                current = nil; loading = nil
            }
            do { try await Task.sleep(for: .seconds(10)) } catch { return }
        }
    }
}

/// Downsamples before retaining pixels. Frame callbacks may run off the main
/// thread; only the first frame is converted, with a maximum 320-pixel edge.
final class RemoteThumbnailRenderer: NSObject, RTCVideoRenderer, @unchecked Sendable {
    private let lock = NSLock()
    private var captured: CGImage?
    private var converting = false
    var image: CGImage? { lock.withLock { captured } }
    func setSize(_ size: CGSize) {}
    func accept(_ image: CGImage) {
        guard lock.withLock({ if captured != nil || converting { return false }; converting = true; return true }) else { return }
        let size = Self.dimensions(width: image.width, height: image.height)
        let context = CGContext(data: nil, width: size.width, height: size.height, bitsPerComponent: 8,
                                bytesPerRow: size.width * 4, space: CGColorSpaceCreateDeviceRGB(),
                                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        context?.interpolationQuality = .low
        context?.draw(image, in: CGRect(x: 0, y: 0, width: size.width, height: size.height))
        lock.withLock { captured = context?.makeImage(); converting = false }
    }
    static func dimensions(width: Int, height: Int) -> (width: Int, height: Int) {
        let scale = min(1, 320 / Double(max(1, max(width, height))))
        return (max(1, Int(Double(width) * scale)), max(1, Int(Double(height) * scale)))
    }
    func renderFrame(_ frame: RTCVideoFrame?) {
        guard let frame, lock.withLock({ if captured != nil || converting { return false }; converting = true; return true }) else { return }
        let buffer = frame.buffer.toI420()
        let size = Self.dimensions(width: Int(buffer.width), height: Int(buffer.height))
        var pixels = [UInt8](repeating: 255, count: size.width * size.height * 4)
        func byte(_ value: Double) -> UInt8 { UInt8(min(255, max(0, value.rounded()))) }
        for y in 0..<size.height {
            let sourceY = y * Int(buffer.height) / size.height
            for x in 0..<size.width {
                let sourceX = x * Int(buffer.width) / size.width
                let luma = 1.164 * (Double(buffer.dataY[sourceY * Int(buffer.strideY) + sourceX]) - 16)
                let u = Double(buffer.dataU[(sourceY / 2) * Int(buffer.strideU) + sourceX / 2]) - 128
                let v = Double(buffer.dataV[(sourceY / 2) * Int(buffer.strideV) + sourceX / 2]) - 128
                let offset = (y * size.width + x) * 4
                pixels[offset] = byte(luma + 1.596 * v)
                pixels[offset + 1] = byte(luma - 0.392 * u - 0.813 * v)
                pixels[offset + 2] = byte(luma + 2.017 * u)
            }
        }
        var result: CGImage?
        if let provider = CGDataProvider(data: Data(pixels) as CFData) {
            result = CGImage(width: size.width, height: size.height, bitsPerComponent: 8, bitsPerPixel: 32,
                             bytesPerRow: size.width * 4, space: CGColorSpaceCreateDeviceRGB(),
                             bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
                             provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent)
        }
        if let image = result, frame.rotation != ._0 {
            let orientation: CGImagePropertyOrientation = frame.rotation == ._90 ? .right : frame.rotation == ._180 ? .down : .left
            let rotated = CIImage(cgImage: image).oriented(orientation)
            result = CIContext().createCGImage(rotated, from: rotated.extent)
        }
        lock.withLock { captured = result; converting = false }
    }
}

struct RemoteScreenOverview: View {
    let service: RemoteService
    let hands: [RemoteHand]
    let active: Bool
    let select: (RemoteHand) -> Void
    @StateObject private var previews = RemoteScreenPreviews()
    var body: some View {
        ScrollView {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 240, maximum: 440), spacing: 16)], spacing: 16) {
                ForEach(hands, id: \.identity) { hand in
                    Button { select(hand) } label: {
                        VStack(alignment: .leading, spacing: 8) {
                            ZStack {
                                Color.black
                                if let preview = previews.images[hand.identity] {
                                    Image(decorative: preview.image, scale: 1).resizable().scaledToFit()
                                } else {
                                    VStack(spacing: 8) {
                                        Image(systemName: hand.kind == .phone ? "iphone" : "display").font(.largeTitle)
                                        Text(previews.loading == hand.identity ? "Updating preview…" : previews.unavailable.contains(hand.identity) ? "Preview unavailable" : "Waiting for preview").font(.caption)
                                    }.foregroundStyle(.secondary)
                                }
                            }.aspectRatio(16 / 10, contentMode: .fit).clipShape(RoundedRectangle(cornerRadius: 10))
                            Text(hand.machineName).font(.headline).lineLimit(1)
                            HStack {
                                Text(hand.name).lineLimit(1)
                                Spacer()
                                if let preview = previews.images[hand.identity] {
                                    Text(preview.updated, style: .relative).monospacedDigit()
                                }
                            }.font(.caption).foregroundStyle(.secondary)
                        }.padding(10).background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 14))
                    }.buttonStyle(.plain).help("Open " + hand.machineName + " · " + hand.name)
                        .accessibilityIdentifier("remote-screen:\(hand.machineID):\(hand.id)")
                }
            }.padding(12)
        }
        .task(id: (active ? "active:" : "paused:") + hands.map(\.identity).joined(separator: "|")) {
            if active { await previews.refresh(service: service, hands: hands) }
            else { previews.stop() }
        }
        .onDisappear { previews.stop() }
        .accessibilityIdentifier("remote-screen-overview")
    }
}
#endif
