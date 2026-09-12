import Foundation
import InboxCore
import ImageIO

enum DemoContent {
    #if DEBUG
    static func scheduledJobs() -> [ScheduledJob] {
        guard ProcessInfo.processInfo.environment["NANOCODEX_DEMO_EMPTY_SCHEDULES"] != "1" else { return [] }
        let now = Date().timeIntervalSince1970 * 1000
        return [("durability", "daily-check", true), ("data", "daily-check", false)].map { agent, id, enabled in
            try! ScheduledJob(.object([
                "id": .string(id), "cron": .string("0 9 * * *"), "timezone": .string("Europe/Athens"),
                "input": .string(enabled ? "Check the reconnect tests and summarize any failures." : "Review the fuel forecast and report material changes."),
                "enabled": .bool(enabled), "session_mode": .string(enabled ? "new" : "continue"),
                "next_run_at": enabled ? .number((now + 86_400_000).rounded(.down)) : .null,
                "last_run_at": .number((now - 86_400_000).rounded(.down)),
                "last_skipped_at": enabled ? .null : .number((now - 3_600_000).rounded(.down)),
                "last_agent_id": .string(enabled ? "inbox" : agent)
            ]), agentID: agent)
        }
    }

    /// The UI journey uses the same protocol events as the live WebRTC channel.
    static var voiceTranscript: [(Duration, JSON)] {
        [
            (.zero, .object(["type": .string("turn.done"), "turn": .object([
                "role": .string("assistant"),
                "transcript": .string("<realtime_delegation><source>internal-only</source><input>Internal handoff.</input></realtime_delegation>")
            ])])),
            (.zero, .object(["type": .string("input_transcript.added"), "item": .object(["text": .string("Can you hear")])])),
            (.seconds(18), .object(["type": .string("input_transcript.added"), "item": .object(["text": .string(" me?")])])),
            (.zero, .object(["type": .string("turn.done"), "turn": .object(["role": .string("user"), "transcript": .string("Can you hear me?")])])),
            (.zero, .object(["type": .string("output_transcript.added"), "item": .object(["text": .string("I can")])])),
            (.seconds(8), .object(["type": .string("output_transcript.added"), "item": .object(["text": .string(" hear you")])])),
            (.seconds(8), .object(["type": .string("output_transcript.added"), "item": .object(["text": .string(" clearly.")])])),
            (.zero, .object(["type": .string("turn.done"), "turn": .object(["role": .string("assistant"), "transcript": .string("I can hear you clearly.")])])),
        ]
    }
    static var voiceDurableRows: [TranscriptRow] {
        let event = try! AgentEvent(.object([
            "cursor": .string("90"), "type": .string("turn_accepted"), "turn_id": .string("demo-voice"),
            "input": .string("<realtime_delegation><source>transcript_tail_flush</source><transcript_delta>user: Can you hear me?\nassistant: I can hear you clearly.</transcript_delta></realtime_delegation>")
        ]))
        return transcript([event])
    }
    #endif

    static func cards() -> [AgentCard] {
        #if DEBUG
        if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_EMPTY_AGENTS"] == "1" { return [] }
        #endif
        let values: [(String, String, String, String)] = [
            ("durability", "Make long sessions bulletproof", "Ready", "The reconnect fix is ready. Two turns survive a disconnect, and steering stays attached to the right run. Ready for your review."),
            ("inbox", "Build the agent inbox", "Running", "Connecting agent tabs to live sessions. Drafts stay with their agent while you switch tabs."),
            ("data", "Tighten the fuel forecast", "Running", "Comparing the latest price observations against the holdout window. Checking where the forecast drifts."),
            ("hands", "Reconnect the browser Hand", "Failed", "The browser Hand disconnected before the page loaded. Reconnect the Hand, then send a follow-up to continue.")
        ]
        return values.enumerated().map { index, value in
            var card = AgentCard(id: value.0, title: value.1, updatedAt: Double(100 - index), turnCount: 4)
            let longPreview = ProcessInfo.processInfo.environment["NANOCODEX_DEMO_LONG_THREAD"] == "1"
                || ProcessInfo.processInfo.environment["NANOCODEX_DEMO_LONG_PREVIEW"] == "1"
            if longPreview {
                card.preview = (1...30).map { "Progress note \($0). Checking the reconnect boundary and preserving your draft while you read." }.joined(separator: "\n\n")
            }
            card.status = value.2; if !longPreview { card.preview = value.3 }; card.model = "gpt-6-astra"; card.checked = true
            if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_MARKDOWN"] == "1" {
                card.preview = """
                # Markdown check

                Read **bold**, *italic*, and `inline code` with [a link](https://example.com).

                - First item
                - Second item

                > A quoted reply

                ```swift
                let marker = "**literal**"
                ```

                | Name | Value |
                | --- | --- |
                | **Result** | `42` |

                """ + String(repeating: "Keep the beginning of this long response intact. ", count: 35)
            }
            card.latestCursor = Cursor(rawValue: "12")!; card.stateCursor = card.latestCursor
            if value.2 == "Running" { card.activeTurns = ["demo-turn-" + value.0] }
            if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_ACTIVITY"] == "1" {
                try? card.apply(state: .object(["agent_id": .string(card.id),
                    "latest_event_cursor": .string("12"),
                    "active_turns": .array(card.activeTurns.map(JSON.string)),
                    "settings": .object(["model": .string(card.model)])]))
                let event: JSON = card.isRunning
                    ? .object(["type": .string("event"), "cursor": .string("13"), "turn_id": .string("demo-turn-" + card.id),
                        "event": .object(["type": .string("assistant.message"), "payload": .object([
                            "phase": .string("commentary"), "text": .string(value.3)])])])
                    : .object(["type": .string(card.status == "Failed" ? "turn_failed" : "turn_completed"),
                        "cursor": .string("13"), "turn_id": .string("demo-turn-" + card.id),
                        "error": .string("Browser Hand disconnected. Reconnect it to continue."),
                        "final_message": .string(value.3)])
                if let envelope = try? AgentEvent(event) { card.apply(events: [envelope]) }
            }
            return card
        }
    }
    /// A real raster and audio payload travel through the durable tool.result
    /// projector, including the runtime's JSON-string input_text/input_image list.
    private static func generatedOutputRows() -> [TranscriptRow] {
        let context = CGContext(data: nil, width: 280, height: 150, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        context.setFillColor(CGColor(red: 0.08, green: 0.13, blue: 0.27, alpha: 1)); context.fill(CGRect(x: 0, y: 0, width: 280, height: 150))
        context.setFillColor(CGColor(red: 0.2, green: 0.85, blue: 0.7, alpha: 1)); context.fill(CGRect(x: 30, y: 24, width: 48, height: 48))
        context.setFillColor(CGColor(red: 0.65, green: 0.48, blue: 1, alpha: 1)); context.fill(CGRect(x: 114, y: 24, width: 48, height: 82))
        context.setFillColor(CGColor(red: 1, green: 0.65, blue: 0.35, alpha: 1)); context.fill(CGRect(x: 198, y: 24, width: 48, height: 108))
        let png = NSMutableData()
        let destination = CGImageDestinationCreateWithData(png, "public.png" as CFString, 1, nil)!
        CGImageDestinationAddImage(destination, context.makeImage()!, nil); CGImageDestinationFinalize(destination)
        let image = (png as Data).base64EncodedString()
        var audio = Data("RIFF".utf8)
        func u16(_ value: UInt16) { audio.append(UInt8(value & 255)); audio.append(UInt8(value >> 8)) }
        func u32(_ value: UInt32) { for shift in stride(from: 0, through: 24, by: 8) { audio.append(UInt8((value >> shift) & 255)) } }
        u32(16036); audio.append(Data("WAVEfmt ".utf8)); u32(16); u16(1); u16(1); u32(8000); u32(16000); u16(2); u16(16)
        audio.append(Data("data".utf8)); u32(16000); audio.append(Data(count: 16000))
        func event(_ cursor: String, _ type: String, _ payload: JSON) -> AgentEvent {
            try! AgentEvent(.object(["cursor": .string(cursor), "type": .string("event"), "turn_id": .string("generated-turn"),
                "event": .object(["type": .string(type), "payload": payload])]))
        }
        let memory: JSON = .object(["operation": .string("read"), "memories": .array([.object([
            "key": .object(["id": .number(1), "version": .number(2)]), "content": .string("INTERNAL_MEMORY_RECORD")
        ])])])
        let inner: JSON = .object(["content": .array([.object(["type": .string("image"), "mimeType": .string("image/png"), "data": .string(image)])])])
        let content: JSON = .array([
            .object(["type": .string("input_text"), "text": .string("Script completed\nWall time 0.1 seconds\nOutput:\n")]),
            .object(["type": .string("input_text"), "text": .string(memory.pretty)]),
            .object(["type": .string("input_text"), "text": .string("INTERNAL_COMMAND_OUTPUT: completed diagnostics")]),
            .object(["type": .string("input_image"), "image_url": .string("data:image/png;base64," + image)])
        ])
        var extraMedia: [JSON] = []
        if let video = ProcessInfo.processInfo.environment["NANOCODEX_DEMO_VIDEO_BASE64"] {
            extraMedia.append(.object(["type": .string("video"), "mimeType": .string("video/mp4"), "name": .string("Sample video.mp4"), "data": .string(video)]))
        }
        let structured: JSON = .object(["exit_code": .number(0), "content": .array(extraMedia + [
            .object(["type": .string("resource"), "resource": .object(["uri": .string("artifact:///chart.csv"), "mimeType": .string("text/csv"), "blob": .string(Data("Series,Value\nA,48\nB,82\nC,108\n".utf8).base64EncodedString())])]),
            .object(["type": .string("audio"), "mimeType": .string("audio/wav"), "data": .string(audio.base64EncodedString())])
        ])])
        return transcript([
            event("1", "tool.call", .object(["call_id": .string("memory"), "tool": .string("memory"), "arguments": .object(["operation": .string("read")])])),
            event("2", "tool.result", .object(["call_id": .string("memory"), "result": .object(["content": .array([
                .object(["type": .string("text"), "text": .string(memory.pretty)])
            ])])])),
            event("3", "tool.call", .object(["call_id": .string("inner"), "tool": .string("make_chart"), "arguments": .null])),
            event("4", "tool.result", .object(["call_id": .string("inner"), "result": inner])),
            event("5", "tool.call", .object(["call_id": .string("outer"), "tool": .string("functions.exec"), "arguments": .string("image(chart); text(result)")])),
            event("6", "tool.result", .object(["call_id": .string("outer"), "tool": .string("functions.exec"), "result": .string(content.pretty), "structured_result": structured])),
            event("7", "tool.call", .object(["call_id": .string("wait"), "tool": .string("functions.wait"), "arguments": .null])),
            event("8", "tool.result", .object(["call_id": .string("wait"), "result": .string("INTERNAL_WAIT_OUTPUT: process finished")])),
            event("9", "assistant.message", .object(["phase": .string("final_answer"), "text": .string("## Generated chart\n\nThe **three bars** are ready to review.")])),
        ])
    }

    static func rows(_ id: String) -> [TranscriptRow] {
        guard let card = cards().first(where: { $0.id == id }) else { return [] }
        if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_GENERATED_OUTPUTS"] == "1" { return generatedOutputRows() }
        if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_RENDER_PROFILE"] == "1" {
            return (1...80).map { index in
                .init(id: "profile-\(index)", role: "Agent", text: """
                ## Review note \(index)

                Keep **the draft** and `cursor` attached to the same conversation while reading [the reference](https://example.com).

                - Preserve previous messages.
                - Render the latest update.

                > Reconnect without losing your place.

                | Check | Result |
                | --- | --- |
                | History | Ready |
                | Draft | Retained |
                """)
            }
        }
        if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_LONG_THREAD"] == "1" {
            return (1...36).map { .init(id: "note-\($0)", role: "Agent", text: "Progress note \($0). Checking the reconnect boundary and preserving your place while new output arrives.") }
        }
        if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_THINKING_MARKDOWN"] == "1" {
            return [.init(id: "user-" + id, role: "You", text: "Check this reasoning."),
                    .init(id: "thinking-" + id, role: "Thinking", text: """
                    ## Reasoning check

                    Check **both paths** and `answer` before continuing.

                    - Preserve the draft
                    - Keep the cursor

                    ```swift
                    let answer = 42
                    print("ready")
                    ```

                    > Ready to continue.
                    """),
                    .init(id: "agent-" + id, role: "Agent", text: "Both paths are ready.")]
        }
        var activity = ToolPresentation(name: "exec_command", arguments: .object(["cmd": .string("swift test --package-path apple/InboxCore"), "workdir": .string("apple")]))
        activity.finish(.object(["output": .string("Sample result: reconnect and steering checks passed. This demo does not execute commands."), "exit_code": .number(0)]))
        if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_TOOL_ERROR"] == "1" {
            activity.finish(.object(["stderr": .string("The browser disconnected. Reconnect it and try again."), "exit_code": .number(1)]))
        }
        if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_MANY_TOOLS"] == "1" {
            var update = TranscriptRow(id: "commentary-" + id, role: "Agent", text: "Checking the remaining steps.")
            update.phase = "commentary"
            return [.init(id: "user-" + id, role: "You", text: card.title),
                    .init(id: "thinking-1-" + id, role: "Thinking", text: "Checking the reconnect boundary before changing the implementation."),
                    .init(id: "tool-1-" + id, role: "Tool", text: activity.title, tool: activity),
                    update,
                    .init(id: "tool-2-" + id, role: "Tool", text: activity.title, tool: activity),
                    .init(id: "thinking-2-" + id, role: "Thinking", text: "The checks agree. Preparing a concise answer."),
                    .init(id: "tool-3-" + id, role: "Tool", text: activity.title, tool: activity),
                    .init(id: "agent-" + id, role: "Agent", text: card.preview, running: card.isRunning)]
        }
        return [.init(id: "user-" + id, role: "You", text: card.title),
                .init(id: "tool-" + id, role: "Tool", text: activity.title, tool: activity),
                .init(id: "agent-" + id, role: "Agent", text: card.preview, running: card.isRunning)]
    }
}

#if DEBUG && targetEnvironment(simulator)
import CryptoKit

/// Simulator-only transport fixture exercises real account restoration, history
/// ownership, and cancellation without signing into or modifying a live account.
enum StartupFixture {
    static var enabled: Bool { ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_FIXTURE"] == "1" }
    static let credential: AccountCredential = {
        let profile = ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_PROFILE"] ?? "default"
        let value = try! AccountCredential(origin: "https://startup-fixture-\(profile).invalid",
                                          apiKey: "ncx_live_abcdefgh1234_" + String(repeating: "x", count: 43))
        let scope = SHA256.hash(data: Data((value.origin + ":" + String(value.apiKey.prefix(21))).utf8)).map { String(format: "%02x", $0) }.joined()
        let key = "inbox.selectedTab." + scope
        if UserDefaults.standard.string(forKey: key) == nil { UserDefaults.standard.set("saved", forKey: key) }
        return value
    }()
    static var configuration: URLSessionConfiguration {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StartupFixtureProtocol.self]
        return configuration
    }
}

private final class StartupFixtureProtocol: URLProtocol, @unchecked Sendable {
    private static let queue = DispatchQueue(label: "nanocodex.startup-fixture")
    private static var historyLive = false
    private static var historyStreams: [String: StartupFixtureProtocol] = [:]
    private static var historyPages: Int { historyMedia ? 6 : 20 }
    private static let historyPageSize = 128
    private static let historyPadding = String(repeating: "p", count: 1_200_000)
    private static var warmTabs: Bool { ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_WARM_TABS"] == "1" }
    private static var historyMedia: Bool { ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_HISTORY_MEDIA"] == "1" }
    private static var historyWindow: Bool { ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_HISTORY_WINDOW"] == "1" }
    private var stopped = false
    private let requestID = UUID().uuidString
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host?.hasPrefix("startup-fixture-") == true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    private func record(_ phase: String, bytes: Int? = nil) {
        var event: [String: Any] = ["phase": phase, "request": requestID, "path": request.url!.path,
                                    "query": request.url!.query ?? "",
                                    "time": ProcessInfo.processInfo.systemUptime, "process": ProcessInfo.processInfo.processIdentifier]
        if let bytes { event["bytes"] = bytes }
        let url = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("startup-requests.jsonl")
        let data = (try! JSONSerialization.data(withJSONObject: event)) + Data("\n".utf8)
        if !FileManager.default.fileExists(atPath: url.path) { FileManager.default.createFile(atPath: url.path, contents: nil) }
        if let file = try? FileHandle(forWritingTo: url) {
            defer { try? file.close() }
            _ = try? file.seekToEnd(); try? file.write(contentsOf: data)
        }
    }
    override func startLoading() {
        Self.queue.async { [self] in
            guard !stopped else { return }
            record("start")
            let path = request.url!.path
            let id = request.url!.pathComponents.dropFirst(3).first ?? "saved"
            let isStream = path.hasSuffix("/events")
            var status = 200, delay = 0.05, body = "{}"
            if path == "/v1/agents" {
                delay = 6
                if ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_REJECT"] == "1" { status = 401 }
            let now = Date().timeIntervalSince1970 * 1000
                body = #"{"data":["saved","other","slow"],"summaries":{"saved":{"title":"Saved conversation","updated_at":\#(now),"turn_count":1,"may_have_scheduled_jobs":true},"other":{"title":"Other conversation","updated_at":\#(now - 1),"turn_count":1,"may_have_scheduled_jobs":true},"slow":{"title":"Background conversation","updated_at":\#(now - 2),"turn_count":1,"may_have_scheduled_jobs":true}}}"#
            } else if path.hasSuffix("/events/history") {
                delay = id == "slow" ? 20 : 10
                body = "{\"data\":[{\"cursor\":\"1\",\"type\":\"turn_completed\",\"turn_id\":\"t\",\"final_message\":\"Loaded \(id) conversation.\"}],\"has_more\":false,\"latest_cursor\":\"1\"}"
            } else if path.hasSuffix("/triggers") {
                body = #"{"data":[]}"#
            } else if isStream {
                body = ": keepalive\n\n"
            } else {
                body = "{\"agent_id\":\"\(id)\",\"latest_event_cursor\":\"1\",\"active_turns\":[]}"
            }
            if Self.warmTabs { delay = path.hasSuffix("/events/history") ? 2 : 0.1 }
            if Self.historyWindow {
                delay = 0.12
                if path == "/v1/agents" {
                    let now = Date().timeIntervalSince1970 * 1000
                    body = #"{"data":["saved"],"summaries":{"saved":{"title":"History window fixture","updated_at":\#(now),"turn_count":20}}}"#
                } else if path.hasSuffix("/events/history") {
                    body = Self.historyBody(request.url!)
                    if Self.historyMedia, request.url!.query?.contains("before=") == true {
                        delay = Double(ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_HISTORY_DELAY_MS"].flatMap(Int.init) ?? 3000) / 1000
                    }
                } else if isStream {
                    Self.historyStreams[requestID] = self
                } else if !path.hasSuffix("/triggers") {
                    body = "{\"agent_id\":\"saved\",\"latest_event_cursor\":\"\(Self.historyLatest)\",\"active_turns\":[]}"
                }
            }
            let responseBody = body, responseStatus = status
            Self.queue.asyncAfter(deadline: .now() + delay) { [self] in
                guard !stopped else { return }
                record("response", bytes: responseBody.utf8.count)
                let response = HTTPURLResponse(url: request.url!, statusCode: responseStatus, httpVersion: "HTTP/1.1",
                                               headerFields: ["Content-Type": isStream ? "text/event-stream" : "application/json"])!
                client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                client?.urlProtocol(self, didLoad: Data(responseBody.utf8))
                if !isStream { client?.urlProtocolDidFinishLoading(self) }
            }
        }
    }
    override func stopLoading() {
        Self.queue.async { [self] in
            stopped = true; Self.historyStreams[requestID] = nil; record("stop")
        }
    }

    private static var historyLatest: Int { historyPages * historyPageSize + (historyLive ? 1 : 0) }
    private static func historyEvent(_ cursor: Int) -> [String: Any] {
        if cursor > historyPages * historyPageSize {
            return ["cursor": String(cursor), "type": "turn_completed", "turn_id": "fixture-live",
                    "final_message": "Fixture live arrival beyond history window."]
        }
        if historyMedia {
            let slot = (cursor - 1) % historyPageSize
            let call = "history-image-\((cursor - 1) / historyPageSize * 3 + max(0, slot - 121) / 2 + 1)"
            if slot >= 121 && slot <= 126 {
                let index = (cursor - 1) / historyPageSize * 3 + (slot - 121) / 2 + 1
                let type = slot % 2 == 1 ? "tool.call" : "tool.result"
                var payload: [String: Any] = ["call_id": call, "tool": "make_chart"]
                if type == "tool.call" { payload["arguments"] = ["title": "History image \(index)"] }
                else {
                    let context = CGContext(data: nil, width: 240, height: 360, bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                    context.setFillColor(CGColor(red: CGFloat(index) / 20, green: 0.3, blue: 0.6, alpha: 1))
                    context.fill(CGRect(x: 0, y: 0, width: 240, height: 360))
                    let data = NSMutableData()
                    let destination = CGImageDestinationCreateWithData(data, "public.png" as CFString, 1, nil)!
                    CGImageDestinationAddImage(destination, context.makeImage()!, nil); CGImageDestinationFinalize(destination)
                    let bytes = data as Data
                    payload["result"] = ["type": "image", "mimeType": "image/png", "data": bytes.base64EncodedString(), "title": "History image \(index)"]
                }
                return ["cursor": String(cursor), "type": "event", "turn_id": "one-large-turn", "event": ["type": type, "payload": payload]]
            }
            if cursor == historyPages * historyPageSize {
                return ["cursor": String(cursor), "type": "turn_completed", "turn_id": "one-large-turn", "final_message": "Completed image review."]
            }
            return ["cursor": String(cursor), "type": "event", "turn_id": "one-large-turn", "event": ["type": "fixture.transport", "payload": [:]]]
        }
        let page = (cursor - 1) / historyPageSize + 1
        if cursor % historyPageSize == 0 {
            return ["cursor": String(cursor), "type": "turn_completed", "turn_id": "fixture-page-\(page)",
                    "final_message": "## History page \(page) of \(historyPages)\n\n"
                        + String(repeating: "This page stays readable across native history paging and live updates. ", count: 8)]
        }
        var payload: [String: Any] = ["page": page]
        if cursor % historyPageSize == 1 { payload["padding"] = historyPadding }
        return ["cursor": String(cursor), "type": "event", "turn_id": "fixture-page-\(page)",
                "event": ["type": "fixture.transport", "payload": payload]]
    }
    private static func historyBody(_ url: URL) -> String {
        let query = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        let before = query.first { $0.name == "before" }?.value.flatMap(Int.init)
        let after = query.first { $0.name == "after" }?.value.flatMap(Int.init)
        let head = historyLatest
        let first: Int, last: Int
        if let after { first = after + 1; last = min(head, after + historyPageSize) }
        else { last = min(head, (before ?? (head + 1)) - 1); first = max(1, last - historyPageSize + 1) }
        let events = first <= last ? (first...last).map(historyEvent) : []
        let body: [String: Any] = ["data": events, "latest_cursor": String(head),
                                  "has_more": after == nil ? first > 1 : last < head]
        if let before, before <= historyPageSize + 1, !historyLive {
            historyLive = true
            queue.asyncAfter(deadline: .now() + 2) {
                let encoded = try! JSONSerialization.data(withJSONObject: historyEvent(historyLatest))
                let frame = Data("id: \(historyLatest)\ndata: ".utf8) + encoded + Data("\n\n".utf8)
                for stream in historyStreams.values where !stream.stopped {
                    stream.record("live", bytes: frame.count)
                    stream.client?.urlProtocol(stream, didLoad: frame)
                }
            }
        }
        return String(data: try! JSONSerialization.data(withJSONObject: body), encoding: .utf8)!
    }
}
#endif
