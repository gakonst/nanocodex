import XCTest
@testable import InboxCore

final class ModelSelectionTests: XCTestCase {
    private func state(_ values: [String: JSON] = [:]) -> JSON {
        .object(["agent_id": .string("fixture"), "latest_event_cursor": .string("10"),
                 "active_turns": .array([]), "settings": .object(["model": .string("gpt-6-astra"), "thinking": .string("low")])]
            .merging(values, uniquingKeysWith: { _, new in new }))
    }
    func testModelCatalogOffersOnlyImplementedEfforts() {
        XCTAssertEqual(Set(ModelChoice.all.map(\.id)).count, ModelChoice.all.count)
        XCTAssertEqual(ModelChoice.find("kimi-k3")?.efforts, ["low", "high"])
        XCTAssertEqual(ModelChoice.find("mimo-v2.6-pro")?.efforts, ["low", "medium", "high"])
        XCTAssertFalse(ModelChoice.find("gpt-6-astra")!.efforts.contains("none"))
    }
    func testPendingAutomaticRouteAndResolvedProvider() throws {
        var card = AgentCard(id: "fixture", title: "Fixture")
        try card.apply(state: state(["model_routing_enabled": .bool(true), "model_routing_automatic": .bool(true)]))
        XCTAssertTrue(card.routingAutomatic); XCTAssertFalse(card.modelLocked); XCTAssertEqual(card.provider, "")
        try card.apply(state: state(["model_routing_enabled": .bool(true), "model_routing_automatic": .bool(true),
            "model_route": .object(["model": .string("kimi-k3"), "backend": .string("vercel"), "thinking": .string("high")])]))
        XCTAssertEqual(card.model, "kimi-k3"); XCTAssertEqual(card.provider, "vercel"); XCTAssertEqual(card.thinking, "high")
        XCTAssertTrue(card.modelLocked); XCTAssertTrue(card.effortLocked)
    }
    func testNativeAstraEffortRemainsAvailableAfterModelLocks() throws {
        var card = AgentCard(id: "fixture", title: "Fixture")
        try card.apply(state: state(["accepted_turns": .number(1)]))
        XCTAssertTrue(card.modelLocked); XCTAssertFalse(card.effortLocked); XCTAssertEqual(card.provider, "ChatGPT")
        card.model = "gpt-5.6-sol"
        XCTAssertTrue(card.effortLocked)
    }
    func testAcceptedEventDoesNotUnlockBetweenCompletionAndStateRefresh() throws {
        var card = AgentCard(id: "fixture", title: "Fixture")
        try card.apply(state: state())
        card.apply(events: [try AgentEvent(.object(["type": .string("turn_accepted"), "cursor": .string("11"), "turn_id": .string("turn")]))])
        card.apply(events: [try AgentEvent(.object(["type": .string("turn_completed"), "cursor": .string("12"), "turn_id": .string("turn")]))])
        XCTAssertFalse(card.isRunning); XCTAssertTrue(card.modelLocked)
    }
}
