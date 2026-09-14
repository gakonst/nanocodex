import Foundation
import XCTest
import InboxCore
@testable import NanocodexContext

final class ContextDeliveryTests: XCTestCase {
    /// Opt-in real-service evidence using only generated text and a new agent.
    /// The context library's production target has no service dependency.
    func testCapturedContextSurvivesLiveDeliveryAndReplay() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["NANOCODEX_CONTEXT_LIVE"] == "1", let key = environment["NC_API_KEY"] else {
            throw XCTSkip("Set NANOCODEX_CONTEXT_LIVE=1 and NC_API_KEY for live context delivery evidence.")
        }
        let credential = try AccountCredential(origin: environment["NANOCODEX_MANAGED_URL"] ?? "https://nanocodex.gakonst.workers.dev", apiKey: key)
        var client = ManagedClient(credential: credential)
        defer { client.close() }
        let agentID = try await client.create(requestID: UUID().uuidString)
        print("Context validation agent: \(agentID)")
        let path = try ManagedClient.agentPath(agentID)
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("context-live-" + UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        do {
            let scope = UUID().uuidString, store = ContextStore(directory: directory)
            try store.activate(scope); try store.setEnabled(true, scope: scope)
            let sources = ["Instagram", "Messages"]
            let codes = sources.map { _ in "CAPTURE_" + UUID().uuidString.replacingOccurrences(of: "-", with: "") }
            for (source, code) in zip(sources, codes) {
                let provider = NSItemProvider(object: "Verification code: \(code)" as NSString)
                var input = try await ContextImport.load(provider)
                input.source = source
                try store.capture([input], session: store.captureSession())
            }
            var turnIDs: [String] = []
            for (index, source) in sources.enumerated() {
                try store.route(source: source, agentID: agentID, scope: scope)
                let captures = ContextPrompt.candidates(in: try store.snapshot(scope: scope), agentID: agentID)
                XCTAssertEqual(captures.map(\.input.source), [source])
                let input = try ContextPrompt.render(captures) + "\n\nMy request:\nReply with only the verification code in the \(source) captured text included in this request. Do not use tools."
                let pending = PendingMessage(agentID: agentID, input: input, predecessor: "", contextIDs: captures.map(\.id))
                let frozen = try JSONEncoder().encode(pending)
                let receipt = try await client.command(pending.submission)
                XCTAssertEqual(receipt["turn_id"].string, pending.id)
                try store.markUsed(captures.map(\.id), agentID: agentID, turnID: pending.id, scope: scope)

                // Reopen both the client and the serialized outbox, then retry
                // exactly the same admitted turn against the real service.
                client.close(); client = ManagedClient(credential: credential)
                let restored = try JSONDecoder().decode(PendingMessage.self, from: frozen)
                let replay = try await client.command(restored.submission)
                XCTAssertEqual(replay["turn_id"].string, pending.id)
                let terminal = try await completedTurn(path: path, id: pending.id, client: client)
                XCTAssertEqual(terminal["terminal"]["final_message"].string.trimmingCharacters(in: .whitespacesAndNewlines), codes[index])
                turnIDs.append(pending.id)
            }
            client.close(); client = ManagedClient(credential: credential)
            let history = try await client.history(agentID)
            let completed = history.events.filter { $0.type == "turn_completed" }.map(\.turnID)
            XCTAssertEqual(Set(completed), Set(turnIDs))
            XCTAssertEqual(completed.count, 2, "Replaying admitted context must not create a second turn.")
            let reopened = ContextStore(directory: directory)
            XCTAssertTrue(ContextPrompt.candidates(in: try reopened.snapshot(scope: scope), agentID: agentID).isEmpty)
            print("Two captured sources completed across reconnect/replay with exactly two durable terminal events.")
        } catch {
            do { try await deleteAgent(path: path, client: client) }
            catch { print("Context validation cleanup pending for agent \(agentID): \(error.localizedDescription)") }
            throw error
        }
        try await deleteAgent(path: path, client: client)
    }

    private func completedTurn(path: String, id: String, client: ManagedClient) async throws -> JSON {
        let deadline = ContinuousClock.now.advanced(by: .seconds(180))
        while ContinuousClock.now < deadline {
            let turn = try await client.json(path: path + "/turns/" + id)
            if ["completed", "failed", "cancelled"].contains(turn["state"].string) {
                guard turn["state"].string == "completed" else { throw DeliveryFailure(message: "Synthetic turn ended as " + turn["state"].string) }
                return turn
            }
            try await Task.sleep(for: .seconds(1))
        }
        throw DeliveryFailure(message: "Synthetic context turn did not complete within 180 seconds.")
    }

    private func deleteAgent(path: String, client: ManagedClient) async throws {
        for attempt in 0..<5 {
            do { _ = try await client.json(path: path, method: "DELETE"); return }
            catch APIError.http(404) { return }
            catch APIError.http(503) where attempt < 4 { try await Task.sleep(for: .seconds(2)) }
        }
    }
}

private struct DeliveryFailure: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}
