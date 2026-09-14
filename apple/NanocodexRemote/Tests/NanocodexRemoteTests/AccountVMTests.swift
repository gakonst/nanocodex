#if os(macOS)
import XCTest
import WebRTC
@testable import NanocodexRemote

private final class VMFirstFrame: NSObject, RTCVideoRenderer, @unchecked Sendable {
    private let lock = NSLock()
    private var received: (time: TimeInterval, width: Int, height: Int)?
    var first: (time: TimeInterval, width: Int, height: Int)? { lock.withLock { received } }
    func setSize(_ size: CGSize) {}
    func renderFrame(_ frame: RTCVideoFrame?) {
        guard let frame, frame.width > 0, frame.height > 0 else { return }
        lock.withLock {
            if received == nil { received = (ProcessInfo.processInfo.systemUptime, Int(frame.width), Int(frame.height)) }
        }
    }
}

private final class VMRequestTimeline: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [(String, TimeInterval)] = []
    func record(_ path: String) { lock.withLock { recorded.append((path, ProcessInfo.processInfo.systemUptime)) } }
    func events(since start: TimeInterval) -> [[String: Any]] {
        lock.withLock { recorded.map { ["path": $0.0, "started_ms": Int(($0.1 - start) * 1000)] } }
    }
}

final class AccountVMTests: XCTestCase {
    // Requires an explicitly selected disposable VM with an idle, focused foot
    // terminal. Measures decoded pixels after real input, not a signaling ack.
    @MainActor func testVMInputToVisibleFrame() async throws {
        let environment = ProcessInfo.processInfo.environment
        let (origin, token, machine) = try fixture()
        let service = try RemoteService(origin: origin) { $0.setValue("Bearer " + token, forHTTPHeaderField: "Authorization") }
        let viewer = RemoteViewer()
        defer {
            print("Native VM final state: \(viewer.status); \(viewer.diagnosticRecovery)")
            viewer.close(); service.close()
        }
        let hands = try await service.list()
        let hand = try XCTUnwrap(hands.first { $0.machineID == machine && $0.kind == .vm })
        await viewer.connect(service: service, hand: hand)
        print("Native VM waiting for video: \(viewer.status); \(viewer.diagnosticRecovery)")
        do { try await eventually(timeout: 40) { viewer.connected && viewer.track != nil } }
        catch { print("Native VM ICE: \(await viewer.diagnosticICE())"); throw error }
        print("Native VM video connected: \(viewer.diagnosticRecovery)")
        // Cancel before the first grant can return, then explicitly retake. A
        // cancelled grant must be released without ever enabling input.
        viewer.takeControl(); viewer.releaseControl(); viewer.takeControl()
        XCTAssertFalse(viewer.controlling)
        try await eventually { viewer.controlling }
        print("Native VM cancelled pending acquire and explicit retake passed")

        // Keep these calls adjacent: the real host's generationless `revoked`
        // acknowledgement used to cancel this next request and strand Watching.
        viewer.releaseControl(); viewer.takeControl()
        XCTAssertFalse(viewer.controlling)
        try await eventually { viewer.controlling }
        print("Native VM immediate release and reacquire passed")
        let checkpoint = environment["NANOCODEX_TEST_VM_RESTART_CHECKPOINT"]
        try await verifyInput(viewer, release: checkpoint == nil)

        if let checkpoint {
            XCTAssertTrue(viewer.controlling)
            let started = ProcessInfo.processInfo.systemUptime
            try Data("REMOTE_VM_RESTART_READY\n".utf8).write(to: URL(fileURLWithPath: checkpoint), options: .atomic)
            print("REMOTE_VM_RESTART_READY machine=\(machine) generation=\(hand.generation)")
            try await eventually(timeout: 30) { !viewer.connected }
            XCTAssertNil(viewer.track, "A disconnected VM must not retain stale video")
            XCTAssertFalse(viewer.controlling, "A disconnected VM must release its input lease")
            XCTAssertEqual(viewer.hand?.machineID, machine)
            try await eventually(timeout: 90) {
                viewer.connected && viewer.track != nil && viewer.hand?.generation != hand.generation
            }
            XCTAssertFalse(viewer.controlling, "A recovered VM must require a new control request")
            XCTAssertEqual(viewer.hand?.machineID, machine)
            print("Native VM automatic restart recovery: \(Int((ProcessInfo.processInfo.systemUptime - started) * 1000)) ms; generation=\(viewer.hand!.generation)")
            try await verifyInput(viewer)
        }
    }

    // Read-only benchmark: a real decoded frame establishes readiness. Each
    // sample creates a fresh authenticated service and viewer; later samples
    // additionally show the effect of the process's warm WebRTC factory.
    @MainActor func testVMTimeToFirstDecodedFrame() async throws {
        let samples = Int(ProcessInfo.processInfo.environment["NANOCODEX_TEST_VM_SAMPLES"] ?? "3") ?? 3
        for sample in 1...max(1, min(samples, 10)) {
            let (origin, token, machine) = try fixture(), timeline = VMRequestTimeline()
            let service = try RemoteService(origin: origin) {
                $0.setValue("Bearer " + token, forHTTPHeaderField: "Authorization")
                timeline.record($0.url?.path ?? "unknown")
            }
            let viewer = RemoteViewer(), renderer = VMFirstFrame()
            var renderedTrack: RTCVideoTrack?
            let trackObserver = viewer.$track.sink { track in
                renderedTrack?.remove(renderer)
                renderedTrack = track
                track?.add(renderer)
            }
            defer {
                trackObserver.cancel(); renderedTrack?.remove(renderer)
                viewer.close(); service.close()
            }
            let started = ProcessInfo.processInfo.systemUptime
            let hands = try await service.list()
            let hand = try XCTUnwrap(hands.first { $0.machineID == machine && $0.kind == .vm })
            let selected = ProcessInfo.processInfo.systemUptime
            var states: [[String: Any]] = []
            var events: [[String: Any]] = []
            viewer.connectionEvent = { event in
                events.append(["elapsed_ms": Int((ProcessInfo.processInfo.systemUptime - selected) * 1000), "event": event])
            }
            let stateObserver = Task {
                var previous = ""
                var capturedSlowICE = false
                while !Task.isCancelled {
                    let state = "\(viewer.status); \(viewer.diagnosticRecovery); track=\(viewer.track != nil)"
                    if state != previous {
                        states.append(["elapsed_ms": Int((ProcessInfo.processInfo.systemUptime - selected) * 1000), "state": state])
                        previous = state
                    }
                    if !capturedSlowICE && !viewer.connected && ProcessInfo.processInfo.systemUptime - selected > 3 {
                        capturedSlowICE = true
                        let ice = await viewer.diagnosticICE(includeAddresses: false)
                        states.append(["elapsed_ms": Int((ProcessInfo.processInfo.systemUptime - selected) * 1000), "ice": ice])
                    }
                    do { try await Task.sleep(for: .milliseconds(100)) } catch { return }
                }
            }
            defer { stateObserver.cancel() }
            await viewer.connect(service: service, hand: hand)
            do { try await eventually(timeout: 40) { renderer.first != nil } }
            catch { print("VM first-frame diagnostics: \(viewer.diagnosticRecovery); \(await viewer.diagnosticICE())"); throw error }
            let frame = try XCTUnwrap(renderer.first)
            XCTAssertFalse(viewer.controlling, "A first-frame benchmark must not acquire control")
            let receipt: [String: Any] = [
                "sample": sample, "catalog_ms": Int((selected - started) * 1000),
                "selection_to_decoded_frame_ms": Int((frame.time - selected) * 1000),
                "catalog_to_decoded_frame_ms": Int((frame.time - started) * 1000),
                "frame_width": frame.width, "frame_height": frame.height,
                "requests": timeline.events(since: started),
                "states": states, "events": events, "final_state": viewer.diagnosticRecovery,
                "ice": await viewer.diagnosticICE(includeAddresses: false),
                "initial_generation": hand.generation, "final_generation": viewer.hand?.generation ?? "",
            ]
            print("VM_FIRST_FRAME " + String(decoding: try JSONSerialization.data(withJSONObject: receipt, options: [.sortedKeys]), as: UTF8.self))
        }
    }

    private func fixture() throws -> (origin: URL, token: String, machine: String) {
        let environment = ProcessInfo.processInfo.environment
        guard let path = environment["NANOCODEX_TEST_REMOTE_ENV"],
              let machine = environment["NANOCODEX_TEST_VM_MACHINE_ID"] else {
            throw XCTSkip("Requires an explicitly selected isolated factory VM")
        }
        var values: [String: String] = [:]
        for line in try String(contentsOfFile: path, encoding: .utf8).split(separator: "\n") {
            guard let split = line.firstIndex(of: "=") else { continue }
            values[String(line[..<split])] = String(line[line.index(after: split)...])
        }
        let origin = try XCTUnwrap(URL(string: try XCTUnwrap(values["NANOCODEX_MANAGED_URL"])))
        let local = ["127.0.0.1", "localhost"].contains(origin.host ?? "")
        let live = environment["NANOCODEX_TEST_VM_LIVE"] == "1" && origin.scheme == "https"
        guard (local || live), machine.hasPrefix("vm:") else {
            throw XCTSkip("Select a disposable VM; a live account also requires NANOCODEX_TEST_VM_LIVE=1")
        }
        let token = try XCTUnwrap(environment["NANOCODEX_API_KEY"] ?? values["NANOCODEX_API_KEY"])
        return (origin, token, machine)
    }

    @MainActor private func verifyInput(_ viewer: RemoteViewer, release: Bool = true) async throws {
        let track = try XCTUnwrap(viewer.track), transition = ScreenTransition()
        track.add(transition); defer { track.remove(transition) }
        print("Native VM waiting for control")
        viewer.takeControl(); try await eventually { viewer.controlling }
        print("Native VM control granted; waiting for decoded stable frames")
        try await eventually { transition.stable }
        for escape in ["47m", "0m"] {
            viewer.input(kind: .text, text: "printf '\\033[" + escape + "\\033[2J\\033[H'")
            try await Task.sleep(for: .milliseconds(150))
            try await eventually { transition.stable }
            transition.arm()
            for down in [true, false] { viewer.input(kind: .key, down: down, key: 40) }
            try await eventually { transition.milliseconds != nil }
            print("Local VM input to first visible transition: \(Int(transition.milliseconds!)) ms")
            try await eventually { transition.stable }
        }
        if release {
            viewer.releaseControl()
            XCTAssertFalse(viewer.controlling)
        }
    }

    @MainActor private func eventually(timeout: TimeInterval = 15, _ predicate: () -> Bool) async throws {
        let deadline = ProcessInfo.processInfo.systemUptime + timeout
        while ProcessInfo.processInfo.systemUptime < deadline {
            if predicate() { return }
            try await Task.sleep(for: .milliseconds(20))
        }
        XCTFail("The VM did not reach the expected decoded video or input state")
        throw RemoteError.unavailable
    }
}
#endif
