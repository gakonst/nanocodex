import XCTest
import InboxCore
import NanocodexContext
@testable import NanocodexHand

final class HandTests: XCTestCase {
    private func directory() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("native-hand-test-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        addTeardownBlock { try FileManager.default.removeItem(at: root) }
        return root
    }
    func testWorkspaceFilesAreBoundedAndAccountIsolated() async throws {
        let root = try directory()
        let one = try HandWorkspace(id: "phone-one", name: "iPhone", root: root.appendingPathComponent("one"))
        let two = try HandWorkspace(id: "phone-two", name: "iPhone", root: root.appendingPathComponent("two"))
        _ = try await one.call(name: "write_file", input: .object(["path": .string("notes/hello.txt"), "content": .string("Hello from this iPhone")]))
        let read = try await one.call(name: "read_file", input: .object(["path": .string("/workspace/notes/hello.txt")]))
        XCTAssertEqual(read["content"].string, "Hello from this iPhone")
        do { _ = try await two.call(name: "read_file", input: .object(["path": .string("notes/hello.txt")])); XCTFail("Cross-account read succeeded") } catch { }
        let listing = try await one.call(name: "list_files", input: .object(["path": .string(".")]))
        XCTAssertEqual(listing["entries"].array.first?["name"].string, "notes")
        for path in ["../two/secret", "/etc/passwd", "notes/../../secret", "/workspace/../secret"] {
            do { _ = try await one.call(name: "write_file", input: .object(["path": .string(path), "content": .string("bad")])); XCTFail("Escaping path accepted: " + path) } catch { }
        }
        try FileManager.default.createSymbolicLink(atPath: root.appendingPathComponent("one/link").path, withDestinationPath: root.appendingPathComponent("two").path)
        do { _ = try await one.call(name: "write_file", input: .object(["path": .string("link/secret"), "content": .string("bad")])); XCTFail("Symlink escape accepted") } catch { }
        let large = String(repeating: "x", count: 1024 * 1024) + " full file"
        _ = try await one.call(name: "write_file", input: .object(["path": .string("large"), "content": .string(large)]))
        let complete = try await one.call(name: "read_file", input: .object(["path": .string("large")]))
        XCTAssertEqual(complete["content"].string, large)
        for index in 0..<225 {
            _ = try await one.call(name: "write_file", input: .object(["path": .string("many/file-\(index)"), "content": .string("entry")]))
        }
        let all = try await one.call(name: "list_files", input: .object(["path": .string("many")]))
        XCTAssertEqual(all["entries"].array.count, 225)
        XCTAssertEqual(all["has_more"], .bool(false))
    }
    func testCatalogAdvertisesOnlyImplementedDeviceTools() throws {
        let hand = try HandWorkspace(id: "phone-one", name: "iPhone", root: directory())
        XCTAssertEqual(hand.catalog["attachment_id"].string, "phone-one")
        XCTAssertEqual(hand.catalog["machines"].array.first?["workspace"].string, "/workspace")
        XCTAssertEqual(Set(hand.catalog["tools"].array.map { $0["definition"]["name"].string }), Set(["device_info", "list_files", "read_file", "write_file"]))
    }
    func testMessageToolsQueryCapturedSourcesAndFenceAccountChanges() async throws {
        let root = try directory(), store = ContextStore(directory: try directory())
        try store.activate("account-a"); try store.setEnabled(true, scope: "account-a")
        let date = Date(timeIntervalSince1970: 1_800_000_000)
        for source in ["Messages", "WhatsApp", "Instagram", "Signal"] {
            try store.capture([.init(source: source, text: "Friday plans from \(source)", sender: "Alex", thread: "Weekend", occurredAt: date)], scope: "account-a")
        }
        let hand = try HandWorkspace(id: "phone-messages", name: "iPhone", root: root, messageContext: ContextQuery(store: store, scope: "account-a"))
        let names = Set(hand.catalog["tools"].array.map { $0["definition"]["name"].string })
        XCTAssertTrue(Set(["message_sources", "search_messages", "read_message"]).isSubset(of: names))
        for source in ["iMessage", "WhatsApp", "Instagram", "Signal"] {
            let result = try await hand.call(name: "search_messages", input: .object([
                "source": .string(source), "query": .string("Friday"), "sender": .string("alex"),
                "conversation": .string("week"), "after": .string("2027-01-01T00:00:00Z")
            ]))
            XCTAssertEqual(result["messages"].array.count, 1)
            let id = try XCTUnwrap(result["messages"].array.first?["id"].string)
            let read = try await hand.call(name: "read_message", input: .object(["id": .string(id)]))
            XCTAssertTrue(read["text"].string.contains("Friday plans"))
            XCTAssertEqual(read["sender"].string, "Alex")
        }
        let page = try await hand.call(name: "search_messages", input: .object(["limit": .number(1)]))
        XCTAssertEqual(page["messages"].array.count, 1)
        let next = try await hand.call(name: "search_messages", input: .object(["limit": .number(1), "cursor": page["nextCursor"]]))
        XCTAssertNotEqual(next["messages"].array.first?["id"], page["messages"].array.first?["id"])
        for input in [JSON.object(["limit": .number(1.5)]), .object(["after": .string("yesterday")]), .object(["account": .string("account-b")])] {
            do { _ = try await hand.call(name: "search_messages", input: input); XCTFail("Invalid query accepted") } catch { }
        }
        try store.setEnabled(false, scope: "account-a")
        let disabled = try await hand.call(name: "message_sources", input: .object([:]))
        XCTAssertTrue(disabled["sources"].array.allSatisfy { $0["count"].number == 0 })
        do { _ = try await hand.call(name: "search_messages", input: .object([:])); XCTFail("Disabled capture exposed messages") } catch { }
        try store.activate("account-b"); try store.setEnabled(true, scope: "account-b")
        try store.capture([.init(source: "Signal", text: "Private to B")], scope: "account-b")
        do { _ = try await hand.call(name: "read_message", input: .object(["id": page["messages"].array[0]["id"]])); XCTFail("Old account remained queryable") } catch { }
        let other = try HandWorkspace(id: "phone-b", name: "iPhone", root: root.appendingPathComponent("b"), messageContext: ContextQuery(store: store, scope: "account-b"))
        let isolated = try await other.call(name: "search_messages", input: .object([:]))
        XCTAssertEqual(isolated["messages"].array.count, 1)
        XCTAssertEqual(isolated["messages"].array.first?["excerpt"].string, "Private to B")
    }
    @MainActor
    func testDuplicateCallsReuseReceiptAndExpiredCallsNeverWrite() async throws {
        let root = try directory()
        let workspace = try HandWorkspace(id: "phone-one", name: "iPhone", root: root)
        let credential = try AccountCredential(origin: "https://example.invalid", apiKey: "ncx_live_" + String(repeating: "a", count: 12) + "_" + String(repeating: "b", count: 43))
        let session = try HandSession(credential: credential, workspace: workspace)
        defer { session.close() }
        func call(_ id: String, content: String, deadline: Double) -> JSON {
            .object(["type": .string("call"), "call_id": .string(id), "session_id": .string("session"), "model": .string("test"), "name": .string("write_file"), "input": .object(["path": .string("receipt.txt"), "content": .string(content)]), "deadline_at": .number(deadline), "output_byte_budget": .number(16_384), "output_token_budget": .number(4096)])
        }
        let deadline = Date().timeIntervalSince1970 * 1000 + 10_000
        let frame = call("one", content: "first", deadline: deadline)
        let first = try await session.invoke(frame)
        try Data("changed locally".utf8).write(to: root.appendingPathComponent("receipt.txt"))
        let replay = try await session.invoke(frame)
        XCTAssertEqual(replay, first)
        XCTAssertEqual(try String(contentsOf: root.appendingPathComponent("receipt.txt"), encoding: .utf8), "changed locally")
        do { _ = try await session.invoke(call("one", content: "different", deadline: deadline)); XCTFail("Conflicting reuse accepted") } catch { }
        let expired = try await session.invoke(call("two", content: "late", deadline: 1))
        XCTAssertEqual(expired["outcome"]["status"].string, "unavailable")
        XCTAssertEqual(try String(contentsOf: root.appendingPathComponent("receipt.txt"), encoding: .utf8), "changed locally")

        // A write that ran must never be reported as unavailable/pre-admission
        // merely because its response cannot fit the broker's byte budget.
        guard case .object(var small) = call("three", content: "written", deadline: deadline) else { return XCTFail() }
        small["output_byte_budget"] = .number(1)
        let bounded = try await session.invoke(.object(small))
        XCTAssertEqual(bounded["outcome"]["status"].string, "ambiguous")
        XCTAssertEqual(try String(contentsOf: root.appendingPathComponent("receipt.txt"), encoding: .utf8), "written")
        session.stop(); session.start(); session.stop()
        let afterPause = try await session.invoke(frame)
        XCTAssertEqual(afterPause, first, "Suspension must preserve the original call receipt")
        session.close()
        do { _ = try await session.invoke(call("closed", content: "wrong account", deadline: deadline)); XCTFail("A closed account executed a call") } catch { }
        XCTAssertEqual(try String(contentsOf: root.appendingPathComponent("receipt.txt"), encoding: .utf8), "written")
    }
}
