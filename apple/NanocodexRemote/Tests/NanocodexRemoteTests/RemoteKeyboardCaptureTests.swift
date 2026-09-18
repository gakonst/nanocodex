#if os(macOS)
import AppKit
import ImageIO
import XCTest
@testable import NanocodexRemote

final class RemoteKeyboardCaptureTests: XCTestCase {
    private final class Tap: RemoteKeyboardTap {
        var enabled = true
        var canEnable = true
        var enables = 0
        var invalidations = 0
        func enable() { enables += 1; enabled = canEnable }
        func invalidate() { invalidations += 1; enabled = false }
    }

    func testPermissionDenialDoesNotCreateTapAndGrantRetriesImmediately() {
        var trusted = false
        var creates = 0
        let tap = Tap()
        let capture = RemoteKeyboardTapController(trust: { trusted }) { creates += 1; return tap }
        capture.start()
        XCTAssertEqual(capture.status, .accessibilityRequired)
        XCTAssertFalse(capture.trusted)
        XCTAssertEqual(creates, 0)
        trusted = true
        capture.start()
        XCTAssertEqual(capture.status, .capturing)
        XCTAssertTrue(capture.trusted)
        XCTAssertTrue(capture.enabled)
        XCTAssertEqual(creates, 1)
    }

    func testTrustedCreationFailureHasSeparateDiagnosticAndBoundedRetry() {
        var time = Date(timeIntervalSince1970: 100)
        var creates = 0
        let capture = RemoteKeyboardTapController(trust: { true }, now: { time }) { creates += 1; return nil }
        capture.start()
        XCTAssertTrue(capture.trusted)
        XCTAssertEqual(capture.status, .creationFailed)
        for _ in 0..<20 { capture.start() }
        XCTAssertEqual(creates, 1)
        time.addTimeInterval(2)
        capture.start()
        XCTAssertEqual(creates, 2)
    }

    func testExistingDisabledTapRecoversOnForegroundWithoutRecreation() {
        let tap = Tap()
        var creates = 0
        let capture = RemoteKeyboardTapController(trust: { true }) { creates += 1; return tap }
        capture.start()
        tap.enabled = false // Disabled while capture was not active.
        XCTAssertFalse(capture.enabled)
        capture.start()
        XCTAssertEqual(capture.status, .capturing)
        XCTAssertTrue(capture.enabled)
        XCTAssertEqual(tap.enables, 1)
        XCTAssertEqual(creates, 1)
        XCTAssertEqual(tap.invalidations, 0)
    }

    func testTapThatCannotBeReenabledIsReplacedAfterRetryInterval() {
        var time = Date(timeIntervalSince1970: 100)
        let first = Tap(), replacement = Tap()
        var creates = 0
        let capture = RemoteKeyboardTapController(trust: { true }, now: { time }) {
            creates += 1; return creates == 1 ? first : replacement
        }
        capture.start()
        first.enabled = false; first.canEnable = false
        capture.start()
        XCTAssertEqual(capture.status, .disabled)
        XCTAssertEqual(creates, 1)
        time.addTimeInterval(2)
        capture.start()
        XCTAssertEqual(capture.status, .capturing)
        XCTAssertEqual(first.invalidations, 1)
        XCTAssertEqual(creates, 2)
    }

    func testRevokedPermissionInvalidatesPreviouslyWorkingTap() {
        var trusted = true
        let tap = Tap()
        let capture = RemoteKeyboardTapController(trust: { trusted }) { tap }
        capture.start()
        trusted = false
        capture.start()
        XCTAssertEqual(capture.status, .accessibilityRequired)
        XCTAssertFalse(capture.enabled)
        XCTAssertEqual(tap.invalidations, 1)
    }

    func testStopIsIdempotentAndReentryIsNotThrottled() {
        var creates = 0
        let tap = Tap()
        let capture = RemoteKeyboardTapController(trust: { true }) { creates += 1; return tap }
        capture.start()
        capture.stop(); capture.stop()
        XCTAssertEqual(capture.status, .stopped)
        XCTAssertFalse(capture.enabled)
        XCTAssertEqual(tap.invalidations, 1)
        capture.start()
        XCTAssertEqual(creates, 2)
        XCTAssertTrue(capture.enabled)
    }

    func testHeldModifiersAreReconciledBeforeFirstKeyOrMouseEvent() {
        var state = RemoteModifierState()
        let flags = NSEvent.ModifierFlags(rawValue: NSEvent.ModifierFlags.command.rawValue | 0x10)
        XCTAssertEqual(state.reconcile(flags), [.init(key: 231, down: true)])
        XCTAssertEqual(state.reconcile(flags), [], "Mouse motion must not repeat modifier downs")
        XCTAssertEqual(state.reconcile([]), [.init(key: 231, down: false)])
    }

    func testOverlappingSidesReleaseIndependently() {
        var state = RemoteModifierState()
        func flags(_ sides: UInt) -> NSEvent.ModifierFlags { .init(rawValue: NSEvent.ModifierFlags.shift.rawValue | sides) }
        XCTAssertEqual(state.reconcile(flags(0x2), changedKey: 225), [.init(key: 225, down: true)])
        XCTAssertEqual(state.reconcile(flags(0x6), changedKey: 229), [.init(key: 229, down: true)])
        XCTAssertEqual(state.reconcile(flags(0x4), changedKey: 225), [.init(key: 225, down: false)])
        XCTAssertEqual(state.reconcile([], changedKey: 229), [.init(key: 229, down: false)])
        XCTAssertTrue(state.held.isEmpty)
    }

    func testSyntheticModifierEventsWithoutSideBitsPreserveKnownSides() {
        var state = RemoteModifierState()
        XCTAssertEqual(state.reconcile(.option, changedKey: 230), [.init(key: 230, down: true)])
        XCTAssertEqual(state.reconcile(.option), [])
        XCTAssertEqual(state.reconcile(.option, changedKey: 226), [.init(key: 226, down: true)])
        XCTAssertEqual(state.reconcile(.option, changedKey: 230), [.init(key: 230, down: false)])
        XCTAssertEqual(state.reconcile([], changedKey: 226), [.init(key: 226, down: false)])
    }

    func testFocusReleaseAllowsHeldModifierToBeRestoredOnNextEvent() {
        var state = RemoteModifierState()
        _ = state.reconcile(.control)
        state.reset()
        XCTAssertTrue(state.held.isEmpty)
        XCTAssertEqual(state.reconcile(.control), [.init(key: 224, down: true)])
    }

    func testOnlyCommandShiftEscapeIsExit() {
        XCTAssertTrue(RemoteModifierState.isExit(keyCode: 53, flags: [.command, .shift]))
        XCTAssertTrue(RemoteModifierState.isExit(keyCode: 53, flags: [.command, .shift, .capsLock]))
        for flags: NSEvent.ModifierFlags in [[], .command, .shift, .option, [.command, .option], [.command, .shift, .control], [.command, .shift, .option], [.command, .shift, .function]] {
            XCTAssertFalse(RemoteModifierState.isExit(keyCode: 53, flags: flags))
        }
        for key: UInt16 in [48, 12, 13, 3, 49] { // Tab, Q, W, F, Space
            XCTAssertFalse(RemoteModifierState.isExit(keyCode: key, flags: [.command, .shift]))
        }
    }

    func testReleasingOneMouseButtonDoesNotEndAnotherButtonsDrag() {
        var pointer = RemotePointerState()
        pointer.button(0, down: true); pointer.button(1, down: true)
        pointer.button(0, down: false)
        XCTAssertTrue(pointer.dragging)
        XCTAssertEqual(pointer.buttons, [1])
        pointer.button(1, down: false)
        XCTAssertFalse(pointer.dragging)
        pointer.button(2, down: true); pointer.reset()
        XCTAssertFalse(pointer.dragging)
    }
    // This transport is entirely in memory. No window is activated, no event tap
    // is installed, and no input is posted to the local or a remote desktop.
    @MainActor private final class Socket: RemoteSignalingTransport {
        var onMessage: (RemoteMessage) -> Void = { _ in }
        var onClose: (Error?) -> Void = { _ in }
        var messages: [RemoteMessage] = []
        func connect(hand: RemoteHand?) throws { onMessage(.init(type: "ready")) }
        func send(_ message: RemoteMessage) { messages.append(message) }
        func close(error: Error?) {}
        var inputs: [RemoteInput] {
            messages.compactMap { if case .input(let input) = $0.data { return input }; return nil }
        }
    }

    @MainActor private func connect(_ viewer: RemoteViewer, socket: Socket, service: RemoteService, acquire: Bool = true) async throws {
        let catalog = #"{"id":"screen","machine_id":"test","machine_name":"Test","name":"Screen","kind":"vm","width":3,"height":2,"controllable":true,"generation":"surface","transport":"frames-v1"}"#
        let hand = try JSONDecoder().decode(RemoteHand.self, from: Data(catalog.utf8))
        viewer.makeSignaling = { _ in socket }
        await viewer.connect(service: service, hand: hand)
        let context = try XCTUnwrap(CGContext(data: nil, width: 3, height: 2, bitsPerComponent: 8,
            bytesPerRow: 12, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        let data = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(data, "public.jpeg" as CFString, 1, nil))
        CGImageDestinationAddImage(destination, try XCTUnwrap(context.makeImage()), nil)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        var frame = RemoteMessage(type: "frame")
        frame.jpeg = (data as Data).base64EncodedString(); frame.width = 3; frame.height = 2
        socket.onMessage(frame)
        XCTAssertTrue(viewer.connected)
        if !acquire { socket.messages.removeAll(); return }
        viewer.takeControl()
        var grant = RemoteMessage(type: "control"); grant.data = .control(.init(type: .granted, generation: "lease"))
        socket.onMessage(grant)
        XCTAssertTrue(viewer.controlling)
        socket.messages.removeAll()
    }

    @MainActor private func key(_ code: UInt16, down: Bool, flags: NSEvent.ModifierFlags) throws -> NSEvent {
        try XCTUnwrap(NSEvent.keyEvent(with: down ? .keyDown : .keyUp, location: .zero, modifierFlags: flags,
            timestamp: 0, windowNumber: 0, context: nil, characters: "", charactersIgnoringModifiers: "",
            isARepeat: false, keyCode: code))
    }

    @MainActor func testCanvasForwardsCommandTabWithAlreadyHeldModifierAndReleasesOnExit() async throws {
        let viewer = RemoteViewer(), socket = Socket()
        let service = try RemoteService(origin: URL(string: "https://remote.invalid")!) { _ in XCTFail("Unexpected HTTP") }
        // Initialize before control is acquired so no keyboard tap is started.
        let canvas = MacRemoteCanvas(viewer: viewer); canvas.immersive = true
        defer { canvas.detach(); viewer.close(); service.close() }
        try await connect(viewer, socket: socket, service: service)
        canvas.keyDown(with: try key(48, down: true, flags: .command))
        canvas.keyUp(with: try key(48, down: false, flags: .command))
        XCTAssertEqual(socket.inputs.map(\.key), [227, 43, 43])
        XCTAssertEqual(socket.inputs.map(\.down), [true, true, false])
        XCTAssertTrue(viewer.controlling, "Command-Tab must not release the lease")
        var exited = false
        canvas.onExit = { exited = true }
        canvas.keyDown(with: try key(53, down: true, flags: [.command, .shift]))
        XCTAssertTrue(exited)
        XCTAssertFalse(viewer.controlling, "Exit releases immediately, before SwiftUI updates visibility")
        XCTAssertEqual(socket.inputs.last?.kind, .releaseAll)
        XCTAssertFalse(socket.inputs.contains { $0.key == 41 }, "The exit chord must never reach the host")
        let release = socket.messages.last { if case .control(let message) = $0.data { return message.type == .release }; return false }
        XCTAssertNotNil(release)
    }

    @MainActor func testCanvasEscapeAndOtherShortcutChordsRemainRemote() async throws {
        let viewer = RemoteViewer(), socket = Socket()
        let service = try RemoteService(origin: URL(string: "https://remote.invalid")!) { _ in XCTFail("Unexpected HTTP") }
        let canvas = MacRemoteCanvas(viewer: viewer); canvas.immersive = true
        defer { canvas.detach(); viewer.close(); service.close() }
        try await connect(viewer, socket: socket, service: service)
        canvas.onExit = { XCTFail("Only Command-Shift-Escape may exit") }
        for (code, flags): (UInt16, NSEvent.ModifierFlags) in [(53, []), (48, .option), (12, .command), (13, .command), (3, [.control, .command])] {
            canvas.keyDown(with: try key(code, down: true, flags: flags))
            canvas.keyUp(with: try key(code, down: false, flags: flags))
        }
        XCTAssertTrue(viewer.controlling)
        for usage: UInt16 in [41, 43, 20, 26, 9] {
            XCTAssertTrue(socket.inputs.contains { $0.key == usage && $0.down == true })
            XCTAssertTrue(socket.inputs.contains { $0.key == usage && $0.down == false })
        }
    }

    @MainActor func testHoverWithoutKeyboardFocusCannotLatchRemoteModifiers() async throws {
        let viewer = RemoteViewer(), socket = Socket()
        let service = try RemoteService(origin: URL(string: "https://remote.invalid")!) { _ in XCTFail("Unexpected HTTP") }
        let canvas = MacRemoteCanvas(viewer: viewer)
        defer { canvas.detach(); viewer.close(); service.close() }
        try await connect(viewer, socket: socket, service: service)
        canvas.frame = CGRect(x: 0, y: 0, width: 300, height: 200)
        canvas.layout()
        let event = try XCTUnwrap(NSEvent.mouseEvent(with: .mouseMoved, location: CGPoint(x: 150, y: 100),
            modifierFlags: .command, timestamp: 0, windowNumber: 0, context: nil, eventNumber: 0, clickCount: 0, pressure: 0))
        canvas.mouseMoved(with: event)
        XCTAssertEqual(socket.inputs.map(\.kind), [.move])
    }

    func testCapsLockFlagsChangesBecomeOneBalancedHIDToggleEach() {
        var caps = RemoteCapsLockState()
        XCTAssertEqual(caps.observe(false, changed: false), [])
        XCTAssertEqual(caps.observe(true, changed: true), [.init(key: 57, down: true), .init(key: 57, down: false)])
        XCTAssertEqual(caps.observe(true, changed: true), [], "Key release with unchanged lock flags must not toggle again")
        XCTAssertEqual(caps.observe(false, changed: true), [.init(key: 57, down: true), .init(key: 57, down: false)])
        caps.reset()
        XCTAssertEqual(caps.observe(true, changed: false), [], "Held Caps Lock at focus entry is a baseline, not a fabricated press")
    }

    func testMacHostCapsLockPersistsThroughLeaseCleanupWithoutBeingHeld() {
        var keyboard = RemoteMacKeyboardState(capsLock: false)
        XCTAssertEqual(keyboard.apply(key: 57, down: true), .flagsChanged)
        XCTAssertTrue(keyboard.flags.contains(.maskAlphaShift))
        XCTAssertNil(keyboard.apply(key: 57, down: true), "Repeat must not toggle a locking key")
        XCTAssertNil(keyboard.apply(key: 57, down: false))
        _ = keyboard.apply(key: 56, down: true)
        _ = keyboard.apply(key: 0, down: true)
        XCTAssertTrue(keyboard.flags.contains(.maskShift))
        XCTAssertEqual(keyboard.release(), [0, 56])
        XCTAssertEqual(keyboard.flags, .maskAlphaShift)
        XCTAssertEqual(keyboard.apply(key: 57, down: true), .flagsChanged)
        XCTAssertFalse(keyboard.capsLock)
        XCTAssertEqual(keyboard.release(), [], "Caps Lock is not a held key to release")
    }

    @MainActor private func controlMessage(_ type: RemoteControlMessage.Kind, generation: String? = nil, socket: Socket) {
        var message = RemoteMessage(type: "control"); message.data = .control(.init(type: type, generation: generation))
        socket.onMessage(message)
    }
    @MainActor private func acquires(_ socket: Socket) -> Int {
        socket.messages.filter { if case .control(let message) = $0.data { return message.type == .acquire }; return false }.count
    }
    @MainActor private func focus(_ viewer: RemoteViewer, active: Bool) {
        viewer.updateControlFocus(.init(immersive: true, active: active, connected: viewer.connected, selection: viewer.hand?.identity))
    }

    @MainActor func testViewerDeactivationCleansHeldInputThenReleasesAndRetakesSameViewer() async throws {
        let viewer = RemoteViewer(), socket = Socket()
        let service = try RemoteService(origin: URL(string: "https://remote.invalid")!) { _ in XCTFail("Unexpected HTTP") }
        defer { viewer.close(); service.close() }
        try await connect(viewer, socket: socket, service: service)
        focus(viewer, active: true)
        viewer.input(kind: .key, down: true, key: 225)
        focus(viewer, active: false)
        XCTAssertFalse(viewer.controlling)
        XCTAssertEqual(socket.inputs.last?.kind, .releaseAll)
        guard case .control(let release) = socket.messages.last?.data else { return XCTFail("Lease release must follow input cleanup") }
        XCTAssertEqual(release.type, .release)
        controlMessage(.revoked, generation: "lease", socket: socket)
        XCTAssertEqual(acquires(socket), 0, "Release acknowledgement in background must not reacquire")
        focus(viewer, active: true)
        XCTAssertEqual(acquires(socket), 1)
        controlMessage(.granted, generation: "foreground", socket: socket)
        XCTAssertTrue(viewer.controlling)
    }

    @MainActor func testLateDenialCancelsQueuedForegroundRetakeWithoutLoop() async throws {
        let viewer = RemoteViewer(), socket = Socket()
        let service = try RemoteService(origin: URL(string: "https://remote.invalid")!) { _ in XCTFail("Unexpected HTTP") }
        defer { viewer.close(); service.close() }
        try await connect(viewer, socket: socket, service: service)
        focus(viewer, active: true); focus(viewer, active: false)
        controlMessage(.revoked, generation: "lease", socket: socket)
        focus(viewer, active: true) // acquire pending
        focus(viewer, active: false) // cancel pending acquire
        focus(viewer, active: true) // queued retake, waiting for outcome
        XCTAssertEqual(acquires(socket), 1)
        controlMessage(.denied, socket: socket)
        XCTAssertEqual(acquires(socket), 1, "Late denial must cancel the queued automatic retry")
        for _ in 0..<3 { focus(viewer, active: false); focus(viewer, active: true) }
        XCTAssertEqual(acquires(socket), 1)
        XCTAssertFalse(viewer.controlling)
        viewer.takeControl() // A new explicit request is still allowed.
        XCTAssertEqual(acquires(socket), 2)
    }

    @MainActor func testHumanRevocationCannotReacquireOnFocusReturn() async throws {
        let viewer = RemoteViewer(), socket = Socket()
        let service = try RemoteService(origin: URL(string: "https://remote.invalid")!) { _ in XCTFail("Unexpected HTTP") }
        defer { viewer.close(); service.close() }
        try await connect(viewer, socket: socket, service: service)
        focus(viewer, active: true)
        controlMessage(.revoked, generation: "lease", socket: socket)
        for _ in 0..<3 { focus(viewer, active: false); focus(viewer, active: true) }
        XCTAssertFalse(viewer.controlling)
        XCTAssertEqual(acquires(socket), 0)
    }

    @MainActor func testCanvasCapsLockEmitsBalancedSupportedWireMapping() async throws {
        let viewer = RemoteViewer(), socket = Socket()
        let service = try RemoteService(origin: URL(string: "https://remote.invalid")!) { _ in XCTFail("Unexpected HTTP") }
        let canvas = MacRemoteCanvas(viewer: viewer); canvas.immersive = true
        defer { canvas.detach(); viewer.close(); service.close() }
        try await connect(viewer, socket: socket, service: service)
        for flags: CGEventFlags in [.maskAlphaShift, .maskAlphaShift, []] {
            let cg = try XCTUnwrap(CGEvent(keyboardEventSource: nil, virtualKey: 57, keyDown: true))
            cg.type = .flagsChanged; cg.flags = flags
            canvas.flagsChanged(with: try XCTUnwrap(NSEvent(cgEvent: cg)))
        }
        XCTAssertEqual(socket.inputs.map(\.key), [57, 57, 57, 57])
        XCTAssertEqual(socket.inputs.map(\.down), [true, false, true, false])
        for input in socket.inputs { XCTAssertNoThrow(try input.validate()) }
    }

    @MainActor func testViewerBackgroundConnectDoesNotAcquireWithoutForegroundIntent() async throws {
        let viewer = RemoteViewer(), socket = Socket()
        let service = try RemoteService(origin: URL(string: "https://remote.invalid")!) { _ in XCTFail("Unexpected HTTP") }
        defer { viewer.close(); service.close() }
        viewer.updateControlFocus(.init(immersive: true, active: false, connected: false))
        try await connect(viewer, socket: socket, service: service, acquire: false)
        focus(viewer, active: false)
        XCTAssertEqual(acquires(socket), 0)
        focus(viewer, active: true)
        XCTAssertEqual(acquires(socket), 0)
        viewer.takeControl()
        XCTAssertEqual(acquires(socket), 1)
    }

    @MainActor func testViewerLateBackgroundConnectRetainsOnlyPriorForegroundIntent() async throws {
        let viewer = RemoteViewer(), socket = Socket()
        let service = try RemoteService(origin: URL(string: "https://remote.invalid")!) { _ in XCTFail("Unexpected HTTP") }
        defer { viewer.close(); service.close() }
        viewer.updateControlFocus(.init(immersive: true, active: true, connected: false))
        viewer.updateControlFocus(.init(immersive: true, active: false, connected: false))
        try await connect(viewer, socket: socket, service: service, acquire: false)
        focus(viewer, active: false)
        XCTAssertEqual(acquires(socket), 0)
        focus(viewer, active: true)
        XCTAssertEqual(acquires(socket), 1)
    }

    @MainActor func testViewerRapidFocusReturnWaitsForReleaseAcknowledgement() async throws {
        let viewer = RemoteViewer(), socket = Socket()
        let service = try RemoteService(origin: URL(string: "https://remote.invalid")!) { _ in XCTFail("Unexpected HTTP") }
        defer { viewer.close(); service.close() }
        try await connect(viewer, socket: socket, service: service)
        focus(viewer, active: true); focus(viewer, active: false); focus(viewer, active: true)
        XCTAssertEqual(acquires(socket), 0, "New acquire must wait for old lease release acknowledgement")
        controlMessage(.revoked, generation: "lease", socket: socket)
        XCTAssertEqual(acquires(socket), 1)
        focus(viewer, active: true)
        XCTAssertEqual(acquires(socket), 1)
    }

    @MainActor func testWindowFocusObserverReleasesBeforeSwiftUICanCoalesceTransitions() async throws {
        let viewer = RemoteViewer(), socket = Socket()
        let service = try RemoteService(origin: URL(string: "https://remote.invalid")!) { _ in XCTFail("Unexpected HTTP") }
        defer { viewer.close(); service.close() }
        try await connect(viewer, socket: socket, service: service)
        focus(viewer, active: true)
        let observer = RemoteWindowFocusObserver.FocusView { [self] foreground in focus(viewer, active: foreground) }
        defer { observer.stop() }
        // A notification in this test process only; no application activation,
        // window ordering, input injection, or remote network takes place.
        NotificationCenter.default.post(name: NSApplication.didResignActiveNotification, object: nil)
        XCTAssertFalse(viewer.controlling, "The observer must release synchronously, before a subsequent activation")
        XCTAssertEqual(socket.inputs.last?.kind, .releaseAll)
    }

}
#endif
