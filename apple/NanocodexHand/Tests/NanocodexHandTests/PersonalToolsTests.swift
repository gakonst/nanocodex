import XCTest
import InboxCore
@testable import NanocodexHand

final class PersonalToolsTests: XCTestCase {
    func testPaginationRejectsMalformedAndCrossFilterCursors() throws {
        let original = try PersonalToolRequest(["query": .string("Alex")], allowed: ["query", "cursor", "limit"])
        let cursor = try original.cursor(500)
        let continued = try PersonalToolRequest(["query": .string("Alex"), "cursor": cursor, "limit": .number(50)], allowed: ["query", "cursor", "limit"])
        XCTAssertEqual(try continued.offset(), 500)
        let changed = try PersonalToolRequest(["query": .string("Someone else"), "cursor": cursor], allowed: ["query", "cursor"])
        XCTAssertThrowsError(try changed.offset())
        for invalid in ["not base64", "e30=", "", String(repeating: "x", count: 8193)] {
            XCTAssertThrowsError(try PersonalToolRequest(["cursor": .string(invalid)], allowed: ["cursor"]).offset())
        }
        XCTAssertThrowsError(try original.cursor(1_000_001))
        let controls = try PersonalToolRequest(["query": .string("a\nb")], allowed: ["query"])
        XCTAssertThrowsError(try controls.text("query"))
        XCTAssertThrowsError(try PersonalToolRequest(["query": .string(String(repeating: "\u{01}", count: 1024))], allowed: ["query"]))
        // Long but valid queries must still produce usable cursors.
        let long = String(repeating: "a", count: 1024)
        let start = try PersonalToolRequest(["query": .string(long)], allowed: ["query"])
        let next = try PersonalToolRequest(["query": .string(long), "cursor": start.cursor(50)], allowed: ["query", "cursor"])
        XCTAssertEqual(try next.offset(), 50)
    }
    func testBoundsTypesAndUnknownFieldsAreStrict() throws {
        XCTAssertThrowsError(try PersonalToolRequest(["write": .bool(true)], allowed: ["query"]))
        for number in [-1.0, 0, 1.5, 51, .infinity, .nan] {
            let request = try PersonalToolRequest(["limit": .number(number)], allowed: ["limit"])
            XCTAssertThrowsError(try request.integer("limit", default: 20, range: 1...50))
        }
        let request = try PersonalToolRequest(["favorite": .string("true"), "after": .string("yesterday"), "id": .number(5)], allowed: ["favorite", "after", "id"])
        XCTAssertThrowsError(try request.boolean("favorite"))
        XCTAssertThrowsError(try request.date("after"))
        XCTAssertThrowsError(try request.text("id", required: true))
        XCTAssertThrowsError(try request.text("missing", required: true))
        let valid = try PersonalToolRequest(["after": .string("2026-09-19T12:00:00.123Z"), "favorite": .bool(false)], allowed: ["after", "favorite"])
        XCTAssertNotNil(try valid.date("after"))
        XCTAssertEqual(try valid.boolean("favorite"), false)
    }
    func testLocationFreshnessAccuracyDowngradeAndWireSchema() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        func fix(age: Double = 0, accuracy: Double = 15, approximate: Bool = false) -> HandLocationSnapshot {
            HandLocationSnapshot(latitude: 45, longitude: 20, timestamp: now.addingTimeInterval(-age), horizontalAccuracy: accuracy, approximate: approximate)
        }
        XCTAssertTrue(fix().isUsable(now: now, maxAge: 30, reducedAccuracy: false))
        XCTAssertFalse(fix(age: 31).isUsable(now: now, maxAge: 30, reducedAccuracy: false))
        XCTAssertFalse(fix(age: -6).isUsable(now: now, maxAge: 30, reducedAccuracy: false))
        XCTAssertFalse(fix(accuracy: -1).isUsable(now: now, maxAge: 30, reducedAccuracy: false))
        XCTAssertFalse(fix().isUsable(now: now, maxAge: 30, reducedAccuracy: true))
        XCTAssertFalse(fix(approximate: true).isUsable(now: now, maxAge: 30, reducedAccuracy: true))
        XCTAssertTrue(fix(accuracy: 1000, approximate: true).isUsable(now: now, maxAge: 30, reducedAccuracy: true))
        XCTAssertEqual(fix().json["timestamp_ms"], .number(1_800_000_000_000))
        XCTAssertEqual(fix().json["accuracy_meters"], .number(15))
        XCTAssertEqual(fix().json["approximate"], .bool(false))
    }
    func testToolCatalogMatchesDispatchAndBounds() {
        let catalog = HandPersonalTools.catalog { name, _, properties, required in
            .object(["name": .string(name), "properties": .object(properties), "required": .array(required.map(JSON.string))])
        }
        XCTAssertEqual(Set(catalog.map { $0["name"].string }), HandPersonalTools.names)
        XCTAssertEqual(catalog[0]["properties"]["limit"]["maximum"], .number(50))
        XCTAssertEqual(catalog[3]["properties"]["timeoutSeconds"]["maximum"], .number(10))
        XCTAssertEqual(catalog[2]["required"], .array([.string("id")]))
    }
}
