#if os(macOS)
import XCTest
import WebRTC
@testable import NanocodexRemote

// Observe decoded video without retaining screenshots or device contents. This
// measures the first substantial visible transition after the Home command,
// rather than confusing an HTTP acknowledgement with display latency.
final class ScreenTransition: NSObject, RTCVideoRenderer, @unchecked Sendable {
    private let lock = NSLock()
    private var previous: [UInt8] = []
    private var baseline: [UInt8]?
    private var stableFrames = 0
    private var started: TimeInterval?
    private var elapsed: TimeInterval?
    var stable: Bool { lock.lock(); defer { lock.unlock() }; return stableFrames >= 10 }
    var milliseconds: Double? { lock.lock(); defer { lock.unlock() }; return elapsed.map { $0 * 1000 } }
    func arm() { lock.lock(); defer { lock.unlock() }; baseline = previous; started = ProcessInfo.processInfo.systemUptime; elapsed = nil }
    func setSize(_ size: CGSize) {}
    func renderFrame(_ frame: RTCVideoFrame?) {
        guard let frame else { return }
        let buffer = frame.buffer.toI420()
        var samples: [UInt8] = []
        for row in 1...24 { for column in 1...16 {
            let y = Int(buffer.height) * row / 25, x = Int(buffer.width) * column / 17
            samples.append(buffer.dataY[y * Int(buffer.strideY) + x])
        } }
        func difference(_ other: [UInt8]) -> Double {
            guard other.count == samples.count else { return 255 }
            return Double(zip(samples, other).reduce(0) { $0 + abs(Int($1.0) - Int($1.1)) }) / Double(samples.count)
        }
        lock.lock(); defer { lock.unlock() }
        stableFrames = difference(previous) < 2 ? stableFrames + 1 : 0
        previous = samples
        if elapsed == nil, let started, let baseline, difference(baseline) > 12 {
            elapsed = ProcessInfo.processInfo.systemUptime - started
        }
    }
}

final class AccountPhoneTests: XCTestCase {
    @MainActor func testAccountPhoneControlAndDisconnect() async throws {
        guard let path = ProcessInfo.processInfo.environment["NANOCODEX_TEST_REMOTE_ENV"] else {
            throw XCTSkip("Requires a local account credential file and the paired phone bridge")
        }
        let lines = try String(contentsOfFile: path, encoding: .utf8).split(separator: "\n")
        var values: [String: String] = [:]
        for line in lines {
            guard let index = line.firstIndex(of: "=") else { continue }
            values[String(line[..<index])] = String(line[line.index(after: index)...])
        }
        let origin = try XCTUnwrap(URL(string: try XCTUnwrap(values["NANOCODEX_MANAGED_URL"])))
        XCTAssertTrue(["127.0.0.1", "localhost"].contains(origin.host ?? ""), "This test uses the isolated local account service")
        let token = try XCTUnwrap(values["NANOCODEX_API_KEY"])
        let service = try RemoteService(origin: origin) { $0.setValue("Bearer " + token, forHTTPHeaderField: "Authorization") }
        defer { service.close() }
        let host = RemoteMacHost(), viewer = RemoteViewer(), second = RemoteViewer()
        host.iceRenewalInterval = .seconds(3)
        let machineID = "phone-account-test-" + UUID().uuidString
        var bridge: PhoneBridgeConfiguration?
        let environment = ProcessInfo.processInfo.environment
        if let runner = environment["NANOCODEX_TEST_PHONE_RUNNER"], let deviceID = environment["NANOCODEX_TEST_PHONE_UDID"],
           let helper = environment["NANOCODEX_TEST_PHONE_HELPER"] {
            bridge = .init(deviceID: deviceID, runner: URL(fileURLWithPath: runner))
            bridge?.companionExecutable = URL(fileURLWithPath: helper)
        }
        await host.startPhone(service: service, machineID: machineID, name: "Paired phone account test", bridge: bridge)
        do {
            try await eventually("Phone published to the account") { host.sharing }
            let phoneLock = try await phoneRequest("/wda/locked")
            guard phoneLock["value"] as? Bool == false else {
                XCTFail("The paired phone must be unlocked for the control journey"); throw RemoteError.unavailable
            }
            let hands = try await service.list()
            let hand = try XCTUnwrap(hands.first { $0.machineID == machineID })
            await viewer.connect(service: service, hand: hand)
            try await eventually("Native viewer connected through account signaling") { viewer.connected && viewer.track != nil }
            let track = try XCTUnwrap(viewer.track), transition = ScreenTransition()
            track.add(transition)
            defer { track.remove(transition) }
            viewer.takeControl()
            try await eventually("First viewer acquired control") { viewer.controlling }
            await second.connect(service: service, hand: hand)
            try await eventually("Second viewer connected") { second.connected }
            second.takeControl()
            try await eventually("Second viewer was denied control") { second.status.contains("Another viewer") }
            XCTAssertFalse(second.controlling)

            var request = URLRequest(url: URL(string: "http://127.0.0.1:18100/wda/apps/launchUnattached")!)
            request.httpMethod = "POST"; request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = Data(#"{"bundleId":"com.apple.calculator"}"#.utf8)
            let (_, response) = try await URLSession.shared.data(for: request)
            XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200)
            // Open a reversible menu through the actual remote touch path,
            // leaving the user's calculator value and mode unchanged.
            let phoneStatus = try await phoneRequest("/status")
            let sessionID = try XCTUnwrap(phoneStatus["sessionId"] as? String)
            let buttons = try await phoneRequest("/session/\(sessionID)/elements", body: ["using": "predicate string",
                "value": "type == 'XCUIElementTypeButton' AND label == 'Change Mode'"])
            let button = try XCTUnwrap((buttons["value"] as? [[String: Any]])?.first)
            let elementID = try XCTUnwrap(button["element-6066-11e4-a52e-4f735466cecf"] as? String ?? button["ELEMENT"] as? String)
            let existingMenu = try await phoneRequest("/session/\(sessionID)/elements", body: ["using": "predicate string", "value": "label == 'Scientific'"])
            if let entries = existingMenu["value"] as? [Any], !entries.isEmpty {
                // Calculator restores this popover after Home. Dismiss it in
                // the empty area to its left, without choosing a calculator mode.
                for down in [true, false] { viewer.input(kind: .button, x: 0.05, y: 0.25, button: 0, down: down) }
            }
            // AX can expose the button while the launch animation is still
            // moving it. Wait for hit testing, then read the final coordinates.
            var hittable = false
            for _ in 0..<20 {
                let state = try await phoneRequest("/session/\(sessionID)/element/\(elementID)/attribute/hittable")
                if state["value"] as? Bool == true { hittable = true; break }
                try await Task.sleep(for: .milliseconds(100))
            }
            guard hittable else { XCTFail("Calculator's mode button never became hittable"); throw RemoteError.unavailable }
            let geometry = try await phoneRequest("/session/\(sessionID)/element/\(elementID)/rect")
            let rect = try XCTUnwrap(geometry["value"] as? [String: Double])
            let x = (try XCTUnwrap(rect["x"]) + XCTUnwrap(rect["width"]) / 2) / Double(hand.width - 1)
            let y = (try XCTUnwrap(rect["y"]) + XCTUnwrap(rect["height"]) / 2) / Double(hand.height - 1)
            for down in [true, false] { viewer.input(kind: .button, x: x, y: y, button: 0, down: down) }
            var menuOpened = false
            for _ in 0..<15 {
                let menu = try await phoneRequest("/session/\(sessionID)/elements", body: ["using": "predicate string", "value": "label == 'Scientific'"])
                if let entries = menu["value"] as? [Any], !entries.isEmpty { menuOpened = true; break }
                try await Task.sleep(for: .milliseconds(100))
            }
            guard menuOpened else {
                XCTFail("A WebRTC pointer down/up did not open the physical iPhone's Calculator mode menu")
                throw RemoteError.unavailable
            }
            try await Task.sleep(for: .seconds(1))
            try await eventually("Calculator video settled before measuring input") { transition.stable }
            transition.arm()
            viewer.input(kind: .key, down: true, key: 74)
            let deadline = ProcessInfo.processInfo.systemUptime + 5
            var home = false
            while ProcessInfo.processInfo.systemUptime < deadline {
                let (data, _) = try await URLSession.shared.data(from: URL(string: "http://127.0.0.1:18100/wda/activeAppInfo")!)
                let value = (try JSONSerialization.jsonObject(with: data) as? [String: Any])?["value"] as? [String: Any]
                if value?["bundleId"] as? String == "com.apple.springboard" { home = true; break }
                try await Task.sleep(for: .milliseconds(100))
            }
            XCTAssertTrue(home, "Account-authenticated WebRTC input returned the physical iPhone to Home")
            try await eventually("The phone's Home transition appeared in decoded WebRTC video") { transition.milliseconds != nil }
            print("Local phone Home input to first visible transition: \(Int(transition.milliseconds!)) ms")
            // Keep both peers and the exclusive control lease through several
            // accelerated credential rotations, then exercise control handoff.
            for _ in 0..<12 {
                try await Task.sleep(for: .seconds(1))
                XCTAssertTrue(viewer.connected && viewer.controlling && second.connected)
            }
            viewer.releaseControl()
            try await Task.sleep(for: .milliseconds(200))
            second.takeControl()
            try await eventually("Released control passed to the other viewer") { second.controlling }
            await host.stop()
            try await eventually("Stopping sharing disconnected the controller") { !second.connected && !second.controlling }
        } catch {
            print("First ICE pairs: \(await viewer.diagnosticICE()); second ICE pairs: \(await second.diagnosticICE())")
            XCTFail("Host: \(host.status) \(host.diagnosticStates); first viewer: \(viewer.status) \(viewer.diagnosticState); second viewer: \(second.status) \(second.diagnosticState)")
            viewer.close(); second.close(); await host.stop(); throw error
        }
        viewer.close(); second.close(); await host.stop()
    }

    private func phoneRequest(_ path: String, body: [String: Any]? = nil) async throws -> [String: Any] {
        var request = URLRequest(url: URL(string: "http://127.0.0.1:18100" + path)!, timeoutInterval: 5)
        if let body {
            request.httpMethod = "POST"; request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let (data, response) = try await URLSession.shared.data(for: request)
        XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200)
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    @MainActor private func eventually(_ description: String, _ predicate: () -> Bool) async throws {
        let deadline = ProcessInfo.processInfo.systemUptime + 20
        while ProcessInfo.processInfo.systemUptime < deadline {
            if predicate() { return }
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTFail(description)
        throw RemoteError.unavailable
    }
}
#endif
