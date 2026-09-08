import AppKit
import CoreAudio
import InboxCore
import NanocodexVoice
import SwiftUI
import XCTest
@testable import Nanocodex

final class VoiceTests: XCTestCase {
    @MainActor
    func testVoiceUsesRequestedPaneAndProjectsSavedConversation() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-voice-contract")
        defer { model.shutdown() }
        model.runtime.requestOverride = { method, _ in
            if method == "connect" { return Self.connectedState }
            if method == "createThread" { return try .encoded(AgentThread(id: "created", title: "Voice", updatedAt: 0, turnCount: 0)) }
            if method == "openThread" { return Self.emptyThread("created") }
            return .null
        }
        try await model.connect(baseUrl: "https://service.invalid", key: "fixture-only", remember: false)
        model.tabs = [WorkspaceTab(id: "voice"), WorkspaceTab(id: "other", threadId: "other-agent", draft: "Keep this draft")]
        model.activeTabID = "other"
        _ = try await model.voiceConfiguration(tabID: "voice")
        XCTAssertEqual(model.tab("voice")?.threadId, "created")
        XCTAssertEqual(model.activeTabID, "other")
        XCTAssertEqual(model.tab("other")?.draft, "Keep this draft")
        model.messages["created"] = [.init(id: "saved", turnId: "turn", kind: .user, text: "<realtime_delegation><source>transcript_tail_flush</source><input>Internal handoff instruction</input><transcript_delta>user: Hello\nassistant: Hi there.</transcript_delta></realtime_delegation>")]
        XCTAssertEqual(model.displayedTranscript("voice").map(\.text), ["Hello", "Hi there."])
        XCTAssertEqual(model.displayedTranscript("voice").map(\.kind), [.user, .assistant])
        model.voice.startTranscriptPreview(agentID: "created")
        model.closeTab("other")
        XCTAssertTrue(model.voice.isEngaged)
        model.closeTab("voice")
        XCTAssertFalse(model.voice.isEngaged)
    }

    /// Actual native microphone/WebRTC/data-channel journey. Synthetic speech
    /// enters through the installed BlackHole device, never the physical mic.
    @MainActor
    func testNativeSpeechInterruptFollowUpAndStop() async throws {
        let env = ProcessInfo.processInfo.environment
        guard env["NANOCODEX_DESKTOP_VOICE_LIVE"] == "1" else { throw XCTSkip("Opt-in native voice service evidence") }
        let credential = try XCTUnwrap(env["NANOCODEX_DESKTOP_VOICE_KEYCHAIN"] == "1"
            ? AccountKeychain.read() : AccountKeychain.environmentCredential())
        let input = try Self.defaultInput()
        let loopback = try Self.audioDevice(named: "BlackHole 2ch")
        try Self.setDefaultInput(loopback)
        defer { try? Self.setDefaultInput(input) }
        let client = ManagedClient(credential: try AccountCredential(origin: credential.baseUrl, apiKey: credential.apiKey))
        defer { client.close() }
        let agentID = try await client.create(requestID: UUID().uuidString)
        print("Native speech validation agent: \(agentID)")
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-native-speech-\(agentID)")
        defer { model.shutdown() }
        model.runtime.requestOverride = { method, _ in
            if method == "connect", case .object(var state) = Self.connectedState {
                state["baseUrl"] = .string(credential.baseUrl); return .object(state)
            }
            if method == "openThread" { return Self.emptyThread(agentID) }
            return .null
        }
        try await model.connect(baseUrl: credential.baseUrl, key: credential.apiKey, remember: false)
        model.tabs = [WorkspaceTab(id: "voice", threadId: agentID, title: "Native voice validation")]
        model.select("voice"); model.isStarting = false
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        let host = NSHostingView(rootView: ContentView().environmentObject(model).frame(width: 1100, height: 800))
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1100, height: 800), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = host; window.makeKeyAndOrderFront(nil)
        defer { window.close() }
        var player: Process?
        defer { if player?.isRunning == true { player?.terminate() } }
        func play(_ path: String) throws {
            let process = Process(); process.executableURL = URL(fileURLWithPath: env["NANOCODEX_VOICE_SOX"] ?? "/opt/homebrew/bin/sox")
            process.arguments = ["-q", path, "-t", "coreaudio", "BlackHole 2ch"]
            process.standardOutput = FileHandle.nullDevice; process.standardError = FileHandle.nullDevice
            try process.run(); player = process
        }
        func wait(_ condition: () -> Bool, seconds: Double = 25) async throws {
            let deadline = Date().addingTimeInterval(seconds)
            while !condition(), Date() < deadline, model.voice.phase != .failed { try await Task.sleep(for: .milliseconds(40)) }
            guard condition() else { throw RuntimeFailure(message: model.voice.errorMessage ?? "Native speech evidence timed out") }
        }
        func deleteAgent() async throws {
            for attempt in 0..<5 {
                do { _ = try await client.json(path: "/v1/agents/\(agentID)", method: "DELETE"); return }
                catch APIError.http(503) where attempt < 4 { try await Task.sleep(for: .seconds(2)) }
            }
        }
        let began = Date()
        var milestones: [[String: Any]] = []
        func mark(_ stage: String) {
            let now = Date()
            milestones.append(["stage": stage, "utc": now.ISO8601Format(), "elapsed_ms": now.timeIntervalSince(began) * 1000])
            print("NATIVE_VOICE \(now.timeIntervalSince1970) \(stage)")
        }
        func saveFailure(_ error: Error) {
            let report: [String: Any] = ["status": "failed", "error": error.localizedDescription,
                "milestones": milestones, "phase": String(describing: model.voice.phase),
                "input_level": model.voice.inputLevel, "output_level": model.voice.outputLevel,
                "sent_bytes": model.voice.audioBytesSent, "received_bytes": model.voice.audioBytesReceived,
                "transcript_speakers": model.voice.transcripts.map(\.speaker)]
            try? JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
                .write(to: evidence.appendingPathComponent("native-voice-live-failure.json"))
        }
        let observer = Task { @MainActor in
            var seen: Set<String> = []
            while !Task.isCancelled {
                for row in model.voice.transcripts where !row.text.isEmpty {
                    let stage = "first_\(row.speaker)_transcript"
                    if seen.insert(stage).inserted { mark(stage) }
                }
                do { try await Task.sleep(for: .milliseconds(40)) } catch { return }
            }
        }
        defer { observer.cancel() }
        do {
            mark("call_start")
            model.voice.start { try await model.voiceConfiguration(tabID: "voice") }
            try await wait({ model.voice.phase == .active }, seconds: 50)
            mark("call_ready")
            let startupMS = Date().timeIntervalSince(began) * 1000
            mark("count_fixture_start")
            try play("/tmp/nanocodex-voice-count.wav")
            try await wait({ model.voice.outputLevel > 0.015 })
            mark("count_output")
            try await Task.sleep(for: .milliseconds(1000))
            try await wait({ model.voice.outputLevel > 0.015 })
            mark("interrupt_fixture_start")
            let interruptedAt = Date()
            try play("/tmp/nanocodex-native-voice-interrupt.wav")
            try await wait({ model.voice.outputLevel < 0.004 }, seconds: 8)
            mark("interrupt_output_quiet")
            let quietMS = Date().timeIntervalSince(interruptedAt) * 1000
            try await wait({ model.voice.transcripts.contains { $0.speaker == "assistant" && ($0.text.lowercased().contains("thirteen") || $0.text.contains("13")) } })
            mark("interrupt_answer")
            try await wait({ model.voice.outputLevel < 0.004 && player?.isRunning != true })
            mark("followup_fixture_start")
            try play("/tmp/nanocodex-native-voice-followup.wav")
            try await wait({ model.voice.transcripts.contains { $0.speaker == "assistant" && $0.text.lowercased().contains("blue") } })
            mark("followup_answer")
            XCTAssertGreaterThan(model.voice.audioBytesSent, 0); XCTAssertGreaterThan(model.voice.audioBytesReceived, 0)
            XCTAssertFalse(model.voice.transcripts.contains { $0.text.contains("<realtime_") || $0.text.contains("<source>") })
            host.layoutSubtreeIfNeeded(); host.displayIfNeeded()
            let image = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds)); host.cacheDisplay(in: host.bounds, to: image)
            try XCTUnwrap(image.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent("native-voice-live.png"))
            let rows = model.voice.transcripts.map { ["speaker": $0.speaker, "text": $0.text] }
            try await wait({ player?.isRunning != true && model.voice.outputLevel < 0.004 })
            try play("/tmp/nanocodex-voice-count.wav")
            try await wait({ model.voice.outputLevel > 0.015 })
            let stoppedAt = Date(); model.voice.stop()
            let stopMS = Date().timeIntervalSince(stoppedAt) * 1000
            XCTAssertFalse(model.voice.isEngaged); XCTAssertEqual(model.voice.inputLevel, 0); XCTAssertEqual(model.voice.outputLevel, 0)
            await model.voice.finishStopping()
            if player?.isRunning == true { player?.terminate() }
            let restartedAt = Date()
            model.voice.start { try await model.voiceConfiguration(tabID: "voice") }
            try await wait({ model.voice.phase == .active }, seconds: 30)
            let restartMS = Date().timeIntervalSince(restartedAt) * 1000
            model.voice.stop(); await model.voice.finishStopping()
            try JSONSerialization.data(withJSONObject: ["milestones": milestones, "startup_ms": startupMS, "restart_ms": restartMS, "first_quiet_after_interrupt_ms": quietMS, "stop_ms": stopMS, "transcripts": rows], options: [.prettyPrinted, .sortedKeys]).write(to: evidence.appendingPathComponent("native-voice-live.json"))
            print("Native speech PASS: startup \(Int(startupMS)) ms, restart \(Int(restartMS)) ms, quiet after interrupt \(Int(quietMS)) ms, stop \(Int(stopMS)) ms")
        } catch {
            mark("failed")
            saveFailure(error)
            model.voice.stop(); await model.voice.finishStopping()
            try? await deleteAgent(); throw error
        }
        try await deleteAgent()
    }

    /// Self-contained speech should answer directly; unknown personal facts must
    /// go through the managed agent and remain explicitly unknown when absent.
    @MainActor
    func testNativeGreetingAndPersonalMemory() async throws {
        let env = ProcessInfo.processInfo.environment
        guard env["NANOCODEX_DESKTOP_MEMORY_VOICE_LIVE"] == "1" else { throw XCTSkip("Opt-in native memory speech evidence") }
        let credential = try XCTUnwrap(env["NANOCODEX_DESKTOP_VOICE_KEYCHAIN"] == "1"
            ? AccountKeychain.read() : AccountKeychain.environmentCredential())
        let timingPath = try XCTUnwrap(env["NANOCODEX_VOICE_TIMING_LOG"])
        let input = try Self.defaultInput()
        try Self.setDefaultInput(Self.audioDevice(named: "BlackHole 2ch"))
        defer { try? Self.setDefaultInput(input) }
        let client = ManagedClient(credential: try AccountCredential(origin: credential.baseUrl, apiKey: credential.apiKey))
        defer { client.close() }
        let agentID = try await client.create(requestID: UUID().uuidString)
        print("Native memory speech agent: \(agentID)")
        let voice = VoiceSession()
        defer { voice.stop() }
        var player: Process?
        defer { if player?.isRunning == true { player?.terminate() } }
        func play(_ path: String) throws {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: env["NANOCODEX_VOICE_SOX"] ?? "/opt/homebrew/bin/sox")
            process.arguments = ["-q", path, "-t", "coreaudio", "BlackHole 2ch"]
            process.standardOutput = FileHandle.nullDevice; process.standardError = FileHandle.nullDevice
            try process.run(); player = process
        }
        func wait(_ condition: () -> Bool, seconds: Double = 30) async throws {
            let deadline = Date().addingTimeInterval(seconds)
            while !condition(), Date() < deadline, voice.phase != .failed { try await Task.sleep(for: .milliseconds(40)) }
            guard condition() else { throw RuntimeFailure(message: voice.errorMessage ?? "Native memory speech timed out") }
        }
        let began = Date()
        var milestones: [[String: Any]] = []
        func mark(_ stage: String) {
            let now = Date()
            milestones.append(["stage": stage, "utc": now.ISO8601Format(), "elapsed_ms": now.timeIntervalSince(began) * 1000])
            print("NATIVE_MEMORY_VOICE \(now.timeIntervalSince1970) \(stage)")
        }
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        func save(_ status: String, error: String = "") throws {
            let settings = voice.settings
            let report: [String: Any] = ["status": status, "error": error, "milestones": milestones,
                "voice": settings.voice, "pace": settings.pace.rawValue, "updates": settings.updates.rawValue,
                "handoff_mode": settings.handoffMode.rawValue, "custom_instruction_characters": settings.instructions.count,
                "transcripts": voice.transcripts.map { ["speaker": $0.speaker, "text": $0.text] }]
            try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
                .write(to: evidence.appendingPathComponent("native-memory-voice-live.json"))
        }
        do {
            mark("call_start")
            voice.start { VoiceConfiguration(baseURL: URL(string: credential.baseUrl)!, apiKey: credential.apiKey, agentID: agentID) }
            try await wait({ voice.phase == .active }, seconds: 50)
            mark("call_ready")
            mark("greeting_fixture_start")
            try play("/tmp/nanocodex-native-voice-greeting.wav")
            try await wait({ voice.outputLevel > 0.015 })
            mark("greeting_audio")
            try await wait({ voice.transcripts.contains { $0.speaker == "assistant" && !$0.text.isEmpty } })
            mark("greeting_transcript")
            try await wait({ player?.isRunning != true && voice.outputLevel < 0.004 })
            let previousIDs = Set(voice.transcripts.map(\.id))
            let before = try Data(contentsOf: URL(fileURLWithPath: timingPath)).count
            mark("personal_fixture_start")
            try play("/tmp/nanocodex-native-voice-personal.wav")
            try await wait({ voice.transcripts.contains { row in
                guard row.speaker == "assistant", !previousIDs.contains(row.id) else { return false }
                let text = row.text.lowercased().replacingOccurrences(of: "’", with: "'")
                return ["don't know", "do not know", "couldn't find", "could not find", "can't find", "cannot find", "no record", "don't have", "do not have"].contains { text.contains($0) }
            } }, seconds: 60)
            mark("personal_unknown_answer")
            // Keep listening through the final spoken sentence. A partial
            // "couldn't find" prefix alone is not evidence of the full answer.
            try await wait({
                guard !voice.isWorking, voice.outputLevel < 0.004,
                      let data = try? Data(contentsOf: URL(fileURLWithPath: timingPath)) else { return false }
                let current = String(decoding: data.dropFirst(before), as: UTF8.self)
                guard let done = current.range(of: "realtime.turn.done.role.assistant", options: .backwards),
                      let output = current.range(of: "realtime.output_transcript.added", options: .backwards) else { return false }
                return done.lowerBound > output.lowerBound
            })
            mark("personal_response_finished")
            let trace = String(decoding: try Data(contentsOf: URL(fileURLWithPath: timingPath)).dropFirst(before), as: UTF8.self)
            guard trace.contains("delegate.begin") else {
                throw RuntimeFailure(message: "Personal memory was answered without checking the managed agent")
            }
            try save("passed")
            voice.stop(); await voice.finishStopping()
            _ = try await client.json(path: "/v1/agents/\(agentID)", method: "DELETE")
        } catch {
            mark("failed"); try? save("failed", error: error.localizedDescription)
            await Self.captureFailureState(client: client, agentID: agentID, evidence: evidence)
            voice.stop(); await voice.finishStopping()
            _ = try? await client.json(path: "/v1/agents/\(agentID)", method: "DELETE")
            throw error
        }
    }

    @MainActor
    func testNativeApplicationSceneHasContent() async throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_DESKTOP_WINDOW_LIVE"] == "1" else {
            throw XCTSkip("Set NANOCODEX_DESKTOP_WINDOW_LIVE=1 to inspect the hosted app scene")
        }
        let deadline = Date().addingTimeInterval(5)
        while !NSApp.windows.contains(where: { $0.title == "Nanocodex" && $0.canBecomeMain }), Date() < deadline {
            try await Task.sleep(for: .milliseconds(50))
        }
        let window = try XCTUnwrap(NSApp.windows.first { $0.title == "Nanocodex" && $0.canBecomeMain })
        let content = try XCTUnwrap(window.contentView)
        XCTAssertGreaterThan(content.bounds.width, 0)
        XCTAssertGreaterThan(content.bounds.height, 0)
        XCTAssertFalse(content.subviews.isEmpty)
        print("NATIVE_WINDOW visible=\(window.isVisible) width=\(content.bounds.width) height=\(content.bounds.height) subviews=\(content.subviews.count)")
    }

    @MainActor
    func testNativeOwnedAgentDiagnostics() async throws {
        let env = ProcessInfo.processInfo.environment
        let requestedID = env["NANOCODEX_DIAGNOSTIC_AGENT_ID"] ?? ""
        let title = env["NANOCODEX_DIAGNOSTIC_AGENT_TITLE"] ?? ""
        guard !requestedID.isEmpty || !title.isEmpty else {
            throw XCTSkip("Set NANOCODEX_DIAGNOSTIC_AGENT_TITLE or NANOCODEX_DIAGNOSTIC_AGENT_ID to one owned test agent")
        }
        let credential = try XCTUnwrap(AccountKeychain.read())
        let client = ManagedClient(credential: try AccountCredential(origin: credential.baseUrl, apiKey: credential.apiKey))
        defer { client.close() }
        let agentID: String
        if !requestedID.isEmpty {
            _ = try ManagedClient.agentPath(requestedID)
            agentID = requestedID
        } else {
            let matches = try await client.list().filter { $0.title == title }
            XCTAssertEqual(matches.count, 1, "The exact owned fixture title must select one agent")
            agentID = try XCTUnwrap(matches.first?.id)
        }
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        await Self.captureFailureState(client: client, agentID: agentID, evidence: evidence)
        let report = try JSONDecoder().decode(InboxCore.JSON.self, from: Data(contentsOf:
            evidence.appendingPathComponent("native-memory-voice-failure-state.json")))
        XCTAssertFalse(report["state_read_failed"].bool, "Owned agent state must be readable")
        XCTAssertFalse(report["history_read_failed"].bool, "Owned agent history must be readable")
    }

    private static func captureFailureState(client: ManagedClient, agentID: String, evidence: URL) async {
        // Only this fixture's newly created agent; never persist messages,
        // tool payloads, headers, or credentials in transport diagnostics.
        func token(_ value: String) -> String {
            value.range(of: "^[A-Za-z0-9._:-]{1,128}$", options: .regularExpression) == nil ? "" : value
        }
        var report: [String: Any] = ["agent_id": agentID, "captured_at": Date().ISO8601Format()]
        var turnIDs = Set<String>()
        do {
            let state = try await client.state(agentID)
            report["latest_event_cursor"] = token(state["latest_event_cursor"].string)
            let active = state["active_turns"].array.map { token($0.string) }.filter { !$0.isEmpty }
            report["active_turns"] = active; turnIDs.formUnion(active)
        } catch { report["state_read_failed"] = true }
        do {
            let page = try await client.history(agentID)
            report["history_latest_cursor"] = page.latest.rawValue
            report["history_has_more"] = page.hasMore
            report["events"] = page.events.map { event -> [String: Any] in
                let turnID = token(event.turnID)
                if !turnID.isEmpty { turnIDs.insert(turnID) }
                var row: [String: Any] = ["cursor": event.cursor.rawValue, "type": token(event.type),
                    "turn_id": turnID, "agent_event_type": token(event.data["event"]["type"].string)]
                for field in ["timestamp", "created_at"] {
                    if case .number(let timestamp) = event.data[field] { row[field] = timestamp }
                }
                return row
            }
        } catch { report["history_read_failed"] = true }
        var turns: [[String: Any]] = []
        for id in turnIDs.sorted().prefix(8) {
            do {
                let turn = try await client.turn(agentID: agentID, turnID: id)
                var row: [String: Any] = ["turn_id": id, "state": token(turn["state"].string),
                    "status": token(turn["status"].string), "error_code": token(turn["error"]["code"].string)]
                if case .number(let retryAt) = turn["retry_at"] { row["retry_at"] = retryAt }
                turns.append(row)
            } catch { turns.append(["turn_id": id, "read_failed": true]) }
        }
        report["turns"] = turns
        if let data = try? JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]) {
            try? data.write(to: evidence.appendingPathComponent("native-memory-voice-failure-state.json"))
        }
    }

    private static var connectedState: JSONValue { .object(["connected": .bool(true), "baseUrl": .string("https://service.invalid"), "threads": .array([]), "hands": .array([]), "defaults": .object([:]), "platform": .string("darwin"), "version": .string("0.1.0"), "accountScope": .string("voice-evidence")]) }
    private static func emptyThread(_ id: String) -> JSONValue { .object(["id": .string(id), "events": .array([]), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([]), "settings": .object(["model": .string("gpt-5.4"), "thinking": .string("high"), "reasoning_mode": .string("standard"), "fast_mode": .bool(false)])]) }
    private static func defaultInput() throws -> AudioDeviceID {
        var address = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultInputDevice, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
        var device: AudioDeviceID = 0; var size = UInt32(MemoryLayout.size(ofValue: device))
        guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, &device) == noErr else { throw RuntimeFailure(message: "Cannot read audio input") }
        return device
    }
    private static func setDefaultInput(_ device: AudioDeviceID) throws {
        var address = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultInputDevice, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
        var device = device
        guard AudioObjectSetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, UInt32(MemoryLayout.size(ofValue: device)), &device) == noErr else { throw RuntimeFailure(message: "Cannot select audio input") }
    }
    private static func audioDevice(named name: String) throws -> AudioDeviceID {
        var address = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDevices, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
        var size: UInt32 = 0
        AudioObjectGetPropertyDataSize(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size)
        var devices = [AudioDeviceID](repeating: 0, count: Int(size) / MemoryLayout<AudioDeviceID>.size)
        AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, &devices)
        for device in devices {
            var property = AudioObjectPropertyAddress(mSelector: kAudioObjectPropertyName, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
            var value: CFString = "" as CFString; var length = UInt32(MemoryLayout<CFString>.size)
            if AudioObjectGetPropertyData(device, &property, 0, nil, &length, &value) == noErr, value as String == name { return device }
        }
        throw XCTSkip("Install BlackHole 2ch for native synthetic speech evidence")
    }
}
