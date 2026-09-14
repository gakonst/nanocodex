import Foundation
import XCTest
import InboxCore

final class AttachmentIntegrationTests: XCTestCase {
    /// Uses the same native image preparation, disk storage, commands, and
    /// transcript projection as the app against an authenticated real agent.
    func testNativeImageSurvivesStorageReplayAndConversationReload() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["NANOCODEX_ATTACHMENT_LIVE"] == "1" else {
            throw XCTSkip("Set NANOCODEX_ATTACHMENT_LIVE=1 to run real attachment evidence.")
        }
        let key = try XCTUnwrap(environment["NC_API_KEY"], "A managed account credential is required.")
        let fixture = try XCTUnwrap(environment["NANOCODEX_ATTACHMENT_IMAGE"], "Provide the PNG fixture with heading ATTACHMENT CHECK 4827, red square, blue circle, and green triangle.")
        let deadline = ContinuousClock.now.advanced(by: .seconds(180))
        let credential = try AccountCredential(origin: environment["NANOCODEX_MANAGED_URL"] ?? "https://nanocodex.gakonst.workers.dev", apiKey: key)

        let prepared = try AttachmentPreparation.prepare(url: URL(fileURLWithPath: fixture))
        let scope = "attachment-live-" + UUID().uuidString
        let store = try AttachmentStore(scope: scope)
        try store.save(prepared)
        let storedURL = try store.url(for: prepared.attachment)
        defer {
            do {
                try store.remove(prepared.attachment)
                try FileManager.default.removeItem(at: storedURL.deletingLastPathComponent())
            } catch { XCTFail("Could not remove the test's local attachment: \(error.localizedDescription)") }
        }
        XCTAssertEqual(try Data(contentsOf: storedURL).count, prepared.attachment.byteCount)

        // Restore metadata and bytes independently, as a saved draft does after
        // launch. Only the small metadata reference lives in the draft JSON.
        let restoredMetadata = try JSONDecoder().decode(MessageAttachment.self, from: JSONEncoder().encode(prepared.attachment))
        let restoredContent = try AttachmentStore(scope: scope).content(for: [restoredMetadata])
        XCTAssertTrue(restoredContent == prepared.content, "Stored image bytes changed during restoration.")
        XCTAssertEqual(restoredContent.count, 1)
        XCTAssertEqual(restoredContent.first?["type"].string, "text")

        let client = ManagedClient(credential: credential)
        defer { client.close() }
        let agentID = try await client.create(requestID: "attachment-native-" + UUID().uuidString)
        print("Native attachment validation agent: \(agentID)")
        // XCTest awaits this cleanup even when a later assertion or request
        // fails. Its fresh client remains usable after the test closes its own.
        addTeardownBlock {
            let cleanup = ManagedClient(credential: credential)
            defer { cleanup.close() }
            try await Self.deleteAgent(agentID, client: cleanup)
        }

        let path = try await client.uploadAttachment(agentID: agentID, attachment: restoredMetadata, source: storedURL,
                                                     preview: store.previewURL(for: restoredMetadata))
        XCTAssertEqual(path, restoredMetadata.originalPath)
        let preview = try await client.attachmentPreview(agentID: agentID, attachmentID: restoredMetadata.id)
        XCTAssertEqual(preview, prepared.preview)
        var command = AgentCommand(agentID: agentID, input: "Use image tools to read the attached original image at its /brain path. Reply with its exact heading, then the three shapes from left to right, giving each color followed by its shape.", kind: .followUp)
        command.images = try restoredMetadata.originalContent(path: path)
        let accepted = try await client.command(command)
        XCTAssertEqual(accepted["turn_id"].string, command.requestID)
        let first = try await Self.completedTurn(command.requestID, agentID: agentID, client: client, deadline: deadline)
        let reply = first["terminal"]["final_message"].string
        for expected in ["attachment check 4827", "red square", "blue circle", "green triangle"] {
            XCTAssertTrue(reply.lowercased().contains(expected), "The real model did not read \(expected) from the prepared attachment. Reply: \(reply)")
        }
        print("Native prepared-image reply: \(reply)")
        client.close()

        let restoredClient = ManagedClient(credential: credential)
        defer { restoredClient.close() }
        let replay = try await restoredClient.command(command)
        XCTAssertEqual(replay["turn_id"].string, command.requestID)
        XCTAssertEqual(replay["accepted_cursor"].string, accepted["accepted_cursor"].string)
        XCTAssertEqual(replay["terminal_cursor"].string, first["terminal_cursor"].string)
        XCTAssertEqual(replay["state"].string, "completed")

        let history = try await restoredClient.history(agentID)
        XCTAssertFalse(history.hasMore)
        XCTAssertEqual(history.events.filter { $0.type == "turn_accepted" && $0.turnID == command.requestID }.count, 1)
        let rows = transcript(history.events)
        let user = try XCTUnwrap(rows.first { $0.role == "You" && $0.id.hasPrefix(command.requestID + ":") })
        XCTAssertEqual(user.text, command.input)
        XCTAssertEqual(user.imageFiles, [restoredMetadata], "Reloaded transcript lost the original image reference.")
        XCTAssertTrue(rows.contains { $0.role == "Agent" && $0.text == reply })

        let followUp = AgentCommand(agentID: agentID, input: "From the image in my previous message, which shape was in the middle and what four-digit number appeared in the heading? Reply concisely without tools.", kind: .followUp)
        _ = try await restoredClient.command(followUp)
        let second = try await Self.completedTurn(followUp.requestID, agentID: agentID, client: restoredClient, deadline: deadline)
        let remembered = second["terminal"]["final_message"].string
        for expected in ["blue", "circle", "4827"] {
            XCTAssertTrue(remembered.lowercased().contains(expected), "The real conversation did not retain the image. Reply: \(remembered)")
        }
        let reloaded = try await restoredClient.history(agentID)
        XCTAssertFalse(reloaded.hasMore)
        XCTAssertEqual(Set(reloaded.events.filter { $0.type == "turn_accepted" }.map(\.turnID)), Set([command.requestID, followUp.requestID]))
        XCTAssertEqual(reloaded.events.filter { $0.type == "turn_accepted" }.count, 2)
        XCTAssertTrue(transcript(reloaded.events).contains { $0.role == "Agent" && $0.text == remembered })
        print("Native retained-image follow-up: \(remembered); replay and restored transcript verified.")
    }

    private static func completedTurn(_ turnID: String, agentID: String, client: ManagedClient, deadline: ContinuousClock.Instant) async throws -> JSON {
        let path = try ManagedClient.agentPath(agentID) + "/turns/" + turnID
        while ContinuousClock.now < deadline {
            try Task.checkCancellation()
            let receipt = try await client.json(path: path)
            let state = receipt["state"].string
            if state == "completed" { return receipt }
            if state == "failed" || state == "cancelled" {
                throw AttachmentEvidenceFailure(message: "Real attachment turn ended with state \(state).")
            }
            try await Task.sleep(for: .milliseconds(750))
        }
        throw AttachmentEvidenceFailure(message: "Real attachment turns did not complete within 180 seconds.")
    }

    private static func deleteAgent(_ id: String, client: ManagedClient) async throws {
        let path = try ManagedClient.agentPath(id)
        for attempt in 0..<5 {
            do {
                _ = try await client.json(path: path, method: "DELETE")
                do {
                    _ = try await client.history(id)
                    throw AttachmentEvidenceFailure(message: "Deleted validation agent history is still accessible: " + id)
                } catch APIError.http(404) {
                    print("Native attachment validation agent deleted; history returns 404: \(id)")
                    return
                }
            } catch APIError.http(404) { return }
            catch APIError.http(503) where attempt < 4 { try await Task.sleep(for: .seconds(1)) }
        }
        throw AttachmentEvidenceFailure(message: "Validation agent cleanup remains pending: " + id)
    }
}

private struct AttachmentEvidenceFailure: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}
