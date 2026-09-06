import Foundation
import InboxCore

enum DemoContent {
    static func cards() -> [AgentCard] {
        let values: [(String, String, String, String)] = [
            ("durability", "Make long sessions bulletproof", "Ready", "The reconnect fix is ready. Two turns survive a disconnect, and steering stays attached to the right run. Ready for your review."),
            ("inbox", "Build the agent inbox", "Running", "Wiring the card stack to live sessions. Drafts stay with their agent while you move through the deck."),
            ("data", "Tighten the fuel forecast", "Running", "Comparing the latest price observations against the holdout window. Checking where the forecast drifts."),
            ("hands", "Reconnect the browser Hand", "Failed", "The browser Hand disconnected before the page loaded. Reconnect the Hand, then send a follow-up to continue.")
        ]
        return values.enumerated().map { index, value in
            var card = AgentCard(id: value.0, title: value.1, updatedAt: Double(100 - index), turnCount: 4)
            if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_LONG_THREAD"] == "1" {
                card.preview = (1...30).map { "Progress note \($0). Checking the reconnect boundary and preserving your draft while you read." }.joined(separator: "\n\n")
            }
            card.status = value.2; if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_LONG_THREAD"] != "1" { card.preview = value.3 }; card.model = "gpt-6-astra"; card.checked = true
            card.latestCursor = Cursor(rawValue: "12")!; card.stateCursor = card.latestCursor
            if value.2 == "Running" { card.activeTurns = ["demo-turn-" + value.0] }
            return card
        }
    }
    static func rows(_ id: String) -> [TranscriptRow] {
        guard let card = cards().first(where: { $0.id == id }) else { return [] }
        if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_LONG_THREAD"] == "1" {
            return (1...36).map { .init(id: "note-\($0)", role: "Agent", text: "Progress note \($0). Checking the reconnect boundary and preserving your place while new output arrives.") }
        }
        var activity = ToolPresentation(name: "exec_command", arguments: .object(["cmd": .string("swift test --package-path apple/InboxCore"), "workdir": .string("apple")]))
        activity.finish(.object(["output": .string("Sample result: reconnect and steering checks passed. This demo does not execute commands."), "exit_code": .number(0)]))
        if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_TOOL_ERROR"] == "1" {
            activity.finish(.object(["stderr": .string("The browser disconnected. Reconnect it and try again."), "exit_code": .number(1)]))
        }
        return [.init(id: "user-" + id, role: "You", text: card.title),
                .init(id: "tool-" + id, role: "Tool", text: activity.title, tool: activity),
                .init(id: "agent-" + id, role: "Agent", text: card.preview, running: card.isRunning)]
    }
}
