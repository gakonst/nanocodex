#if os(macOS)
import AppKit
import CoreVideo
import MetalKit
import WebRTC
import XCTest
@testable import NanocodexRemote

/// Exercises the shipped WebRTC Metal renderer inside the production viewport.
/// No network, screen capture permission, input injection, or app activation.
final class RemoteVideoEdgeTests: XCTestCase {
    private final class BackgroundWindow: NSWindow {
        override var canBecomeKey: Bool { false }
        override var canBecomeMain: Bool { false }
    }

    @MainActor func testMetalPreservesAllFourEdgesAcrossFrameAndViewportResizes() async throws {
        guard RTCMTLNSVideoView.isMetalAvailable() else {
            throw XCTSkip("Requires a macOS Metal device and WindowServer")
        }
        _ = NSApplication.shared
        let viewer = RemoteViewer()
        let viewport = MacRemoteViewport(viewer: viewer)
        let window = BackgroundWindow(contentRect: CGRect(x: 40, y: 40, width: 960, height: 600),
                                      styleMask: .borderless, backing: .buffered, defer: false)
        window.ignoresMouseEvents = true
        window.isReleasedWhenClosed = false
        window.contentView = viewport
        window.orderBack(nil)
        defer { viewport.canvas.detach(); window.orderOut(nil); window.close() }
        let video = try XCTUnwrap(viewport.canvas.subviews.compactMap { $0 as? RTCMTLNSVideoView }.first)
        let metal = try XCTUnwrap(video.subviews.compactMap { $0 as? MTKView }.first)
        // Pausing permits a deterministic single draw. Readback requires this
        // public Metal option; all shader, conversion, and sizing code is real.
        metal.isPaused = true
        metal.framebufferOnly = false
        video.isHidden = false
        let device = try XCTUnwrap(metal.device)
        let queue = try XCTUnwrap(device.makeCommandQueue())

        for source in [CGSize(width: 900, height: 1600), CGSize(width: 1920, height: 1080),
                       CGSize(width: 1600, height: 900)] {
            let frame = try edgeFrame(width: Int(source.width), height: Int(source.height))
            video.renderFrame(frame)
            video.setSize(source)
            // WebRTC and MacRemoteCanvas each dispatch the size callback.
            for _ in 0..<3 { await Task.yield(); try await Task.sleep(for: .milliseconds(10)) }
            for host in [CGSize(width: 960, height: 540), CGSize(width: 1000, height: 500),
                         CGSize(width: 400, height: 800)] {
                window.setContentSize(host)
                viewport.needsLayout = true
                viewport.layoutSubtreeIfNeeded()
                viewport.canvas.needsLayout = true
                viewport.canvas.layoutSubtreeIfNeeded()
                video.layoutSubtreeIfNeeded()
                let context = "source=\(source), host=\(host)"
                // These checks use AppKit's actual clipping, not a duplicate of
                // fitted(). A full GPU texture alone cannot detect canvas crop.
                XCTAssertLessThanOrEqual(video.visibleRect.minX, video.bounds.minX + 0.5, context)
                XCTAssertLessThanOrEqual(video.visibleRect.minY, video.bounds.minY + 0.5, context)
                XCTAssertGreaterThanOrEqual(video.visibleRect.maxX, video.bounds.maxX - 0.5, context)
                XCTAssertGreaterThanOrEqual(video.visibleRect.maxY, video.bounds.maxY - 0.5, context)
                XCTAssertEqual(video.bounds.width / video.bounds.height, source.width / source.height,
                               accuracy: 0.002, context)
                XCTAssertTrue(metal.visibleRect.contains(metal.bounds), context)
                XCTAssertEqual(metal.frame.width, video.bounds.width, accuracy: 1, context)
                XCTAssertEqual(metal.frame.height, video.bounds.height, accuracy: 1, context)

                let drawable = try XCTUnwrap(metal.currentDrawable, context)
                // WebRTC 152's Metal renderer allows one GPU frame in flight:
                // starting its next draw waits for the prior command buffer's
                // completion. Retain the first drawable, then draw into another
                // to fence its pixels without requiring on-screen presentation
                // (presented callbacks do not fire for an occluded test window).
                metal.draw()
                metal.releaseDrawables()
                metal.draw()
                let texture = drawable.texture
                XCTAssertEqual(texture.pixelFormat, .bgra8Unorm)
                let stride = texture.width * 4
                let buffer = try XCTUnwrap(device.makeBuffer(length: stride * texture.height, options: .storageModeShared))
                let command = try XCTUnwrap(queue.makeCommandBuffer())
                let blit = try XCTUnwrap(command.makeBlitCommandEncoder())
                blit.copy(from: texture, sourceSlice: 0, sourceLevel: 0,
                          sourceOrigin: MTLOrigin(x: 0, y: 0, z: 0),
                          sourceSize: MTLSize(width: texture.width, height: texture.height, depth: 1),
                          to: buffer, destinationOffset: 0, destinationBytesPerRow: stride,
                          destinationBytesPerImage: stride * texture.height)
                blit.endEncoding()
                let copied = expectation(description: "GPU readback \(context)")
                command.addCompletedHandler { _ in copied.fulfill() }
                command.commit()
                await fulfillment(of: [copied], timeout: 5)
                XCTAssertEqual(command.status, .completed, context)
                let bytes = buffer.contents().assumingMemoryBound(to: UInt8.self)
                func assertPixel(_ x: Int, _ y: Int, _ rgb: (Int, Int, Int), _ edge: String) {
                    let offset = y * stride + x * 4
                    for (channel, expected) in [(2, rgb.0), (1, rgb.1), (0, rgb.2)] {
                        XCTAssertEqual(Int(bytes[offset + channel]), expected, accuracy: 35,
                                       "\(edge) edge channel \(channel), \(context)")
                    }
                }
                assertPixel(texture.width / 2, 0, (0, 255, 0), "top")
                assertPixel(texture.width / 2, texture.height - 1, (255, 255, 0), "bottom")
                assertPixel(0, texture.height / 2, (255, 0, 0), "left")
                assertPixel(texture.width - 1, texture.height / 2, (0, 0, 255), "right")
                metal.releaseDrawables()
            }
        }
        XCTAssertFalse(window.isKeyWindow)
        XCTAssertFalse(window.isMainWindow)
        XCTAssertNotEqual(NSWorkspace.shared.frontmostApplication?.processIdentifier, ProcessInfo.processInfo.processIdentifier,
                       "The rendering regression must not activate its test process")
    }

    private func edgeFrame(width: Int, height: Int) throws -> RTCVideoFrame {
        var optional: CVPixelBuffer?
        let status = CVPixelBufferCreate(kCFAllocatorDefault, width, height, kCVPixelFormatType_32BGRA,
                                        [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &optional)
        XCTAssertEqual(status, kCVReturnSuccess)
        let buffer = try XCTUnwrap(optional)
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        let bytes = try XCTUnwrap(CVPixelBufferGetBaseAddress(buffer)).assumingMemoryBound(to: UInt8.self)
        let stride = CVPixelBufferGetBytesPerRow(buffer)
        for y in 0..<height {
            for x in 0..<width {
                let rgb: (UInt8, UInt8, UInt8)
                if y < 16 { rgb = (0, 255, 0) }
                else if y >= height - 16 { rgb = (255, 255, 0) }
                else if x < 16 { rgb = (255, 0, 0) }
                else if x >= width - 16 { rgb = (0, 0, 255) }
                else { rgb = (32, 32, 32) }
                let offset = y * stride + x * 4
                bytes[offset] = rgb.2; bytes[offset + 1] = rgb.1
                bytes[offset + 2] = rgb.0; bytes[offset + 3] = 255
            }
        }
        return RTCVideoFrame(buffer: RTCCVPixelBuffer(pixelBuffer: buffer), rotation: ._0, timeStampNs: Int64(ProcessInfo.processInfo.systemUptime * 1_000_000_000))
    }
}
#endif
