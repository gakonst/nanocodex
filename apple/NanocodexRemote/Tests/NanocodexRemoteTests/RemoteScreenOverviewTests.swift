#if os(macOS)
import XCTest
import CoreVideo
import WebRTC
@testable import NanocodexRemote

final class RemoteScreenOverviewTests: XCTestCase {
    func testThumbnailBoundsPreserveWideAndPortraitGeometry() {
        for (w, h) in [(3840, 2160), (1080, 1920), (640, 480), (80, 60)] {
            let size = RemoteThumbnailRenderer.dimensions(width: w, height: h)
            XCTAssertLessThanOrEqual(max(size.width, size.height), 320)
            XCTAssertEqual(Double(size.width) / Double(size.height), Double(w) / Double(h), accuracy: 0.02)
        }
    }
    func testDecodedFrameProducesSmallPreviewAndHonorsRotation() throws {
        var pixelBuffer: CVPixelBuffer?
        XCTAssertEqual(CVPixelBufferCreate(kCFAllocatorDefault, 640, 360, kCVPixelFormatType_32BGRA, nil, &pixelBuffer), kCVReturnSuccess)
        let buffer = try XCTUnwrap(pixelBuffer)
        CVPixelBufferLockBaseAddress(buffer, [])
        let data = try XCTUnwrap(CVPixelBufferGetBaseAddress(buffer)).assumingMemoryBound(to: UInt8.self)
        let stride = CVPixelBufferGetBytesPerRow(buffer)
        for y in 0..<360 { for x in 0..<640 {
            let offset = y * stride + x * 4
            data[offset] = 0; data[offset + 1] = 0; data[offset + 2] = 255; data[offset + 3] = 255
        }}
        CVPixelBufferUnlockBaseAddress(buffer, [])
        for rotation in [RTCVideoRotation._0, ._90] {
            let renderer = RemoteThumbnailRenderer()
            renderer.renderFrame(RTCVideoFrame(buffer: RTCCVPixelBuffer(pixelBuffer: buffer), rotation: rotation, timeStampNs: 1))
            let image = try XCTUnwrap(renderer.image)
            XCTAssertEqual(image.width, rotation == ._0 ? 320 : 180)
            XCTAssertEqual(image.height, rotation == ._0 ? 180 : 320)
            // A later video frame cannot continuously rebuild an overview tile.
            renderer.renderFrame(RTCVideoFrame(buffer: RTCCVPixelBuffer(pixelBuffer: buffer), rotation: ._180, timeStampNs: 2))
            XCTAssertTrue(renderer.image === image)
        }
    }
    @MainActor func testReleaseControlDoesNotCloseTheScreen() {
        let viewer = RemoteViewer()
        let canvas = MacRemoteCanvas(viewer: viewer)
        var closed = false
        canvas.onExit = { closed = true }
        canvas.releaseRemoteControl(nil)
        XCTAssertFalse(closed)
        XCTAssertFalse(viewer.controlling)
    }
}
#endif
