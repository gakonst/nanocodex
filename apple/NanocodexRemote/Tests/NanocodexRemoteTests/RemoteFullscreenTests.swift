#if os(macOS)
import AppKit
import ImageIO
import XCTest
@testable import NanocodexRemote

@MainActor private final class FullscreenSocket: RemoteSignalingTransport {
    var onMessage: (RemoteMessage) -> Void = { _ in }
    var onClose: (Error?) -> Void = { _ in }
    var messages: [RemoteMessage] = []
    var connections = 0
    var closed = false
    func connect(hand: RemoteHand?) throws { connections += 1; onMessage(.init(type: "ready")) }
    func send(_ message: RemoteMessage) { messages.append(message) }
    func close(error: Error?) { closed = true }

    var controls: [RemoteControlMessage] {
        messages.compactMap { if case .control(let control) = $0.data { return control }; return nil }
    }
    func grant(_ generation: String) {
        var message = RemoteMessage(type: "control")
        message.data = .control(.init(type: .granted, generation: generation))
        onMessage(message)
    }
}

final class RemoteFullscreenTests: XCTestCase {
    @MainActor private func connectedViewer() async throws -> (RemoteViewer, FullscreenSocket, RemoteService) {
        let service = try RemoteService(origin: URL(string: "https://fullscreen.test")!) { _ in }
        let hand = try JSONDecoder().decode(RemoteHand.self, from: Data(#"{"id":"desktop","machine_id":"vm:test","machine_name":"Test VM","name":"Desktop","kind":"vm","width":3,"height":2,"controllable":true,"generation":"test","transport":"frames-v1"}"#.utf8))
        let socket = FullscreenSocket(), viewer = RemoteViewer()
        viewer.makeSignaling = { _ in socket }
        await viewer.connect(service: service, hand: hand)
        let context = try XCTUnwrap(CGContext(data: nil, width: 3, height: 2, bitsPerComponent: 8,
            bytesPerRow: 12, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        let bytes = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(bytes, "public.jpeg" as CFString, 1, nil))
        CGImageDestinationAddImage(destination, try XCTUnwrap(context.makeImage()), nil)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        var frame = RemoteMessage(type: "frame")
        frame.jpeg = (bytes as Data).base64EncodedString(); frame.width = 3; frame.height = 2
        socket.onMessage(frame)
        XCTAssertTrue(viewer.connected)
        return (viewer, socket, service)
    }

    // Open and cancel in the same main-actor turn: exercise real ownership and
    // transport behavior without activating a fullscreen Space on the test Mac.
    @MainActor func testOpeningTransfersExistingViewerWithoutReconnectOrAcquire() async throws {
        let (viewer, socket, service) = try await connectedViewer()
        defer { viewer.close(); service.close() }
        viewer.takeControl(); socket.grant("initial")
        XCTAssertTrue(viewer.controlling)
        let identity = viewer.hand?.identity
        let fullscreen = RemoteFullscreen()
        fullscreen.open(viewer: viewer)
        XCTAssertTrue(fullscreen.isPresented)
        XCTAssertFalse(viewer.controlling)
        XCTAssertEqual(socket.controls.map(\.type), [.acquire, .release])
        fullscreen.close()
        XCTAssertFalse(fullscreen.isPresented)
        XCTAssertTrue(viewer.connected)
        XCTAssertEqual(viewer.hand?.identity, identity)
        XCTAssertEqual(socket.connections, 1)
        XCTAssertFalse(socket.closed)
        await Task.yield()
        XCTAssertFalse(fullscreen.isPresented, "Cancelled presentation must not reopen on the next run loop")
    }

    @MainActor func testWindowDeactivationAndCloseReleaseControlWithoutClosingSession() async throws {
        let (viewer, socket, service) = try await connectedViewer()
        defer { viewer.close(); service.close() }
        let fullscreen = RemoteFullscreen()
        fullscreen.open(viewer: viewer)
        viewer.takeControl(); socket.grant("active")
        XCTAssertTrue(viewer.controlling)
        fullscreen.windowDidResignKey(Notification(name: NSWindow.didResignKeyNotification))
        XCTAssertFalse(viewer.controlling)
        XCTAssertEqual(socket.controls.last?.type, .release)
        fullscreen.windowWillClose(Notification(name: NSWindow.willCloseNotification))
        XCTAssertFalse(fullscreen.isPresented)
        XCTAssertTrue(viewer.connected)
        XCTAssertFalse(socket.closed)
        XCTAssertEqual(socket.controls.filter { $0.type == .release }.count, 1)
    }

    @MainActor func testClosingActiveWindowReleasesLeaseExactlyOnce() async throws {
        let (viewer, socket, service) = try await connectedViewer()
        defer { viewer.close(); service.close() }
        let fullscreen = RemoteFullscreen()
        fullscreen.open(viewer: viewer)
        viewer.takeControl(); socket.grant("closing")
        XCTAssertTrue(viewer.controlling)
        fullscreen.windowWillClose(Notification(name: NSWindow.willCloseNotification))
        fullscreen.close()
        XCTAssertFalse(viewer.controlling)
        XCTAssertFalse(fullscreen.isPresented)
        XCTAssertTrue(viewer.connected)
        XCTAssertEqual(socket.controls.filter { $0.type == .release }.count, 1)
    }

    @MainActor func testDisconnectClearsControlAndClosingDoesNotReconnect() async throws {
        let (viewer, socket, service) = try await connectedViewer()
        defer { viewer.close(); service.close() }
        let fullscreen = RemoteFullscreen()
        fullscreen.open(viewer: viewer)
        viewer.takeControl(); socket.grant("active")
        XCTAssertTrue(viewer.controlling)
        socket.onClose(RemoteError.unauthorized)
        XCTAssertFalse(viewer.connected)
        XCTAssertFalse(viewer.controlling)
        fullscreen.close()
        XCTAssertFalse(fullscreen.isPresented)
        XCTAssertEqual(socket.connections, 1)
    }
}
#endif
