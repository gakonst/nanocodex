import Foundation
import XCTest
import InboxCore

final class TurnControlIntegrationTests: XCTestCase {
    func testPreAdmissionStopFencesLateSubmissionAndReplay() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["NANOCODEX_TURN_CONTROL_LIVE"] == "1" else {
            throw XCTSkip("Set NANOCODEX_TURN_CONTROL_LIVE=1 with NC_API_KEY for real cancellation evidence.")
        }
        let credential = try AccountCredential(origin: environment["NANOCODEX_MANAGED_URL"] ?? "https://nanocodex.gakonst.workers.dev", apiKey: XCTUnwrap(environment["NC_API_KEY"]))
        let client = ManagedClient(credential: credential)
        defer { client.close() }
        let agentID = try await client.create(requestID: "turn-control-" + UUID().uuidString)
        print("Turn control validation agent: " + agentID)
        addTeardownBlock {
            let cleanup = ManagedClient(credential: credential)
            defer { cleanup.close() }
            for attempt in 0..<5 {
                do {
                    _ = try await cleanup.json(path: ManagedClient.agentPath(agentID), method: "DELETE")
                    return
                } catch APIError.http(404) { return }
                catch APIError.http(503) where attempt < 4 { try await Task.sleep(for: .seconds(1)) }
            }
        }
        let submission = AgentCommand(agentID: agentID, input: "Cancellation fence verification. This input must never execute.", kind: .followUp)
        let stop = AgentCommand(agentID: agentID, turnID: submission.requestID, kind: .stop)
        for _ in 0..<2 {
            let receipt = try await client.command(stop)
            XCTAssertEqual(receipt["turn_id"].string, submission.requestID)
            XCTAssertEqual(receipt["state"].string, "cancelling")
        }
        do {
            _ = try await client.turn(agentID: agentID, turnID: submission.requestID)
            XCTFail("A cancellation fence must not invent an admitted turn")
        } catch APIError.http(404) { }
        let late = try await client.command(submission)
        XCTAssertEqual(late["turn_id"].string, submission.requestID)
        let deadline = ContinuousClock.now.advanced(by: .seconds(60))
        var terminal: JSON = .null
        while ContinuousClock.now < deadline {
            terminal = try await client.turn(agentID: agentID, turnID: submission.requestID)
            if ["cancelled", "completed", "failed"].contains(terminal["state"].string) { break }
            try await Task.sleep(for: .milliseconds(500))
        }
        XCTAssertEqual(terminal["state"].string, "cancelled")
        let replay = try await client.command(submission)
        XCTAssertEqual(replay["state"].string, "cancelled")
        XCTAssertEqual(replay["terminal_cursor"].string, terminal["terminal_cursor"].string)
        let repeatedStop = try await client.command(stop)
        XCTAssertEqual(repeatedStop["state"].string, "cancelled")
        let history = try await client.history(agentID)
        XCTAssertFalse(history.hasMore)
        XCTAssertEqual(history.events.filter { $0.type == "turn_accepted" }.count, 1)
        XCTAssertEqual(history.events.filter { $0.type == "turn_cancelled" }.count, 1)
        let runtimeTypes = history.events.filter { $0.type == "event" && $0.turnID == submission.requestID }.map { $0.data["event"]["type"].string }
        print("Fenced turn runtime events: " + runtimeTypes.joined(separator: ", "))
        XCTAssertFalse(runtimeTypes.contains { ["assistant.delta", "assistant.message", "tool.call"].contains($0) }, "A stopped submission must never produce output or execute tools")
        print("Pre-admission cancellation, late POST, duplicate POST, and repeated Stop confirmed.")
    }
}
