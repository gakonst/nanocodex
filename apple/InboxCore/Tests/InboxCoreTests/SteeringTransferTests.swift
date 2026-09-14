import Foundation
import XCTest
@testable import InboxCore

final class SteeringTransferTests: XCTestCase {
    private let transfer = SteeringTransfer(agentID: "agent", sourceTurnID: "queued-source", targetTurnID: "running-target")
    private var multimodalInput: JSON {
        .array([
            .object(["type": .string("text"), "text": .string("  Keep this\nexactly 🐝  ")]),
            .object(["type": .string("image"), "image_url": .string("https://example.invalid/original.png?x=1&y=2")]),
            .object(["type": .string("text"), "text": .string("Original video reference: /brain/attachments/original-video.mp4")])
        ])
    }

    func testSteerUsesStableSourceMessageIDAndCapturedCurrentTarget() throws {
        let command = transfer.command(input: .string("follow up"))
        let spec = try command.requestSpec()
        XCTAssertEqual(command.requestID, "queued-source")
        XCTAssertEqual(command.turnID, "running-target")
        XCTAssertEqual(spec.path, "/v1/agents/agent/turns/running%2Dtarget/steer")
        XCTAssertEqual(spec.body, .object(["input": .string("follow up"), "message_id": .string("queued-source")]))
        XCTAssertNil(spec.key)
        XCTAssertEqual(transfer.command(input: .string("follow up")), command)
    }

    func testRawMultimodalInputIsPreservedExactlyAndOverridesConvenienceFields() throws {
        var command = AgentCommand(agentID: "agent", turnID: "running-target", input: "must not replace original", kind: .steer, requestID: "queued-source")
        command.images = [.string("must not append")]
        command.rawInput = multimodalInput
        XCTAssertEqual(try command.requestSpec().body, .object([
            "input": multimodalInput, "message_id": .string("queued-source")
        ]))
        XCTAssertEqual(try transfer.command(input: multimodalInput).requestSpec().body, try command.requestSpec().body)
    }

    func testCancellationFencesQueuedSourceAndWithdrawalUsesOriginalSteerIdentity() throws {
        let cancel = try transfer.sourceCancellation.requestSpec()
        XCTAssertEqual(transfer.sourceCancellation.turnID, "queued-source")
        XCTAssertEqual(cancel.path, "/v1/agents/agent/turns/queued%2Dsource/cancel")
        XCTAssertNil(cancel.body)
        XCTAssertNil(cancel.key)
        let withdrawal = transfer.withdrawal
        XCTAssertEqual(withdrawal.turnID, transfer.command(input: multimodalInput).turnID)
        XCTAssertEqual(withdrawal.requestID, "queued-source")
        let spec = try withdrawal.requestSpec()
        XCTAssertEqual(spec.path, "/v1/agents/agent/turns/running%2Dtarget/withdraw-steer")
        XCTAssertEqual(spec.body, .object(["message_id": .string("queued-source")]))
        XCTAssertNil(spec.key)
    }

    func testSendingRestoreBecomesUnconfirmedAndNeverAutomaticallyResumes() throws {
        var value = transfer
        value.phase = .sending
        value = try JSONDecoder().decode(SteeringTransfer.self, from: JSONEncoder().encode(value))
        value.restore()
        XCTAssertEqual(value.phase, .unconfirmed)
        XCTAssertFalse(value.canResume)
        XCTAssertNotNil(value.error)
        let restored = value
        value.restore()
        XCTAssertEqual(value, restored)
        XCTAssertFalse(value.canResume)
        XCTAssertEqual(value.command(input: multimodalInput).requestID, "queued-source")
    }

    func testSafePreparationAndWithdrawalPhasesResumeAfterRestore() throws {
        for phase: SteeringTransfer.Phase in [.preparing, .removingQueued, .ready, .withdrawing] {
            var value = transfer
            value.phase = phase
            value = try JSONDecoder().decode(SteeringTransfer.self, from: JSONEncoder().encode(value))
            value.restore()
            XCTAssertEqual(value.phase, phase)
            XCTAssertTrue(value.canResume, "\(phase)")
        }
        for phase: SteeringTransfer.Phase in [.sending, .accepted, .withdrawn, .unconfirmed] {
            var value = transfer
            value.phase = phase
            XCTAssertFalse(value.canResume, "\(phase)")
        }
    }

    func testCodableRoundTripRetainsIdentityPhaseAndWithdrawalIntent() throws {
        for phase: SteeringTransfer.Phase in [.preparing, .removingQueued, .ready, .sending, .accepted, .withdrawing, .withdrawn, .unconfirmed] {
            var value = transfer
            value.phase = phase
            value.error = "Response unavailable"
            value.withdrawRequested = true
            value.wasAccepted = true
            let decoded = try JSONDecoder().decode(SteeringTransfer.self, from: JSONEncoder().encode(value))
            XCTAssertEqual(decoded, value)
            XCTAssertEqual(decoded.id, "queued-source")
            XCTAssertEqual(decoded.command(input: multimodalInput), value.command(input: multimodalInput))
            XCTAssertEqual(decoded.withdrawal, value.withdrawal)
        }
    }

    func testReceiptsCannotConfirmAnotherTurnOrInferWithdrawal() {
        XCTAssertTrue(transfer.sourceIsFenced(.object(["turn_id": .string("queued-source"), "state": .string("cancelling")])))
        XCTAssertFalse(transfer.sourceIsFenced(.object(["turn_id": .string("running-target"), "state": .string("cancelling")])))
        XCTAssertFalse(transfer.sourceIsFenced(.object(["turn_id": .string("queued-source"), "state": .string("completed")])))
        XCTAssertTrue(transfer.isAccepted(.object(["turn_id": .string("running-target"), "state": .string("steering")])))
        XCTAssertFalse(transfer.isAccepted(.object(["turn_id": .string("queued-source"), "state": .string("steering")])))
        XCTAssertNil(transfer.withdrawalResult(.object(["turn_id": .string("running-target"), "message_id": .string("queued-source")])))
        XCTAssertNil(transfer.withdrawalResult(.object(["turn_id": .string("running-target"), "message_id": .string("other"), "withdrawn": .bool(true)])))
        XCTAssertEqual(transfer.withdrawalResult(.object(["turn_id": .string("running-target"), "message_id": .string("queued-source"), "withdrawn": .bool(false)])), false)
    }

    func testHTTPCommandsSendExactBodiesAndDistinctSourceAndTargetPaths() async throws {
        let payload = multimodalInput
        var paths: [String] = []
        let fixture = try HTTPFixture { request in
            paths.append(request.path)
            XCTAssertEqual(request.method, "POST")
            XCTAssertEqual(request.headers["authorization"], "Bearer " + fixtureKey)
            XCTAssertNil(request.headers["idempotency-key"])
            switch request.path {
            case "/v1/agents/agent/turns/queued-source/cancel":
                XCTAssertTrue(request.body.isEmpty)
            case "/v1/agents/agent/turns/running-target/steer":
                XCTAssertEqual(try? JSONDecoder().decode(JSON.self, from: request.body), .object([
                    "input": payload, "message_id": .string("queued-source")
                ]))
            case "/v1/agents/agent/turns/running-target/withdraw-steer":
                XCTAssertEqual(try? JSONDecoder().decode(JSON.self, from: request.body), .object([
                    "message_id": .string("queued-source")
                ]))
            default: XCTFail("Unexpected request: \(request.path)")
            }
            return .init(body: "{}")
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        _ = try await client.command(transfer.sourceCancellation)
        _ = try await client.command(transfer.command(input: payload))
        _ = try await client.command(transfer.withdrawal)
        XCTAssertEqual(paths, [
            "/v1/agents/agent/turns/queued-source/cancel",
            "/v1/agents/agent/turns/running-target/steer",
            "/v1/agents/agent/turns/running-target/withdraw-steer"
        ])
    }
}
