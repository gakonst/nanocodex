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
            ("inbox", "Build the agent inbox", "Running", "Keeping your conversations in sync. Drafts stay with their agent while you switch conversations."),
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
            // Exercise the catalog's longer labels in the normal demo journey.
            if card.id == "data" { card.model = "mimo-v2.6-pro"; card.thinking = "medium" }
            if card.id == "durability" { card.thinking = "xhigh" }
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

                """ + "\n\n" + String(repeating: "Keep the beginning of this long response intact. ", count: 35)
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
            if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_SIDEBAR"] == "1", card.isRunning {
                card.presentationActivity = card.id == "inbox" ? "I'm checking inbox state" : "I'm comparing forecast results"
                card.presentationTurnID = "demo-turn-" + card.id
            }
            card.presentationLastUserPrompt = card.id == "inbox" ? "Check the inbox navigation and running agents" : "Compare the forecast results"
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

    #if DEBUG
    /// Deterministic landscape, portrait and panorama originals reveal crop and
    /// aspect-ratio mistakes without relying on bundled or remote photography.
    private static func localPhotoArtwork(_ index: Int) -> Data {
        let width = index == 2 ? 480 : index == 3 ? 1080 : 800
        let height = index == 2 ? 800 : 480
        let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        context.scaleBy(x: CGFloat(width) / 800, y: CGFloat(height) / 600)
        func color(_ r: CGFloat, _ g: CGFloat, _ b: CGFloat) -> CGColor {
            CGColor(red: r, green: g, blue: b, alpha: 1)
        }
        let sky = index == 2 ? color(0.96, 0.69, 0.48) : color(0.36, 0.67, 0.84)
        let gradient = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(),
            colors: [color(0.91, 0.91, 0.78), sky] as CFArray, locations: [0, 1])!
        context.drawLinearGradient(gradient, start: CGPoint(x: 0, y: 180), end: CGPoint(x: 0, y: 600), options: [])
        context.setFillColor(color(1, 0.92, 0.64))
        context.fillEllipse(in: CGRect(x: index == 2 ? 455 : 510, y: 400, width: 94, height: 94))
        func ridge(_ points: [CGPoint], _ fill: CGColor) {
            context.beginPath(); context.move(to: points[0])
            for point in points.dropFirst() { context.addLine(to: point) }
            context.closePath(); context.setFillColor(fill); context.fillPath()
        }
        ridge([CGPoint(x: 0, y: 170), CGPoint(x: 0, y: 320), CGPoint(x: 180, y: 450),
               CGPoint(x: 330, y: 295), CGPoint(x: 480, y: 420), CGPoint(x: 800, y: 260),
               CGPoint(x: 800, y: 170)], color(0.27, 0.43, 0.49))
        ridge([CGPoint(x: 100, y: 370), CGPoint(x: 180, y: 450), CGPoint(x: 255, y: 374),
               CGPoint(x: 197, y: 398), CGPoint(x: 166, y: 387)], color(0.95, 0.96, 0.88))
        context.setFillColor(index == 2 ? color(0.24, 0.48, 0.45) : color(0.12, 0.48, 0.62))
        context.fill(CGRect(x: 0, y: 0, width: 800, height: 230))
        for line in 0..<22 {
            let y = CGFloat(line * 10 + 8)
            context.setStrokeColor(CGColor(red: 0.85, green: 0.94, blue: 0.87, alpha: 0.28))
            context.setLineWidth(2)
            context.move(to: CGPoint(x: CGFloat((line * 73) % 300), y: y))
            context.addLine(to: CGPoint(x: CGFloat(420 + (line * 41) % 380), y: y))
            context.strokePath()
        }
        ridge([CGPoint(x: 0, y: 0), CGPoint(x: 0, y: 190), CGPoint(x: 145, y: 155),
               CGPoint(x: 290, y: 0)], color(0.12, 0.26, 0.23))
        for tree in 0..<7 {
            let x = CGFloat(40 + tree * 29)
            let base = CGFloat(80 - tree * 7)
            let top = base + CGFloat(180 - tree * 12)
            context.setFillColor(color(0.09, 0.20, 0.19))
            context.fill(CGRect(x: x - 3, y: base, width: 6, height: top - base))
            ridge([CGPoint(x: x - 27, y: base + 30), CGPoint(x: x, y: top),
                   CGPoint(x: x + 27, y: base + 30)], color(0.10, 0.29, 0.25))
        }
        // A small bright sailboat remains recognizable in a centered square crop.
        context.setFillColor(color(0.97, 0.88, 0.66))
        context.fill(CGRect(x: 448, y: 109, width: 3, height: 105))
        ridge([CGPoint(x: 443, y: 127), CGPoint(x: 443, y: 214), CGPoint(x: 389, y: 127)], color(1, 0.96, 0.85))
        ridge([CGPoint(x: 397, y: 113), CGPoint(x: 480, y: 113), CGPoint(x: 467, y: 97),
               CGPoint(x: 410, y: 97)], color(0.72, 0.24, 0.15))
        let png = NSMutableData()
        let destination = CGImageDestinationCreateWithData(png, "public.png" as CFString, 1, nil)!
        CGImageDestinationAddImage(destination, context.makeImage()!, nil)
        CGImageDestinationFinalize(destination)
        return png as Data
    }

    /// Seed through InboxModel.demo() so the composer exercises its ordinary attachment path.
    static func composerPhotoFixtures() throws -> [PreparedAttachment] {
        try (1...3).map { index in
            try AttachmentPreparation.prepare(data: localPhotoArtwork(index),
                name: "Phone photo \(index).png", mediaType: "image/png")
        }
    }

    /// Keep these originals across relaunches so preview cleanup cannot be
    /// hidden by regenerating fixture files. Each test uses a unique profile.
    private static func localPhotoRows() -> [TranscriptRow] {
        do {
            let scope = "demo." + (ProcessInfo.processInfo.environment["NANOCODEX_DEMO_PROFILE"] ?? "default")
            let store = try AttachmentStore(scope: scope)
            let count = min(10, max(1, Int(ProcessInfo.processInfo.environment["NANOCODEX_DEMO_LOCAL_PHOTO_COUNT"] ?? "2") ?? 2))
            let seededKey = "local-photo-fixture.artwork-v2.\(count)." + scope
            let seeded = UserDefaults.standard.bool(forKey: seededKey)
            let caption = count == 1 ? "Review this phone photo." : "Compare these \(count) phone photos."
            var content: [JSON] = [.object(["type": .string("text"), "text": .string(caption)])]
            for index in 1...count {
                let artworkIndex = count == 1 ? Int(ProcessInfo.processInfo.environment["NANOCODEX_DEMO_LOCAL_PHOTO_STYLE"] ?? "1") ?? 1 : index
                let png = localPhotoArtwork(artworkIndex)
                let prepared = try AttachmentPreparation.prepare(data: png as Data, name: "Phone photo \(index).png", mediaType: "image/png")
                let attachment = try MessageAttachment(id: String(format: "00000000-0000-4000-8000-%012d", index),
                    name: prepared.attachment.name, mediaType: prepared.attachment.mediaType, byteCount: prepared.attachment.byteCount,
                    handID: "fixture-phone")
                if !seeded {
                    try store.save(PreparedAttachment(attachment: attachment, source: prepared.source, preview: prepared.preview))
                }
                content += try attachment.originalContent(path: attachment.originalPath)
            }
            UserDefaults.standard.set(true, forKey: seededKey)
            let projected = TranscriptInput(.array(content))
            var row = TranscriptRow(id: "local-phone-photos", role: "You", text: projected.text)
            row.imageFiles = projected.imageFiles
            return [row]
        } catch {
            return [.init(id: "local-photo-fixture-error", role: "Agent", text: "Local photo fixture failed: " + error.localizedDescription)]
        }
    }
    #endif

    static func rows(_ id: String) -> [TranscriptRow] {
        guard let card = cards().first(where: { $0.id == id }) else { return [] }
        #if DEBUG
        if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_LOCAL_PHOTOS"] == "1" { return localPhotoRows() }
        #endif
        if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_OUTPUT_LINKS"] == "1" {
            return [.init(id: "user-" + id, role: "You", text: "Show the videos"),
                    .init(id: "agent-" + id, role: "Agent", text: """
                    [Main launch video](sandbox:/brain/outputs/frontiers-next/frontiers-merch-launch-actual-character.mp4)
                    [Complete bundle](sandbox:/brain/outputs/frontiers-next/frontiers-launch-and-drops.zip)
                    [Web reference](https://example.com)
                    [Not an output](sandbox:/brain/tmp/secret.mp4)
                    """)]
        }
        if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_GENERATED_OUTPUTS"] == "1" { return generatedOutputRows() }
        if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_WIDE_TABLE"] == "1" {
            return [.init(id: "wide-table", role: "Agent", text: """
            ## Library review

            | Library | Replaces | Mobile behavior | License |
            | --- | --- | --- | --- |
            | MarkdownUI | Custom Markdown parser and table layout | Long descriptive cells wrap; wide tables scroll horizontally | MIT |
            | Nuke | Duplicate attachment download and thumbnail caches | Shared decoding, cancellation and bounded image memory | MIT |
            | GRDB | Separate pending-command preference writes | Atomic recovery of messages and steering after restart | MIT |

            End of table review. The final paragraph remains above the composer.
            """)]
        }
        if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_RENDER_PROFILE"] == "1" {
            // Keep the default fixture stable; allow deterministic long-session
            // profiling without account data or a live managed turn.
            let requested = Int(ProcessInfo.processInfo.environment["NANOCODEX_DEMO_RENDER_ROWS"] ?? "") ?? 80
            let count = min(2_000, max(1, requested))
            var rows: [TranscriptRow] = (1...count).map { index in
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
            if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_RENDER_TOOL"] == "1" {
                var tool = ToolPresentation(name: "exec_command", arguments: .object(["cmd": .string("echo synthetic-recycling-fixture")]))
                tool.finish(.object(["output": .string("Recycled tool output remains expanded."), "exit_code": .number(0)]))
                rows.append(.init(id: "profile-recycling-tool", role: "Tool", text: tool.title, tool: tool))
            }
            return rows
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
    private static var crmFailed = false
    // Opt-in external-backend replacement only. The production create/send,
    // durable PendingMessage admission and native composers are not bypassed.
    private static var composerJourney: Bool { ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_COMPOSER_JOURNEY"] == "1" }
    private static var composerCreationFailed = false
    private static var composerAgents: [String: String] = [:] // creation idempotency -> agent
    private static var composerAdmissions: [String: [String: Any]] = [:]
    private static var composerEvents: [String: [[String: Any]]] = [:]
    private static var composerStreams: [String: StartupFixtureProtocol] = [:]
    private static var historyStreams: [String: StartupFixtureProtocol] = [:]
    private static var historyPages: Int { ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_LIVE_READING"] == "1" ? 3 : historyMedia ? 6 : 20 }
    private static let historyPageSize = 128
    private static let historyPadding = String(repeating: "p", count: 1_200_000)
    private static var warmTabs: Bool { ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_WARM_TABS"] == "1" }
    private static var historyMedia: Bool { ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_HISTORY_MEDIA"] == "1" }
    private static var historyWindow: Bool { ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_HISTORY_WINDOW"] == "1" }
    private var stopped = false
    private var meetingTask: URLSessionDataTask?
    private var meetingSession: URLSession?
    private let requestID = UUID().uuidString
    private var composerBody = Data()
    private var composerStreamCursor = 0
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host?.hasPrefix("startup-fixture-") == true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    private func record(_ phase: String, bytes: Int? = nil) {
        var event: [String: Any] = ["phase": phase, "request": requestID, "path": request.url!.path,
                                    "query": request.url!.query ?? "", "method": request.httpMethod ?? "GET",
                                    "time": ProcessInfo.processInfo.systemUptime, "process": ProcessInfo.processInfo.processIdentifier]
        if let bytes { event["bytes"] = bytes }
        if Self.composerJourney {
            // Whitelist request evidence. Never serialize headers, credentials,
            // context/location, or the entire URLRequest into the fixture log.
            event["idempotency"] = request.value(forHTTPHeaderField: "Idempotency-Key") ?? ""
            if let payload = (try? JSONSerialization.jsonObject(with: composerBody)) as? [String: Any] {
                event["input"] = payload["input"]
                event["configuration"] = payload["configuration"]
                event["id"] = payload["id"]
            }
        }
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
            if Self.composerJourney, request.url?.path.hasPrefix("/v1/agents") == true { composerBody = readComposerBody() }
            record("start")
            // A second process launch reuses the on-disk account cache while
            // every transport operation fails, including roster and history.
            if ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_OFFLINE"] == "1" {
                record("offline")
                client?.urlProtocol(self, didFailWithError: URLError(.notConnectedToInternet))
                return
            }
            let path = request.url!.path
            if Self.composerJourney, serveComposerJourney(path) { return }
            // Only the simulator fixture may bridge meetings to a real local
            // Worker. Account bootstrap remains synthetic; meeting responses,
            // database persistence and mutation semantics are never mocked.
            if path == "/v1/meetings" || path.hasPrefix("/v1/meetings/") {
                if forwardMeetingJourney() { return }
            }
            let id = request.url!.pathComponents.dropFirst(3).first ?? "saved"
            let isStream = path.hasSuffix("/events")
            var status = 200, delay = 0.05, body = "{}"
            if path == "/v1/crm" {
                body = #"{"records":[{"id":"alex","kind":"person","name":"Alex Morgan","title":"Product designer · Example Studio"},{"id":"sam","kind":"person","name":"Sam Rivera","title":"Landscape architect"},{"id":"maya","kind":"person","name":"Maya Chen","title":"Engineer · Northstar"}],"next_cursor":null}"#
                let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
                if ProcessInfo.processInfo.environment["NANOCODEX_CRM_RETRY_FIXTURE"] == "1", !Self.crmFailed {
                    Self.crmFailed = true; status = 503; body = #"{"error":"crm_unavailable"}"#
                } else if query.contains(where: { $0.name == "q" && $0.value == "missing" }) {
                    body = #"{"records":[],"next_cursor":null}"#
                } else if query.contains(where: { $0.name == "kind" && $0.value == "company" }) {
                    body = #"{"records":[{"id":"studio","kind":"company","name":"Example Studio"}],"next_cursor":null}"#
                }
            } else if path == "/v1/crm/alex" {
                let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
                if query.contains(where: { $0.name == "timeline_cursor" && $0.value == "demo-timeline-2" }) {
                    body = #"{"timeline":[{"id":"cancelled-invite","kind":"calendar_meeting","title":"Canceled design sync","occurred_at":"2026-08-28T09:00:00.000Z","status":"cancelled","participation_status":"invited","origin":"source"},{"id":"declined-invite","kind":"calendar_meeting","title":"Declined planning call","occurred_at":"2026-08-27T09:00:00.000Z","status":"confirmed","participation_status":"declined","response_status":"declined","origin":"source"},{"id":"email-note","kind":"email","body":"Sent a follow-up with the sketches.","occurred_at":"2026-08-25T10:00:00.000Z","timestamp_basis":"imported_at","origin":"source"}],"timeline_next_cursor":null}"#
                } else {
                    body = #"{"record":{"id":"alex","kind":"person","name":"Alex Morgan","title":"Product designer · Example Studio"},"identities":[{"id":"social","kind":"github","value":"example"}],"facts":[{"id":"education","predicate":"bio.education","value":"Example University","origin":"user"}],"relationships":[{"id":"friend","from_id":"alex","to_id":"sam","to_name":"Sam Rivera","type":"worked_with","description":"Designed the community garden.","origin":"user"}],"notes":[{"id":"note","body":"Met at the design workshop.","created_at":"2026-09-01"}],"timeline":[{"id":"invite","kind":"calendar_meeting","title":"Community garden review","occurred_at":"2026-09-22T10:00:00.000Z","status":"confirmed","participation_status":"invited","attendance_status":"unknown","origin":"source"},{"id":"proposal","kind":"interaction","type":"proposal","summary":"Garden redesign proposal","body":"Shared the first design concept.","occurred_at":"2026-09-20","precision":"date","origin":"user"}],"timeline_next_cursor":"demo-timeline-2"}"#
                }
            } else if path == "/v1/crm/sam" {
                body = #"{"record":{"id":"sam","kind":"person","name":"Sam Rivera","title":"Landscape architect"},"identities":[],"facts":[],"relationships":[{"id":"friend","from_id":"alex","from_name":"Alex Morgan","to_id":"sam","to_name":"Sam Rivera","type":"worked_with","description":"Designed the community garden together.","origin":"user"}],"notes":[{"id":"sam-note","body":"Interested in making shared spaces feel more welcoming.","created_at":"2026-09-12"}],"timeline":[],"timeline_next_cursor":null}"#
            } else if path == "/v1/agents" {
                delay = 6
                if ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_REJECT"] == "1" { status = 401 }
            let now = Date().timeIntervalSince1970 * 1000
                body = #"{"data":["saved","other","slow"],"summaries":{"saved":{"title":"Saved conversation","updated_at":\#(now),"turn_count":1,"may_have_scheduled_jobs":true},"other":{"title":"Other conversation","updated_at":\#(now - 1),"turn_count":1,"may_have_scheduled_jobs":true},"slow":{"title":"Background conversation","updated_at":\#(now - 2),"turn_count":1,"may_have_scheduled_jobs":true}}}"#
            } else if path == "/v1/connectors/catalog" {
                body = #"{"providers":[{"id":"github","name":"GitHub","description":"Repositories, issues, pull requests, and workflows","capabilities":[{"id":"github","name":"GitHub"}]},{"id":"google","name":"Google Workspace","description":"Mail, files, calendars, tasks, documents, and contacts","capabilities":[{"id":"gmail","name":"Gmail"},{"id":"gcalendar","name":"Google Calendar"},{"id":"gcontacts","name":"Google Contacts"},{"id":"gdocs","name":"Google Docs"},{"id":"gdrive","name":"Google Drive"},{"id":"gsheets","name":"Google Sheets"},{"id":"gslides","name":"Google Slides"},{"id":"gtasks","name":"Google Tasks"}]},{"id":"slack","name":"Slack","description":"Messages, channels, search, and connected workspaces","capabilities":[{"id":"slack","name":"Slack"}]},{"id":"x","name":"X","description":"Posts, messages, follows, likes, bookmarks, and lists","capabilities":[{"id":"x","name":"X"}]},{"id":"spotify","name":"Spotify","description":"Playlists and library","capabilities":[{"id":"spotify","name":"Spotify"}]},{"id":"soundcloud","name":"SoundCloud","description":"Playlists and likes","capabilities":[{"id":"soundcloud","name":"SoundCloud"}]}]}"#
            } else if path == "/v1/connectors/mcp-connections" {
                let mercator = String(repeating: "m", count: 43), linear = String(repeating: "l", count: 43)
                body = #"{"mcp_connections":[{"id":"\#(mercator)","name":"Mercator","status":"connected"},{"id":"\#(linear)","name":"Linear","status":"authorization_required"}]}"#
            } else if path == "/v1/connectors" {
                let first = String(repeating: "a", count: 43), second = String(repeating: "b", count: 43)
                let accounts = #"[{"id":"\#(first)","label":"georgios@paradigm.xyz","account_id":"google-1","capabilities":["gmail","gcalendar","gcontacts","gdocs","gdrive","gsheets","gslides","gtasks"]},{"id":"\#(second)","label":"me@gakonst.com","account_id":"google-2","capabilities":["gmail","gcalendar","gcontacts","gdocs","gdrive","gsheets","gslides","gtasks"]}]"#
                body = #"{"connectors":{"github":{"connected":false,"connections":[]},"gmail":{"connected":true,"connections":\#(accounts)},"gcalendar":{"connected":true,"connections":\#(accounts)},"gcontacts":{"connected":true,"connections":\#(accounts)},"gdocs":{"connected":true,"connections":\#(accounts)},"gdrive":{"connected":true,"connections":\#(accounts)},"gsheets":{"connected":true,"connections":\#(accounts)},"gslides":{"connected":true,"connections":\#(accounts)},"gtasks":{"connected":true,"connections":\#(accounts)},"slack":{"connected":false,"connections":[]},"x":{"connected":false,"connections":[]},"spotify":{"connected":true,"connections":[{"id":"sssssssssssssssssssssssssssssssssssssssssss","label":"Music account","capabilities":["spotify"]}]},"soundcloud":{"connected":false,"connections":[]}}}"#
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
            if ProcessInfo.processInfo.environment["NANOCODEX_SESSION_DONE_FIXTURE"] == "1" {
                delay = 0.1
                let key = "session-done-fixture." + (ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_PROFILE"] ?? "default")
                var doneDates = UserDefaults.standard.dictionary(forKey: key) ?? [:]
                var revision = UserDefaults.standard.integer(forKey: key + ".revision")
                if path.hasSuffix("/done"), request.httpMethod == "PUT" {
                    let data: Data
                    if let supplied = request.httpBody { data = supplied }
                    else if let stream = request.httpBodyStream {
                        stream.open(); defer { stream.close() }
                        var bytes = Data(), buffer = [UInt8](repeating: 0, count: 1024)
                        while stream.hasBytesAvailable {
                            let count = stream.read(&buffer, maxLength: buffer.count)
                            if count <= 0 { break }; bytes.append(contentsOf: buffer.prefix(count))
                        }
                        data = bytes
                    } else { data = Data() }
                    let payload = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
                    let done = payload?["done"] as? Bool ?? false
                    record(done ? "mark-done" : "reopen")
                    if ProcessInfo.processInfo.environment["NANOCODEX_SESSION_DONE_FAILURE"] != "before" {
                        if done != (doneDates[id] != nil) { revision += 1 }
                        if done { doneDates[id] = Date().timeIntervalSince1970 * 1000 }
                        else { doneDates[id] = nil }
                        UserDefaults.standard.set(doneDates, forKey: key)
                        UserDefaults.standard.set(revision, forKey: key + ".revision")
                    }
                    body = String(data: try! JSONSerialization.data(withJSONObject: ["done": done,
                        "done_at": doneDates[id] ?? NSNull(), "presentation_revision": revision]), encoding: .utf8)!
                    if ProcessInfo.processInfo.environment["NANOCODEX_SESSION_DONE_FAILURE"] != nil {
                        status = 503; body = #"{"error":"synthetic_uncertain_write"}"#
                    }
                } else if path == "/v1/agents", let data = body.data(using: .utf8),
                          var roster = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
                          var summaries = roster["summaries"] as? [String: [String: Any]] {
                    for agent in summaries.keys {
                        summaries[agent]?["presentation"] = ["status": "completed", "updatedAt": Date().timeIntervalSince1970 * 1000, "revision": revision,
                            "done": doneDates[agent] != nil, "doneAt": doneDates[agent] ?? NSNull()]
                    }
                    roster["summaries"] = summaries
                    body = String(data: try! JSONSerialization.data(withJSONObject: roster), encoding: .utf8)!
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
            stopped = true; meetingTask?.cancel(); meetingSession?.invalidateAndCancel(); meetingTask = nil; meetingSession = nil; Self.historyStreams[requestID] = nil; Self.composerStreams[requestID] = nil; record("stop")
        }
    }

    /// URLSession may move a POST body to httpBodyStream. Consume it once on
    /// the fixture serial queue, retaining only the synthetic payload in memory.
    private func readComposerBody() -> Data {
        if let data = request.httpBody { return data }
        guard let stream = request.httpBodyStream else { return Data() }
        stream.open(); defer { stream.close() }
        var data = Data(), buffer = [UInt8](repeating: 0, count: 4096)
        while stream.hasBytesAvailable {
            let count = stream.read(&buffer, maxLength: buffer.count)
            if count <= 0 { break }
            data.append(contentsOf: buffer.prefix(count))
        }
        return data
    }

    private static func composerHistory(_ id: String) -> [[String: Any]] {
        if let events = composerEvents[id] { return events }
        return [["cursor": "1", "type": "turn_completed", "turn_id": "fixture-saved-" + id,
                 "final_message": "Loaded \(id) conversation."]]
    }

    /// Return only ordinary managed API JSON/SSE. Visible evidence is an
    /// assistant transcript produced by normal event projection, not fixture UI.
    private func serveComposerJourney(_ path: String) -> Bool {
        guard path == "/v1/agents" || path.hasPrefix("/v1/agents/") else { return false }
        let method = request.httpMethod ?? "GET"
        let id = request.url!.pathComponents.dropFirst(3).first ?? "saved"
        let key = request.value(forHTTPHeaderField: "Idempotency-Key") ?? ""
        let payload = (try? JSONSerialization.jsonObject(with: composerBody)) as? [String: Any]
        let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
        let stream = path.hasSuffix("/events")
        var status = 200, delay = 0.05
        var body: [String: Any] = [:]
        var admitted = false
        if path == "/v1/agents", method == "POST" {
            delay = 1.0 // allow prepare/send + durable outbox flush to share the real creation task
            let asyncConfiguration = payload?["configuration"] as? [String: Any]
            let validAsyncCreation = payload?.count == 1 && asyncConfiguration?.count == 1
                && asyncConfiguration?["code_mode_async"] as? Bool == true
            if (!composerBody.isEmpty && !validAsyncCreation) || key.isEmpty {
                status = 400; body = ["error": "fixture_invalid_creation_contract"]
            } else if ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_COMPOSER_CREATE_FAIL_ONCE"] == "1", !Self.composerCreationFailed {
                // Definite pre-creation failure; retry may safely reuse the key.
                Self.composerCreationFailed = true
                status = 503; body = ["error": "fixture_creation_not_admitted"]
                record("composer-create-rejected")
            } else {
                let created = Self.composerAgents[key] ?? "composer-agent-\(Self.composerAgents.count + 1)"
                Self.composerAgents[key] = created
                if Self.composerEvents[created] == nil { Self.composerEvents[created] = [] }
                body = ["agent_id": created]
                record("composer-created")
            }
        } else if path == "/v1/agents", method == "GET" {
            let ids = Self.composerAgents.values.sorted() + ["saved", "other", "slow"]
            var summaries: [String: Any] = [:]
            for agent in ids {
                summaries[agent] = ["title": agent.hasPrefix("composer-agent-") ? "Composer journey" : "\(agent.capitalized) conversation",
                                    "updated_at": Date().timeIntervalSince1970 * 1000, "turn_count": Self.composerAdmissions.values.filter { $0["agent_id"] as? String == agent }.count]
            }
            body = ["data": ids, "summaries": summaries]
        } else if path.hasSuffix("/turns"), method == "POST" {
            guard let turnID = payload?["id"] as? String, !turnID.isEmpty,
                  let input = payload?["input"] as? String, key == "inbox:" + turnID,
                  Self.composerAgents.values.contains(id) else {
                finishComposerResponse(["error": "fixture_invalid_admission_contract"], status: 400, stream: false, agentID: id, admitted: false, delay: delay)
                return true
            }
            if let prior = Self.composerAdmissions[key] {
                if prior["agent_id"] as? String != id || prior["input"] as? String != input {
                    status = 409; body = ["error": "fixture_idempotency_conflict"]
                } else {
                    body = ["turn_id": turnID, "accepted_cursor": prior["accepted_cursor"]!]
                }
            } else {
                let cursor = (Self.composerEvents[id]?.count ?? 0) + 1
                Self.composerAdmissions[key] = ["agent_id": id, "input": input, "turn_id": turnID, "accepted_cursor": String(cursor)]
                // Publish a snapshot of the actual external request recorder,
                // not merely a label/count synthesized by UI or model state.
                let evidence = composerTransportLedger()
                Self.composerEvents[id, default: []] += [
                    ["cursor": String(cursor), "type": "turn_accepted", "turn_id": turnID, "input": input],
                    ["cursor": String(cursor + 1), "type": "turn_completed", "turn_id": turnID, "final_message": evidence]
                ]
                body = ["turn_id": turnID, "accepted_cursor": String(cursor)]
                admitted = true; record("composer-admitted")
            }
        } else if path.hasSuffix("/events/history") {
            let all = Self.composerHistory(id)
            let before = query.first { $0.name == "before" }?.value.flatMap(Int.init)
            let after = query.first { $0.name == "after" }?.value.flatMap(Int.init)
            let events = all.filter { event in
                let cursor = Int(event["cursor"] as? String ?? "0") ?? 0
                return (before == nil || cursor < before!) && (after == nil || cursor > after!)
            }
            body = ["data": events, "has_more": false, "latest_cursor": all.last?["cursor"] ?? "0"]
        } else if stream {
            composerStreamCursor = query.first { $0.name == "cursor" }?.value.flatMap(Int.init) ?? 0
        } else if path.hasSuffix("/triggers") {
            body = ["data": []]
        } else if path.contains("/turns/"), method == "GET" {
            let turnID = request.url!.lastPathComponent
            body = Self.composerAdmissions.values.first { $0["turn_id"] as? String == turnID && $0["agent_id"] as? String == id } ?? [:]
        } else if path == "/v1/agents/" + id, method == "GET" {
            body = ["agent_id": id, "latest_event_cursor": Self.composerHistory(id).last?["cursor"] ?? "0", "active_turns": []]
        } else {
            status = 404; body = ["error": "fixture_unsupported_agent_operation"]
        }
        finishComposerResponse(body, status: status, stream: stream, agentID: id, admitted: admitted, delay: delay)
        return true
    }

    private func composerTransportLedger() -> String {
        let url = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("startup-requests.jsonl")
        let text = (try? String(contentsOf: url, encoding: .utf8)) ?? ""
        let entries: [[String: Any]] = text.split(separator: "\n").compactMap { line in
            guard let data = String(line).data(using: .utf8),
                  let event = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
                  event["process"] as? Int == Int(ProcessInfo.processInfo.processIdentifier),
                  event["phase"] as? String == "start", event["method"] as? String == "POST",
                  let path = event["path"] as? String,
                  path == "/v1/agents" || (path.hasPrefix("/v1/agents/") && path.hasSuffix("/turns")) else { return nil }
            var request: [String: Any] = ["method": "POST", "path": path, "idempotency": event["idempotency"] ?? ""]
            request["input"] = event["input"]; request["id"] = event["id"]
            request["configuration"] = event["configuration"]
            return request
        }
        let data = try! JSONSerialization.data(withJSONObject: entries, options: [.sortedKeys, .withoutEscapingSlashes])
        return "Composer fixture transport ledger: " + String(decoding: data, as: UTF8.self)
    }

    private func finishComposerResponse(_ body: [String: Any], status: Int, stream: Bool, agentID: String, admitted: Bool, delay: Double) {
        let data = stream ? Data(": keepalive\n\n".utf8) : (try! JSONSerialization.data(withJSONObject: body, options: [.sortedKeys]))
        Self.queue.asyncAfter(deadline: .now() + delay) { [self] in
            guard !stopped else { return }
            record("response", bytes: data.count)
            let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": stream ? "text/event-stream" : "application/json"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            if stream {
                Self.composerStreams[requestID] = self
                emitComposerEvents(agentID: agentID)
            } else {
                client?.urlProtocolDidFinishLoading(self)
            }
            if admitted {
                for subscriber in Self.composerStreams.values where subscriber.request.url?.path == "/v1/agents/" + agentID + "/events" {
                    subscriber.emitComposerEvents(agentID: agentID)
                }
            }
        }
    }

    private func emitComposerEvents(agentID: String) {
        guard !stopped else { return }
        for event in Self.composerHistory(agentID) {
            let cursor = Int(event["cursor"] as? String ?? "0") ?? 0
            guard cursor > composerStreamCursor else { continue }
            let data = try! JSONSerialization.data(withJSONObject: event, options: [.sortedKeys])
            let frame = Data("id: \(cursor)\ndata: ".utf8) + data + Data("\n\n".utf8)
            client?.urlProtocol(self, didLoad: frame)
            composerStreamCursor = cursor
        }
    }

    private func forwardMeetingJourney() -> Bool {
        guard let raw = ProcessInfo.processInfo.environment["NANOCODEX_MEETING_JOURNEY_ORIGIN"],
              var components = URLComponents(string: raw), components.scheme == "http",
              components.host == "127.0.0.1", let port = components.port, (1024...65535).contains(port),
              components.user == nil, components.password == nil, components.query == nil,
              components.fragment == nil, components.path.isEmpty || components.path == "/" else { return false }
        components.path = request.url!.path; components.percentEncodedQuery = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.percentEncodedQuery
        guard let url = components.url else { return false }
        var forwarded = request; forwarded.url = url
        if forwarded.httpBody == nil, let stream = request.httpBodyStream {
            stream.open(); defer { stream.close() }
            var body = Data(), buffer = [UInt8](repeating: 0, count: 8192)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                guard count >= 0, body.count + count <= 1_048_576 else {
                    client?.urlProtocol(self, didFailWithError: URLError(.dataLengthExceedsMaximum)); return true
                }
                if count == 0 { break }; body.append(contentsOf: buffer.prefix(count))
            }
            forwarded.httpBodyStream = nil; forwarded.httpBody = body
        }
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = []
        let session = URLSession(configuration: config); meetingSession = session
        meetingTask = session.dataTask(with: forwarded) { [weak self] data, response, error in
            Self.queue.async {
                guard let self, !self.stopped else { return }
                defer { self.meetingSession?.finishTasksAndInvalidate(); self.meetingSession = nil; self.meetingTask = nil }
                if let error { self.record("meeting-network-error"); self.client?.urlProtocol(self, didFailWithError: error); return }
                guard let response = response as? HTTPURLResponse, let data,
                      let safeResponse = HTTPURLResponse(url: self.request.url!, statusCode: response.statusCode, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"]) else {
                    self.client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse)); return
                }
                self.record("meeting-http-\(response.statusCode)", bytes: data.count)
                self.client?.urlProtocol(self, didReceive: safeResponse, cacheStoragePolicy: .notAllowed)
                self.client?.urlProtocol(self, didLoad: data); self.client?.urlProtocolDidFinishLoading(self)
            }
        }
        meetingTask?.resume(); return true
    }

    private static var historyLatest: Int { historyPages * historyPageSize + (historyLive ? 1 : 0) }
    private static func historyEvent(_ cursor: Int) -> [String: Any] {
        if cursor > historyPages * historyPageSize {
            return ["cursor": String(cursor), "type": "turn_completed", "turn_id": "fixture-live",
                    "final_message": "Fixture live arrival beyond history window."]
        }
        if historyMedia {
            let slot = (cursor - 1) % historyPageSize
            let pageIndex: Int = (cursor - 1) / historyPageSize
            let imageOffset: Int = max(0, slot - 121) / 2
            let call = "history-image-\(pageIndex * 3 + imageOffset + 1)"
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
            let liveDelay: Double = ProcessInfo.processInfo.environment["NANOCODEX_STARTUP_LIVE_READING"] == "1" ? 5 : 2
            queue.asyncAfter(deadline: .now() + liveDelay) {
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
