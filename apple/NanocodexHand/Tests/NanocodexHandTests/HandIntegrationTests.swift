import XCTest
import InboxCore
import NanocodexHand
import NanocodexContext

final class HandIntegrationTests: XCTestCase {
    @MainActor
    func testRealAgentQueriesCapturedMessagesThroughHand() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["NANOCODEX_HAND_CONTEXT_LIVE"] == "1" else { throw XCTSkip("Set NANOCODEX_HAND_CONTEXT_LIVE=1 for live message-query evidence.") }
        let credential = try AccountCredential(origin: environment["NANOCODEX_MANAGED_URL"] ?? "https://nanocodex.gakonst.workers.dev", apiKey: XCTUnwrap(environment["NC_API_KEY"]))
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("message-hand-live-" + UUID().uuidString)
        let store = ContextStore(directory: root.appendingPathComponent("context")), scope = UUID().uuidString
        try store.activate(scope); try store.setEnabled(true, scope: scope)
        let sources = ["Messages", "WhatsApp", "Instagram", "Signal"]
        let codes = sources.map { _ in "MESSAGE_" + UUID().uuidString.replacingOccurrences(of: "-", with: "") }
        for (source, code) in zip(sources, codes) {
            let message = "Friday museum plans. " + String(repeating: "The museum is by the station. ", count: 12) + "Booking code: \(code)"
            try store.capture([.init(source: source, text: message, sender: "Alex", thread: "Weekend plans")], scope: scope)
        }
        let id = "ios-context-" + UUID().uuidString.lowercased()
        let workspace = try HandWorkspace(id: id, name: "Message context iPhone", root: root.appendingPathComponent("workspace"), messageContext: ContextQuery(store: store, scope: scope))
        let hand = try HandSession(credential: credential, workspace: workspace)
        defer { hand.close(); try? FileManager.default.removeItem(at: root) }
        hand.start(); try await ready(hand)
        let client = ManagedClient(credential: credential)
        defer { client.close() }
        let agent = try await client.create(requestID: UUID().uuidString)
        print("Message Hand validation agent: \(agent)")
        addTeardownBlock {
            let cleanup = ManagedClient(credential: credential)
            defer { cleanup.close() }
            _ = try await cleanup.json(path: ManagedClient.agentPath(agent), method: "DELETE")
        }
        let first = AgentCommand(agentID: agent, input: "Using my connected phone Hand \(id), find Alex's Friday museum booking in iMessage, WhatsApp, Instagram, and Signal. Reply with the booking code from each app's captured message.", kind: .followUp)
        _ = try await client.command(first)
        let firstReply = try await completed(first, client: client)
        for code in codes { XCTAssertTrue(firstReply.contains(code), firstReply) }
        hand.stop(); hand.start(); try await ready(hand)
        let newCode = "SIGNAL_" + UUID().uuidString.replacingOccurrences(of: "-", with: "")
        // A new writer represents the next capture arriving independently.
        try ContextStore(directory: root.appendingPathComponent("context")).capture([.init(source: "Signal", text: "Updated Friday museum booking: \(newCode)", sender: "Alex", thread: "Weekend plans")], scope: scope)
        let second = AgentCommand(agentID: agent, input: "My same phone Hand is back. Search Signal for Alex's updated Friday museum booking, read that captured message, and reply with only the updated code. Query the phone again; this is new context.", kind: .followUp)
        _ = try await client.command(second)
        let secondReply = try await completed(second, client: client)
        XCTAssertEqual(secondReply.trimmingCharacters(in: .whitespacesAndNewlines), newCode)
        print("Agent discovered message tools and queried fresh context across Hand reconnect without attached messages.")
    }
    @MainActor
    func testRealAgentUsesNativeDeviceFilesAcrossReconnection() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["NANOCODEX_HAND_LIVE"] == "1" else { throw XCTSkip("Set NANOCODEX_HAND_LIVE=1 for the real device Hand journey.") }
        let credential = try AccountCredential(origin: environment["NANOCODEX_MANAGED_URL"] ?? "https://nanocodex.gakonst.workers.dev", apiKey: XCTUnwrap(environment["NC_API_KEY"]))
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("native-device-live-" + UUID().uuidString)
        let id = "ios-test-" + UUID().uuidString.lowercased()
        let workspace = try HandWorkspace(id: id, name: "Native iPhone Hand test", root: root)
        let hand = try HandSession(credential: credential, workspace: workspace)
        defer { hand.close(); try? FileManager.default.removeItem(at: root) }
        hand.start()
        try await ready(hand)
        let client = ManagedClient(credential: credential)
        defer { client.close() }
        let agent = try await client.create(requestID: UUID().uuidString)
        addTeardownBlock {
            let cleanup = ManagedClient(credential: credential)
            defer { cleanup.close() }
            _ = try await cleanup.json(path: ManagedClient.agentPath(agent), method: "DELETE")
        }
        let first = AgentCommand(agentID: agent, input: "Use accountInfo to find the already-connected Hand \(id). Find its write_file and read_file tools with tool_search. Write exactly IOS_AUTOMATIC_HAND_OK to proof.txt in that Hand's workspace, then read it back. Use that exact Hand, not /brain or another machine. Do not ask about setup. Reply with the file contents.", kind: .followUp)
        _ = try await client.command(first)
        let reply = try await completed(first, client: client)
        XCTAssertTrue(reply.contains("IOS_AUTOMATIC_HAND_OK"), reply)
        XCTAssertEqual(try String(contentsOf: root.appendingPathComponent("proof.txt"), encoding: .utf8), "IOS_AUTOMATIC_HAND_OK")
        hand.stop(); XCTAssertFalse(hand.connected)
        hand.start(); try await ready(hand)
        let second = AgentCommand(agentID: agent, input: "The same iPhone Hand has reconnected. Read proof.txt from its read_file tool again and reply with its contents followed by RECONNECTED_OK.", kind: .followUp)
        _ = try await client.command(second)
        let after = try await completed(second, client: client)
        XCTAssertTrue(after.contains("IOS_AUTOMATIC_HAND_OK") && after.contains("RECONNECTED_OK"), after)
        print("Native device Hand: automatic catalog, real file roundtrip, reconnect, second durable turn passed.")
    }
    @MainActor
    private func ready(_ hand: HandSession) async throws {
        for _ in 0..<200 {
            if hand.connected { return }
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTFail("Native Hand did not connect: " + (hand.lastError ?? "No ready frame"))
        throw HandFailure.connectionLost
    }
    private func completed(_ command: AgentCommand, client: ManagedClient) async throws -> String {
        for _ in 0..<120 {
            let turn = try await client.json(path: ManagedClient.agentPath(command.agentID) + "/turns/" + command.requestID)
            if turn["state"].string == "completed" { return turn["terminal"]["final_message"].string }
            if ["failed", "cancelled"].contains(turn["state"].string) { throw HandFailure.connectionLost }
            try await Task.sleep(for: .seconds(1))
        }
        throw HandFailure.connectionLost
    }
}
