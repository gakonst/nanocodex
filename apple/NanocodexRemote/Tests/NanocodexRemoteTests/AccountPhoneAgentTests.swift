#if os(macOS)
import XCTest
import ImageIO
@testable import NanocodexRemote

final class AccountPhoneAgentTests: XCTestCase {
    // A real model, account Worker, paired phone, and native viewer exercise the
    // shared input owner. All model output stays in an explicitly selected,
    // private evidence directory because it includes actual phone screenshots.
    @MainActor func testAgentPhoneControlAndHumanTakeover() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard let path = environment["NANOCODEX_TEST_REMOTE_ENV"],
              let runner = environment["NANOCODEX_TEST_PHONE_RUNNER"],
              let device = environment["NANOCODEX_TEST_PHONE_UDID"],
              let helper = environment["NANOCODEX_TEST_PHONE_HELPER"],
              let executable = environment["NANOCODEX_TEST_AGENT_CLI"],
              let agent = environment["NANOCODEX_TEST_PHONE_AGENT_ID"],
              let evidence = environment["NANOCODEX_TEST_AGENT_EVIDENCE"] else {
            throw XCTSkip("Requires an isolated local account, test agent, signed runner, and unlocked paired phone")
        }
        var values: [String: String] = [:]
        for line in try String(contentsOfFile: path, encoding: .utf8).split(separator: "\n") {
            guard let split = line.firstIndex(of: "=") else { continue }
            values[String(line[..<split])] = String(line[line.index(after: split)...])
        }
        let origin = try XCTUnwrap(URL(string: try XCTUnwrap(values["NANOCODEX_MANAGED_URL"])))
        guard ["127.0.0.1", "localhost"].contains(origin.host ?? ""), UUID(uuidString: agent) != nil,
              executable.hasPrefix("/"), evidence.hasPrefix("/") else { throw RemoteError.invalidMessage }
        let token = try XCTUnwrap(values["NANOCODEX_API_KEY"])
        let service = try RemoteService(origin: origin) { $0.setValue("Bearer " + token, forHTTPHeaderField: "Authorization") }
        defer { service.close() }
        try FileManager.default.createDirectory(atPath: evidence, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let host = RemoteMacHost(), viewer = RemoteViewer()
        let machine = "phone-agent-test-" + UUID().uuidString
        var bridge = PhoneBridgeConfiguration(deviceID: device, runner: URL(fileURLWithPath: runner))
        bridge.companionExecutable = URL(fileURLWithPath: helper)
        await host.startPhone(service: service, machineID: machine, name: "Paired phone agent test", bridge: bridge)
        do {
            try await eventually { host.sharing }
            let locked = try await phone("/wda/locked")
            guard locked["value"] as? Bool == false else {
                XCTFail("The paired phone must be unlocked for this journey"); throw RemoteError.unavailable
            }
            let hands = try await service.list()
            let hand = try XCTUnwrap(hands.first { $0.machineID == machine })
            await viewer.connect(service: service, hand: hand)
            try await eventually { viewer.connected && viewer.track != nil }
            _ = try await phone("/wda/apps/launchUnattached", body: ["bundleId": "com.apple.calculator"])
            let instructions = "Use tool_search to discover the live screen of the exact device \(machine), named Paired phone agent test. Use ONLY that screen tool through Code Mode. Observe with image(result); never print image base64. Do not use exec_command, browser, or another device. "
            let opened = try await runAgent(executable, agent: agent, values: values, evidence: evidence, phase: "menu", prompt: instructions
                + "Observe Calculator and open its Change Mode menu by tapping the visible button. If already open, dismiss it by tapping empty space and open it again. Do not select a mode or change the calculator value. Show the final screenshot, leave that menu open, and do not release control. Stop on any busy or unknown input outcome without retrying.")
            let menuResults = screenResults(opened, machine: machine)
            XCTAssertTrue(menuResults.contains { $0["status"] as? String == "completed" })
            try assertScreenshot(menuResults)
            let status = try await phone("/status"), session = try XCTUnwrap(status["sessionId"] as? String)
            let menu = try await phone("/session/\(session)/elements", body: ["using": "predicate string", "value": "label == 'Scientific'"])
            XCTAssertFalse((menu["value"] as? [Any] ?? []).isEmpty, "Agent touch must open Calculator's mode menu")

            viewer.takeControl()
            try await eventually { viewer.controlling }
            let blocked = try await runAgent(executable, agent: agent, values: values, evidence: evidence, phase: "busy", prompt: instructions
                + "A human now owns control. For the authorized ownership test, make exactly one key action with key 74 (Home). Catch and report its actual error; it should be busy. Do not retry or release control, and send no other input.")
            let blockedResults = screenResults(blocked, machine: machine)
            XCTAssertEqual(blockedResults.count, 1)
            XCTAssertEqual((blockedResults.first?["structured_result"] as? [String: Any])?["status"] as? String, "busy")
            XCTAssertTrue(viewer.controlling)
            let active = try await phone("/wda/activeAppInfo")
            XCTAssertEqual((active["value"] as? [String: Any])?["bundleId"] as? String, "com.apple.calculator")

            viewer.releaseControl()
            try await Task.sleep(for: .milliseconds(150))
            let resumed = try await runAgent(executable, agent: agent, values: values, evidence: evidence, phase: "home", prompt: instructions
                + "The human has released control. Send exactly one key action with key 74 to return to Home and display its screenshot. Report what you actually see, then release the agent control lease. Do not retry ambiguous input.")
            let resumedResults = screenResults(resumed, machine: machine)
            try assertScreenshot(resumedResults)
            let home = try await phone("/wda/activeAppInfo")
            XCTAssertEqual((home["value"] as? [String: Any])?["bundleId"] as? String, "com.apple.springboard")
            await host.stop()
            try await eventually { !viewer.connected }
            viewer.close()
        } catch {
            XCTFail("Phone host: \(host.status); viewer: \(viewer.status)")
            viewer.close(); await host.stop(); throw error
        }
    }

    private func screenResults(_ events: [[String: Any]], machine: String) -> [[String: Any]] {
        events.compactMap { event in
            guard event["type"] as? String == "tool.result", let payload = event["payload"] as? [String: Any],
                  (payload["metadata"] as? [String: Any])?["machine_id"] as? String == machine,
                  (payload["tool"] as? String)?.hasPrefix("screen_") == true else { return nil }
            return payload
        }
    }

    private func assertScreenshot(_ results: [[String: Any]]) throws {
        let value = try XCTUnwrap(results.compactMap { $0["structured_result"] as? [String: Any] }
            .first { $0["status"] as? String == "ok" && $0["image_url"] is String })
        let url = try XCTUnwrap(value["image_url"] as? String)
        XCTAssertTrue(url.hasPrefix("data:image/jpeg;base64,"))
        let bytes = try XCTUnwrap(Data(base64Encoded: String(url.dropFirst("data:image/jpeg;base64,".count))))
        let source = try XCTUnwrap(CGImageSourceCreateWithData(bytes as CFData, nil))
        let image = try XCTUnwrap(CGImageSourceCreateImageAtIndex(source, 0, nil))
        XCTAssertEqual(image.width, value["width"] as? Int)
        XCTAssertEqual(image.height, value["height"] as? Int)
        XCTAssertGreaterThan(image.height, image.width)
    }

    @MainActor private func runAgent(_ executable: String, agent: String, values: [String: String], evidence: String,
                                    phase: String, prompt: String) async throws -> [[String: Any]] {
        let id = "phone-agent-" + phase + "-" + UUID().uuidString
        let path = URL(fileURLWithPath: evidence).appendingPathComponent(id + ".jsonl").path
        guard FileManager.default.createFile(atPath: path, contents: nil, attributes: [.posixPermissions: 0o600]) else { throw RemoteError.unavailable }
        let output = try FileHandle(forWritingTo: URL(fileURLWithPath: path)); defer { try? output.close() }
        let process = Process(); process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = ["run", "--agent", agent, "--idempotency-key", id, prompt]
        process.environment = ProcessInfo.processInfo.environment.merging(values) { _, value in value }
        process.standardInput = FileHandle.nullDevice; process.standardOutput = output; process.standardError = output
        try process.run()
        let deadline = ProcessInfo.processInfo.systemUptime + 180
        while process.isRunning, ProcessInfo.processInfo.systemUptime < deadline { try await Task.sleep(for: .milliseconds(100)) }
        if process.isRunning { process.terminate(); throw RemoteError.unavailable }
        XCTAssertEqual(process.terminationStatus, 0, "Agent evidence: \(path)")
        print("Phone agent \(phase) evidence: \(path)")
        return try String(contentsOfFile: path, encoding: .utf8).split(separator: "\n").compactMap {
            (try? JSONSerialization.jsonObject(with: Data($0.utf8))) as? [String: Any]
        }
    }

    private func phone(_ path: String, body: [String: Any]? = nil) async throws -> [String: Any] {
        var request = URLRequest(url: URL(string: "http://127.0.0.1:18100" + path)!, timeoutInterval: 5)
        if let body {
            request.httpMethod = "POST"; request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let (data, response) = try await URLSession.shared.data(for: request)
        guard (response as? HTTPURLResponse)?.statusCode == 200 else { throw RemoteError.unavailable }
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    @MainActor private func eventually(_ predicate: () -> Bool) async throws {
        let deadline = ProcessInfo.processInfo.systemUptime + 20
        while ProcessInfo.processInfo.systemUptime < deadline {
            if predicate() { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        throw RemoteError.unavailable
    }
}
#endif
