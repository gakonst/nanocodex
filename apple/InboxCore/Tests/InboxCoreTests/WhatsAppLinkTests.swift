import Foundation
import XCTest
@testable import InboxCore

final class WhatsAppLinkTests: XCTestCase {
    private func fields(_ value: JSON) -> [String: JSON] {
        if case .object(let fields) = value { return fields }
        return [:]
    }
    private let operation = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    private func hint(expiry: Double = Date().addingTimeInterval(290).timeIntervalSince1970 * 1000) -> JSON {
        .object(["ok": .bool(true), "type": .string("whatsapp_link"), "status": .string("input_required"),
                 "connector": .string("whatsapp"), "agent_id": .string("agent_1"), "operation_id": .string(operation), "expires_at": .number(expiry),
                 "phase": .string("ready"), "message": .string("Continue privately on your phone.")])
    }
    private func status(_ link: WhatsAppLink, phase: String = "ready", connected: Bool = false, operation: String? = nil) -> String {
        JSON.object(["connected": .bool(connected), "attempt": .object([
            "operation_id": .string(operation ?? link.operationID), "expires_at": .number(link.expiresAt), "state": .string(phase)
        ])]).pretty
    }
    private func code(_ link: WhatsAppLink, operation: String? = nil, expiry: Double? = nil, alias: String = "ABCD-1234") -> String {
        JSON.object(["operation_id": .string(operation ?? link.operationID), "expires_at": .number(expiry ?? link.expiresAt),
                     "code": .string("ABCD-1234"), "pairing_code": .string(alias)]).pretty
    }

    @MainActor func testPrivateHTTPJourneyResumesSameAttemptThenSendsOnlyVerifiedSafeReceipt() async throws {
        let link = try XCTUnwrap(WhatsAppLink.parse(hint()))
        let capture = WhatsAppRequests()
        let fixture = try HTTPFixture { request in
            let index = capture.append(request)
            if request.path.hasSuffix("/pairing") { return FixtureReply(body: self.code(link)) }
            return FixtureReply(body: self.status(link, phase: index >= 4 ? "paired" : "ready", connected: index >= 4))
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey))
        defer { client.close() }
        let account = UUID()
        let controller = WhatsAppLinkController(link: link, account: account)
        controller.activate(account: account)
        await controller.refresh(client: client, account: account, configuration: fixture.configuration)
        XCTAssertEqual(controller.phase, .ready); XCTAssertEqual(controller.code, "ABCD-1234")
        XCTAssertNil(controller.safeReceipt)
        controller.suspend()
        XCTAssertNil(controller.code); XCTAssertFalse(controller.shouldPoll)
        await controller.refresh(client: client, account: account, configuration: fixture.configuration)
        XCTAssertEqual(capture.count, 2)
        controller.activate(account: account)
        await controller.refresh(client: client, account: account, configuration: fixture.configuration)
        XCTAssertEqual(controller.code, "ABCD-1234")
        await controller.refresh(client: client, account: account, configuration: fixture.configuration)
        XCTAssertEqual(controller.phase, .connected); XCTAssertNil(controller.code); XCTAssertFalse(controller.shouldPoll)
        let receipt = try XCTUnwrap(controller.safeReceipt)
        XCTAssertEqual(receipt["status"].string, "connected")
        XCTAssertNotNil(BrowserReceiptPresentation.summary(receipt.pretty))
        XCTAssertEqual(receipt["operation_id"].string, operation)
        XCTAssertFalse(receipt.pretty.contains("ABCD")); XCTAssertEqual(Set(fields(receipt).keys), ["type", "status", "connector", "operation_id"])
        await controller.refresh(client: client, account: account, configuration: fixture.configuration)
        XCTAssertEqual(capture.count, 5)
        for request in capture.snapshot() {
            XCTAssertEqual(request.method, "GET"); XCTAssertTrue(request.body.isEmpty)
            XCTAssertEqual(request.headers["authorization"], "Bearer \(fixtureKey)")
            XCTAssertEqual(request.headers["cache-control"], "no-store")
            if request.path.hasSuffix("/pairing") { XCTAssertEqual(request.query, "operation_id=\(operation)") }
        }
        let cached = await client.cachedJSON(path: "/v1/connectors/whatsapp/pairing?operation_id=" + operation)
        XCTAssertNil(cached)
    }

    @MainActor func testUncertainStartReconcilesAuthoritativeExpiryWithoutCreatingNewAttempt() async throws {
        var provisional = fields(hint())
        provisional["phase"] = .string("unknown")
        provisional["operation_id"] = .string(operation.uppercased())
        let link = try XCTUnwrap(WhatsAppLink.parse(.object(provisional)))
        XCTAssertEqual(link.operationID, operation.uppercased())
        var authoritative = provisional
        authoritative["expires_at"] = .number(link.expiresAt - 5000)
        let actual = try XCTUnwrap(WhatsAppLink.parse(.object(authoritative)))
        let capture = WhatsAppRequests()
        let fixture = try HTTPFixture { request in
            let index = capture.append(request)
            if request.path.hasSuffix("/pairing") { return FixtureReply(body: self.code(actual)) }
            return FixtureReply(body: self.status(actual, phase: index == 0 ? "unknown" : index >= 3 ? "paired" : "ready", connected: index >= 3))
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey))
        defer { client.close() }
        let controller = WhatsAppLinkController(link: link, account: UUID())
        controller.activate(account: controller.account)
        await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration)
        XCTAssertEqual(controller.phase, .unknown); XCTAssertTrue(controller.shouldPoll)
        XCTAssertEqual(controller.expiresAt, actual.expiresAt); XCTAssertNil(controller.code)
        controller.suspend(); controller.activate(account: controller.account)
        await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration)
        XCTAssertEqual(controller.code, "ABCD-1234")
        await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration)
        XCTAssertEqual(controller.phase, .connected); XCTAssertNil(controller.code)
        XCTAssertTrue(capture.snapshot().allSatisfy { $0.method == "GET" })
        XCTAssertEqual(capture.snapshot().filter { $0.path.hasSuffix("/pairing") }.first?.query, "operation_id=" + operation.uppercased())
    }

    @MainActor func testMalformedAndCrossAttemptHTTPResponsesFailClosedAndStop() async throws {
        let link = try XCTUnwrap(WhatsAppLink.parse(hint()))
        let other = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        let journeys: [(String, String, Int)] = [
            (status(link, operation: other), code(link), 1),
            (status(link), code(link, operation: other), 2),
            (status(link), code(link, expiry: link.expiresAt + 1), 2),
            (status(link), code(link, alias: "DIFFERENT"), 2),
            (status(link), "{\"code\":\"ABCD-1234\"}", 2),
            (status(link, phase: "paired", connected: false), code(link), 1),
            (status(link, phase: "ready", connected: true), code(link), 1)
        ]
        for (statusBody, codeBody, expected) in journeys {
            let capture = WhatsAppRequests()
            let fixture = try HTTPFixture { request in
                _ = capture.append(request)
                return FixtureReply(body: request.path.hasSuffix("/pairing") ? codeBody : statusBody)
            }
            let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey))
            let account = UUID(), controller = WhatsAppLinkController(link: link, account: UUID())
            // Use the bound account for each independent journey.
            controller.activate(account: controller.account)
            await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration)
            XCTAssertEqual(controller.phase, .unavailable); XCTAssertNil(controller.code); XCTAssertNil(controller.safeReceipt)
            await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration)
            XCTAssertEqual(capture.count, expected)
            controller.activate(account: account)
            XCTAssertFalse(controller.shouldPoll)
            client.close(); fixture.close()
        }
    }

    @MainActor func testExpiryUnknownAndAccountSwitchNeverRequestAnotherCode() async throws {
        for phase in ["expired"] {
            let link = try XCTUnwrap(WhatsAppLink.parse(hint()))
            let capture = WhatsAppRequests()
            let fixture = try HTTPFixture { request in _ = capture.append(request); return FixtureReply(body: self.status(link, phase: phase)) }
            let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey))
            let controller = WhatsAppLinkController(link: link, account: UUID())
            controller.activate(account: controller.account)
            await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration)
            XCTAssertFalse(controller.shouldPoll); XCTAssertNil(controller.code); XCTAssertNil(controller.safeReceipt)
            await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration)
            XCTAssertEqual(capture.count, 1)
            client.close(); fixture.close()
        }
        let link = try XCTUnwrap(WhatsAppLink.parse(hint()))
        let requested = expectation(description: "Private code request in flight")
        let gate = DispatchGroup(); gate.enter()
        let fixture = try HTTPFixture { request in
            if request.path.hasSuffix("/pairing") { requested.fulfill(); return FixtureReply(body: self.code(link), gate: gate) }
            return FixtureReply(body: self.status(link))
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey))
        defer { client.close() }
        let controller = WhatsAppLinkController(link: link, account: UUID())
        controller.activate(account: controller.account)
        let pending = Task { await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration) }
        await fulfillment(of: [requested], timeout: 5)
        controller.activate(account: UUID()) // account switch invalidates the in-flight private reply
        gate.leave(); await pending.value
        XCTAssertEqual(controller.phase, .cancelled); XCTAssertNil(controller.code); XCTAssertNil(controller.safeReceipt)
        let expired = WhatsAppLinkController(link: link, account: UUID())
        expired.activate(account: expired.account)
        expired.expire(now: Date(timeIntervalSince1970: link.expiresAt / 1000))
        XCTAssertEqual(expired.phase, .expired); XCTAssertNil(expired.code)
    }

    @MainActor func testReturningAfterCodeDeadlineReconcilesConnectionWithoutFetchingCode() async throws {
        let link = try XCTUnwrap(WhatsAppLink.parse(hint(expiry: Date().addingTimeInterval(-1).timeIntervalSince1970 * 1000)))
        for paired in [false, true] {
            let capture = WhatsAppRequests()
            let fixture = try HTTPFixture { request in
                _ = capture.append(request)
                return FixtureReply(body: self.status(link, phase: paired ? "paired" : "expired", connected: paired))
            }
            let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey))
            let controller = WhatsAppLinkController(link: link, account: UUID())
            controller.activate(account: controller.account)
            await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration)
            XCTAssertEqual(controller.phase, paired ? .connected : .expired)
            XCTAssertNil(controller.code); XCTAssertFalse(controller.shouldPoll)
            XCTAssertEqual(controller.safeReceipt != nil, paired)
            await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration)
            XCTAssertEqual(capture.count, 1)
            XCTAssertEqual(capture.snapshot().first?.path, "/v1/connectors/whatsapp")
            client.close(); fixture.close()
        }
    }

    @MainActor func testInlineRecoveryAndReappearanceOnlyReadTheOriginalAttempt() async throws {
        for failure in [503, 400] {
            let link = try XCTUnwrap(WhatsAppLink.parse(hint()))
            let capture = WhatsAppRequests()
            let fixture = try HTTPFixture { request in
                let index = capture.append(request)
                if index == 0 { return FixtureReply(status: failure, body: "{}") }
                if request.path.hasSuffix("/pairing") { return FixtureReply(body: self.code(link)) }
                return FixtureReply(body: self.status(link, phase: index >= 5 ? "paired" : "ready", connected: index >= 5))
            }
            defer { fixture.close() }
            let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey))
            defer { client.close() }
            let controller = WhatsAppLinkController(link: link, account: UUID())
            controller.activate(account: controller.account)
            await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration)
            XCTAssertEqual(controller.phase, failure == 503 ? .retrying : .unavailable)
            XCTAssertEqual(controller.shouldPoll, failure == 503)
            XCTAssertNil(controller.code); XCTAssertNil(controller.safeReceipt)

            // Returning to the card or checking an unavailable result uses the same operation.
            controller.suspend()
            await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration)
            XCTAssertEqual(capture.count, 1)
            controller.activate(account: controller.account)
            await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration)
            XCTAssertEqual(controller.phase, .ready); XCTAssertEqual(controller.code, "ABCD-1234")
            controller.suspend()
            XCTAssertNil(controller.code)
            controller.activate(account: controller.account)
            await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration)
            XCTAssertEqual(controller.code, "ABCD-1234")
            await controller.refresh(client: client, account: controller.account, configuration: fixture.configuration)
            XCTAssertEqual(controller.phase, .connected); XCTAssertNil(controller.code)
            XCTAssertEqual(controller.safeReceipt?["operation_id"].string, operation)
            XCTAssertEqual(capture.count, 6)
            for request in capture.snapshot() {
                XCTAssertEqual(request.method, "GET"); XCTAssertTrue(request.body.isEmpty)
                XCTAssertTrue(["/v1/connectors/whatsapp", "/v1/connectors/whatsapp/pairing"].contains(request.path))
                if request.path.hasSuffix("/pairing") { XCTAssertEqual(request.query, "operation_id=" + operation) }
            }
        }
    }

    func testRealTranscriptEventsRequireAccountConnectorAttributionIncludingNestedCodeMode() throws {
        let safe = hint()
        func event(_ cursor: Int, _ type: String, _ call: String, _ tool: String, metadata: JSON = .null, result: JSON = .null) throws -> AgentEvent {
            try AgentEvent(.object(["cursor": .string(String(cursor)), "type": .string("event"), "turn_id": .string("synthetic-turn"),
                "event": .object(["type": .string(type), "payload": .object(["call_id": .string(call), "tool": .string(tool),
                    "metadata": metadata, "arguments": .object(["operation": .string("connect"), "connector": .string("whatsapp")]), "result": result])])]))
        }
        for nestedFirst in [false, true] {
            let meta: JSON = .object(["tool_name": .string("account_connectors")])
            let events = try [event(1, "tool.call", "outer", "functions.exec"), event(2, "tool.call", "nested", "user_synthetic", metadata: meta)]
            let nested = try event(nestedFirst ? 3 : 4, "tool.result", "nested", "user_synthetic", metadata: meta, result: safe)
            let outer = try event(nestedFirst ? 4 : 3, "tool.result", "outer", "functions.exec", result: safe)
            let rows = transcript(events + (nestedFirst ? [nested, outer] : [outer, nested]))
            XCTAssertEqual(rows.compactMap { $0.tool?.whatsAppLink }.count, 1)
            XCTAssertNil(rows.first { $0.tool?.title == "Run code" }?.tool?.whatsAppLink)
        }
        for name in ["exec", "read_file", "web", "mcp__evil__account_connectors", "user_account_connectors"] {
            var tool = ToolPresentation(name: name, arguments: .null); tool.finish(safe)
            XCTAssertNil(tool.whatsAppLink)
        }
        var tool = ToolPresentation(name: "account_connectors", arguments: .null)
        tool.finish(.object(["content": .array([.object(["type": .string("text"), "text": .string(safe.pretty)])])]))
        XCTAssertNotNil(tool.whatsAppLink)
        let persisted = try JSONEncoder().encode(tool)
        XCTAssertFalse(String(decoding: persisted, as: UTF8.self).contains("ABCD-1234"))
        XCTAssertNil(WhatsAppLink.parse(hint(expiry: 1e300)))
        var missingAgent = fields(safe); missingAgent.removeValue(forKey: "agent_id")
        XCTAssertNil(WhatsAppLink.parse(.object(missingAgent)))
        var malformed = fields(safe); malformed["code"] = .string("ABCD-1234")
        tool.finish(.object(malformed)); XCTAssertNil(tool.whatsAppLink)
        tool.finish(safe, failed: true); XCTAssertNil(tool.whatsAppLink)
    }
}

private final class WhatsAppRequests: @unchecked Sendable {
    private let lock = NSLock()
    private var requests: [FixtureRequest] = []
    func append(_ request: FixtureRequest) -> Int { lock.lock(); defer { lock.unlock() }; let index = requests.count; requests.append(request); return index }
    var count: Int { snapshot().count }
    func snapshot() -> [FixtureRequest] { lock.lock(); defer { lock.unlock() }; return requests }
}
