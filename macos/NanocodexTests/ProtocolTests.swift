import XCTest
@testable import Nanocodex

final class ProtocolTests: XCTestCase {
    @MainActor
    func testNativePhoneAndCodeScreensRenderWithoutNetwork() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-protocol")
        model.isStarting = false
        model.runtime.requestOverride = { method, _ in
            guard method == "startSignIn" else { throw RuntimeFailure(message: "Unexpected network request") }
            let now = Date().timeIntervalSince1970 * 1000
            return .object(["phone": .string("+15555550100"), "resendAt": .number(now + 30_000), "expiresAt": .number(now + 300_000)])
        }
        let content = NSHostingView(rootView: ContentView().environmentObject(model).frame(width: 1200, height: 840))
        content.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1200, height: 840), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = content; window.setContentSize(NSSize(width: 1200, height: 840)); window.orderFront(nil)
        defer { window.close() }
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
        let evidence = root.appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        func fields(_ view: NSView) -> [NSTextField] { (view as? NSTextField).map { [$0] } ?? view.subviews.flatMap(fields) }
        func capture(_ name: String) throws {
            content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
            let rep = try XCTUnwrap(content.bitmapImageRepForCachingDisplay(in: content.bounds))
            content.cacheDisplay(in: content.bounds, to: rep)
            try XCTUnwrap(rep.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent(name))
        }
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertTrue(fields(content).contains { $0.placeholderString == "+1 415 555 0123" || $0.placeholderString == "Phone number" })
        try capture("native-sign-in-phone.png")
        _ = try await model.startPhoneSignIn(phone: "+15555550100", baseUrl: "https://example.invalid")
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertTrue(fields(content).contains { $0.placeholderString == "6-digit code" })
        try capture("native-sign-in-code.png")
    }
    func testSignInChallengeUsesMillisecondsAndAcceptsPastedCode() throws {
        let challenge = try JSONValue.object([
            "phone": .string("+15555550100"), "resendAt": .number(1_780_000_030_000), "expiresAt": .number(1_780_000_300_000)
        ]).decode(SignInChallenge.self)
        let now = Date(timeIntervalSince1970: 1_780_000_000)
        XCTAssertEqual(challenge.resendSeconds(at: now), 30)
        XCTAssertEqual(challenge.resendSeconds(at: now.addingTimeInterval(31)), 0)
        XCTAssertFalse(challenge.isExpired(at: now.addingTimeInterval(299)))
        XCTAssertTrue(challenge.isExpired(at: now.addingTimeInterval(300)))
        XCTAssertEqual(SignInChallenge.normalizedCode("123 456\n"), "123456")
        XCTAssertEqual(SignInChallenge.normalizedCode("１２３abc1234567"), "123456")
    }
    func testAcceptedTurnLocksModelBeforeCompletionAndDecodesOlderSnapshots() throws {
        var value: [String: JSONValue] = [
            "id": .string("thread"), "events": .array([]), "hasMore": .bool(false), "connected": .bool(true),
            "activeTurns": .array([]), "settings": try .encoded(AgentSettings())
        ]
        XCTAssertFalse(try JSONValue.object(value).decode(ThreadSnapshot.self).hasAcceptedTurn)
        value["acceptedTurns"] = .number(1)
        XCTAssertTrue(try JSONValue.object(value).decode(ThreadSnapshot.self).hasAcceptedTurn)
        value.removeValue(forKey: "acceptedTurns")
        value["activeTurns"] = .array([.string("first-turn")])
        XCTAssertTrue(try JSONValue.object(value).decode(ThreadSnapshot.self).hasAcceptedTurn)
    }
    @MainActor
    func testPhoneOnboardingWaitsForCommitAndRetriesWithoutConsumingCodeAgain() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-protocol")
        model.isStarting = false
        var calls: [String] = []
        var completionAttempts = 0
        model.runtime.requestOverride = { method, _ in
            calls.append(method)
            switch method {
            case "startSignIn":
                return .object(["phone": .string("+15555550100"), "resendAt": .number(30_000), "expiresAt": .number(300_000)])
            case "verifySignIn":
                return .object(["baseUrl": .string("https://example.invalid"), "apiKey": .string("isolated-private-credential")])
            case "connect":
                XCTAssertTrue(model.showsOnboarding)
                return Self.connectedState
            case "completeSignIn":
                XCTAssertTrue(model.state.connected)
                XCTAssertTrue(model.showsOnboarding)
                completionAttempts += 1
                if completionAttempts == 1 { throw RuntimeFailure(message: "Temporary connection interruption") }
                return .null
            default: throw RuntimeFailure(message: "Unexpected protocol method: \(method)")
            }
        }
        _ = try await model.startPhoneSignIn(phone: "+15555550100", baseUrl: "https://example.invalid")
        do { try await model.finishPhoneSignIn(code: "123456"); XCTFail("Expected the interrupted completion") }
        catch { XCTAssertTrue(model.phoneSignInActive); XCTAssertTrue(model.showsOnboarding) }
        try await model.finishPhoneSignIn(code: "123456")
        XCTAssertFalse(model.showsOnboarding)
        XCTAssertFalse(model.phoneSignInActive)
        XCTAssertEqual(calls, ["startSignIn", "verifySignIn", "connect", "completeSignIn", "completeSignIn"])
        try await model.cancelPhoneSignIn()
        XCTAssertEqual(calls.count, 5)
    }
    @MainActor
    func testCancellingPhoneSwitchPreservesExistingAccountAndTabs() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-protocol")
        model.state = try Self.connectedState.decode(DesktopState.self)
        model.tabs = [WorkspaceTab(title: "Existing work", draft: "Keep this draft")]
        let tabs = model.tabs
        var calls: [String] = []
        model.runtime.requestOverride = { method, _ in
            calls.append(method)
            if method == "startSignIn" { return .object(["phone": .string("+15555550100"), "resendAt": .number(30_000), "expiresAt": .number(300_000)]) }
            guard method == "cancelSignIn" else { throw RuntimeFailure(message: "Existing account was changed") }
            return .null
        }
        _ = try await model.startPhoneSignIn(phone: "+15555550100", baseUrl: "https://example.invalid")
        XCTAssertFalse(model.showsOnboarding)
        try await model.cancelPhoneSignIn()
        XCTAssertTrue(model.state.connected)
        XCTAssertEqual(model.tabs, tabs)
        XCTAssertEqual(calls, ["startSignIn", "cancelSignIn"])
    }
    private static var connectedState: JSONValue { .object([
        "connected": .bool(true), "baseUrl": .string("https://example.invalid"), "threads": .array([]), "hands": .array([]),
        "defaults": .object([:]), "platform": .string("darwin"), "version": .string("0.1.0")
    ]) }
    func testAstraSelectionNormalizesUnsupportedSettings() throws {
        var settings = AgentSettings(model: "gpt-5.6-sol", thinking: "none", reasoning_mode: "pro", fast_mode: true)
        settings.selectModel("gpt-6-astra")

        XCTAssertEqual(settings.modelName, "Astra")
        XCTAssertEqual(settings.thinking, "high")
        XCTAssertEqual(settings.reasoning_mode, "standard")
        XCTAssertFalse(settings.supportsNoReasoning)
        XCTAssertFalse(settings.supportsProReasoning)
        XCTAssertEqual(try JSONValue.encoded(settings), .object([
            "model": .string("gpt-6-astra"), "thinking": .string("high"),
            "reasoning_mode": .string("standard"), "fast_mode": .bool(true),
        ]))
    }
    func testAstraRetainsSupportedEffortsAndExistingDefaults() throws {
        XCTAssertEqual(AgentSettings(), AgentSettings(model: "gpt-5.6-sol", thinking: "high", reasoning_mode: "standard", fast_mode: false))
        for effort in ["low", "medium", "high", "xhigh", "max"] {
            var settings = AgentSettings(model: "gpt-5.6-terra", thinking: effort, reasoning_mode: "pro", fast_mode: false)
            settings.selectModel("gpt-6-astra")
            XCTAssertEqual(settings.thinking, effort)
            XCTAssertEqual(settings.reasoning_mode, "standard")
            XCTAssertFalse(settings.fast_mode)
            let retained = try JSONValue.encoded(settings).decode(AgentSettings.self)
            XCTAssertEqual(retained, settings)
        }
        var existing = AgentSettings(model: "gpt-5.6-sol", thinking: "none", reasoning_mode: "pro", fast_mode: true)
        existing.selectModel("gpt-5.6-sol")
        XCTAssertEqual(existing.thinking, "none")
        XCTAssertEqual(existing.reasoning_mode, "pro")
        XCTAssertTrue(existing.supportsNoReasoning)
        XCTAssertTrue(existing.supportsProReasoning)
    }
    func testDurableReplayDoesNotDuplicateOutput() throws {
        let events: [ManagedEvent] = [
            .init(cursor: "1", turnId: "turn", data: .object(["type": .string("turn_accepted"), "id": .string("turn"), "input": .string("hello")])),
            .init(cursor: "2", turnId: "turn", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string("Hello")])])])),
            .init(cursor: "3", turnId: "turn", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.message"), "payload": .object(["text": .string("Hello there")])])])),
            .init(cursor: "4", turnId: "turn", data: .object(["type": .string("turn_completed"), "id": .string("turn"), "final_message": .string("Hello there")]))
        ]
        let projected = projectTimeline(events + events)
        XCTAssertEqual(projected.count, 2)
        XCTAssertEqual(projected.last?.text, "Hello there")
        XCTAssertFalse(projected.last?.streaming ?? true)
    }
    func testToolResultUpdatesCallAndPreservesExactOutput() {
        let events: [ManagedEvent] = [
            .init(cursor: "90071992547409930", turnId: "turn", data: .object(["type": .string("event"), "event": .object(["type": .string("tool.call"), "payload": .object(["call_id": .string("c"), "tool": .string("exec_command"), "arguments": .object(["cmd": .string("pwd")])])])])),
            .init(cursor: "90071992547409931", turnId: "turn", data: .object(["type": .string("event"), "event": .object(["type": .string("tool.result"), "payload": .object(["call_id": .string("c"), "status": .string("completed"), "result": .string("/workspace")])])]))
        ]
        let projected = projectTimeline(events)
        XCTAssertEqual(projected.count, 1)
        XCTAssertEqual(projected.first?.status, "completed")
        XCTAssertEqual(projected.first?.output, "/workspace")
    }
    func testSubagentStreamsRemainSeparate() {
        func delta(_ cursor: String, _ agent: String, _ text: String) -> ManagedEvent {
            .init(cursor: cursor, turnId: "turn", data: .object(["type": .string("event"), "agent_id": .string(agent), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string(text)])])]))
        }
        let result = projectTimeline([delta("1", "researcher", "Research"), delta("2", "coder", "Code"), delta("3", "coder", " change")])
        XCTAssertEqual(result.map(\.text), ["Research", "Code change"])
        XCTAssertNotEqual(result[0].agent, result[1].agent)
    }
    func testSharedStateAndLayoutContractDecodes() throws {
        let fixture = #"{"connected":true,"hasCredentials":true,"baseUrl":"https://example.test","threads":[],"hands":[{"id":"mac-1","name":"This Mac","kind":"local","workspace":"/tmp/work","status":"connected","calls":0,"activeCalls":0,"logs":[]}],"defaults":{"workspace":"/tmp/default"},"platform":"darwin","version":"0.1.0","layout":{"tabs":[{"id":"tab-1","draft":"unfinished","target":"mac-1","folder":""}],"activeTabId":"tab-1","tabPosition":"top","theme":"system"}}"#
        let state = try JSONDecoder().decode(DesktopState.self, from: Data(fixture.utf8))
        XCTAssertEqual(state.hands.first?.status, "connected")
        XCTAssertEqual(state.layout?.tabs.first?.draft, "unfinished")
        XCTAssertEqual(state.layout?.tabPosition, "top")
        XCTAssertEqual(state.defaults["workspace"].string, "/tmp/default")
    }
}

import AppKit
import SwiftUI

/// Runs the native AppKit editor and rendered SwiftUI window against the real
/// managed service. This does not require macOS's separate UI automation mode.
final class NativeServiceTests: XCTestCase {
    @MainActor
    func testNativeWindowHandAndTwoDurableTurns() async throws {
        let repository = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let envURL = repository.appendingPathComponent(".env")
        guard FileManager.default.fileExists(atPath: envURL.path) else { throw XCTSkip("A development .env account is required for the real managed-service journey.") }
        let env = try String(contentsOf: envURL, encoding: .utf8)
        guard let line = env.components(separatedBy: .newlines).first(where: { $0.hasPrefix("NC_API_KEY=") }), let key = line.split(separator: "=", maxSplits: 1).last.map(String.init) else { throw XCTSkip("NC_API_KEY is required for live native evidence.") }
        let apiKey = key.trimmingCharacters(in: CharacterSet.whitespacesAndNewlines.union(CharacterSet(charactersIn: "\"'")))
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("nanocodex-native-service-\(UUID().uuidString)")
        let workspace = directory.appendingPathComponent("workspace")
        let statePath = directory.appendingPathComponent("state")
        try FileManager.default.createDirectory(at: workspace, withIntermediateDirectories: true)
        let evidence = repository.appendingPathComponent("macos/build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        var createdID: String?
        var model = AppModel(runtimeDirectory: statePath.path)
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1200, height: 840), styleMask: [.titled, .closable, .resizable, .miniaturizable], backing: .buffered, defer: false)
        window.title = "Nanocodex"; window.center()
        window.setContentSize(NSSize(width: 1200, height: 840))
        var host = NSHostingView(rootView: ContentView().environmentObject(model).frame(width: 1200, height: 840))
        host.sizingOptions = []
        window.contentView = host; window.makeKeyAndOrderFront(nil)
        window.setContentSize(NSSize(width: 1200, height: 840))
        let start = Date()
        do {
            await model.start()
            print("Native journey: connected")
            XCTAssertTrue(model.state.connected, model.error ?? model.state.error ?? "Account did not connect")
            let connectedAt = Date().timeIntervalSince(start)
            try await Task.sleep(for: .milliseconds(250))
            try capture(host, to: evidence.appendingPathComponent("native-new-thread.png"))
            let editor = try XCTUnwrap(findEditor(host))
            window.makeFirstResponder(editor)
            editor.insertText("A native draft", replacementRange: editor.selectedRange())
            XCTAssertEqual(model.activeTab?.draft, "A native draft")
            let newline = try XCTUnwrap(NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: .shift, timestamp: 0, windowNumber: window.windowNumber, context: nil, characters: "\r", charactersIgnoringModifiers: "\r", isARepeat: false, keyCode: 36))
            editor.keyDown(with: newline)
            XCTAssertTrue(model.activeTab?.draft.contains("\n") == true, "Shift Return must insert a newline in the native editor")
            let firstTab = model.activeTabID
            model.newTab(); try await Task.sleep(for: .milliseconds(80))
            XCTAssertEqual(editor.string, "")
            editor.insertText("Second native draft", replacementRange: editor.selectedRange())
            model.closeTab(model.activeTabID); try await Task.sleep(for: .milliseconds(80))
            XCTAssertEqual(model.activeTabID, firstTab)
            XCTAssertTrue(editor.string.hasPrefix("A native draft"))
            model.reopenTab(); try await Task.sleep(for: .milliseconds(80))
            XCTAssertEqual(editor.string, "Second native draft")
            model.updateDraft("")
            model.updateTab { $0.folder = workspace.path }
            model.tabPosition = "top"; model.persistLayout()
            try await Task.sleep(for: .milliseconds(100))
            let prompt = "Native Nanocodex integration test. Write exactly native-hand-roundtrip-ok to native-evidence.txt in the selected Hand workspace using exec_command, then read the file using exec_command. Reply NATIVE_HAND_READY when both operations succeeded."
            editor.insertText(prompt, replacementRange: editor.selectedRange())
            let enter = try XCTUnwrap(NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber, context: nil, characters: "\r", charactersIgnoringModifiers: "\r", isARepeat: false, keyCode: 36))
            editor.keyDown(with: enter)
            try await waitUntil(timeout: 25) { model.activeTab?.threadId != nil }
            createdID = model.activeTab?.threadId
            try await waitUntil(timeout: 110) { model.activeMessages.contains { $0.kind == .assistant && $0.text.contains("NATIVE_HAND_READY") } && !model.isRunning }
            XCTAssertEqual(try String(contentsOf: workspace.appendingPathComponent("native-evidence.txt"), encoding: .utf8).trimmingCharacters(in: .newlines), "native-hand-roundtrip-ok")
            XCTAssertEqual(model.state.hands.count, 1)
            XCTAssertEqual(model.state.hands.first?.agentId, createdID, "Selected folders must create a grant scoped to the thread")
            XCTAssertEqual(model.state.hands.first?.status, "connected")
            XCTAssertNil(model.error)
            print("Native journey: first real file roundtrip completed")
            let firstTurnAt = Date().timeIntervalSince(start)
            try capture(host, to: evidence.appendingPathComponent("native-first-turn.png"))
            model.screen = .hands; try await Task.sleep(for: .milliseconds(150))
            try capture(host, to: evidence.appendingPathComponent("native-connected-hand.png"))
            model.screen = .chat
            await model.prepareToQuit()
            print("Native journey: previous helper closed")
            try await Task.sleep(for: .milliseconds(500))
            model = AppModel(runtimeDirectory: statePath.path)
            host = NSHostingView(rootView: ContentView().environmentObject(model).frame(width: 1200, height: 840))
            host.sizingOptions = []
            window.contentView = host
            window.setContentSize(NSSize(width: 1200, height: 840))
            await model.start()
            print("Native journey: restarted helper connected")
            XCTAssertEqual(model.tabPosition, "top")
            XCTAssertEqual(model.activeTab?.threadId, createdID)
            try await waitUntil(timeout: 20) { model.activeMessages.contains { $0.kind == .assistant && $0.text.contains("NATIVE_HAND_READY") } }
            XCTAssertEqual(model.state.hands.first?.status, "stopped", "Compute must not silently restart after reopening the app")
            let restartedHand = try XCTUnwrap(model.state.hands.first)
            await model.startHand(restartedHand.id)
            print("Native journey: Hand reconnected")
            XCTAssertEqual(model.state.hands.first?.status, "connected")
            await model.send("Read native-evidence.txt again from the same selected Hand. Reply with its contents followed by NATIVE_SECOND_TURN_READY.")
            try await waitUntil(timeout: 60) { model.activeMessages.filter { $0.kind == .user }.count >= 2 && !model.isRunning && model.pending[model.activeTabID] == nil }
            print("Native journey: second turn completed")
            guard model.activeMessages.contains(where: { $0.kind == .assistant && $0.text.contains("native-hand-roundtrip-ok") && $0.text.contains("NATIVE_SECOND_TURN_READY") }) else { throw RuntimeFailure(message: "The reconnected Hand did not complete the second file read: " + (model.activeMessages.last(where: { $0.kind == .assistant })?.text ?? "No assistant reply")) }
            try await Task.sleep(for: .milliseconds(150))
            try capture(host, to: evidence.appendingPathComponent("native-durable-thread.png"))
            await model.stopHand(restartedHand.id)
            XCTAssertEqual(model.state.hands.first?.status, "stopped")
            let report: [String: Any] = ["connectedSeconds": connectedAt, "firstTurnSeconds": firstTurnAt, "totalSeconds": Date().timeIntervalSince(start), "agentId": createdID ?? "", "nativeEditor": true, "shiftReturn": true, "returnSubmit": true, "tabs": true, "persistedTopTabs": true, "folderHand": true, "realFileRoundtrip": true, "twoDurableTurns": true, "stoppedHand": true]
            try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]).write(to: evidence.appendingPathComponent("native-journey.json"))
            await model.prepareToQuit()
            window.orderOut(nil)
            if let createdID { try await removeTestThread(createdID, origin: model.state.baseUrl, key: apiKey) }
            try FileManager.default.removeItem(at: directory)
        } catch {
            try? capture(host, to: evidence.appendingPathComponent("native-failure.png"))
            try? JSONEncoder().encode(model.activeSnapshot?.events ?? []).write(to: evidence.appendingPathComponent("native-failure-events.json"))
            try? JSONEncoder().encode(model.state.hands).write(to: evidence.appendingPathComponent("native-failure-hands.json"))
            let id = createdID ?? model.activeTab?.threadId
            await model.prepareToQuit(); window.orderOut(nil)
            if let id { try? await removeTestThread(id, origin: model.state.baseUrl, key: apiKey) }
            throw error
        }
    }
    @MainActor
    private func waitUntil(timeout: TimeInterval, condition: () -> Bool) async throws {
        let end = Date().addingTimeInterval(timeout)
        while !condition() {
            guard Date() < end else { throw RuntimeFailure(message: "The live native journey did not reach its expected state within \(Int(timeout)) seconds.") }
            try await Task.sleep(for: .milliseconds(100))
        }
    }
    @MainActor
    private func findEditor(_ view: NSView) -> ComposerTextView? {
        if let editor = view as? ComposerTextView { return editor }
        return view.subviews.lazy.compactMap { self.findEditor($0) }.first
    }
    @MainActor
    private func capture(_ view: NSView, to url: URL) throws {
        view.layoutSubtreeIfNeeded(); view.displayIfNeeded()
        let bitmap = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: bitmap)
        try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: url)
        let attachment = XCTAttachment(contentsOfFile: url); attachment.lifetime = .keepAlways; add(attachment)
    }
    private func removeTestThread(_ id: String, origin: String, key: String) async throws {
        var request = URLRequest(url: try XCTUnwrap(URL(string: origin + "/v1/agents/" + id)))
        request.httpMethod = "DELETE"; request.setValue("Bearer " + key, forHTTPHeaderField: "Authorization")
        let (_, response) = try await URLSession.shared.data(for: request)
        XCTAssertTrue([200, 204, 404].contains((response as? HTTPURLResponse)?.statusCode ?? 0), "Remove only the thread created by this test")
    }
}

private final class EvidenceWindow: NSWindow {
    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect { frameRect }
}
