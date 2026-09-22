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
        XCTAssertNotNil(ModelChoice.find("gpt-6-sol"))
        XCTAssertNotNil(ModelChoice.find("gpt-6-luna"))
        XCTAssertNil(ModelChoice.find("gpt-5.6-sol"))
        XCTAssertNil(ModelChoice.find("gpt-5.6-luna"))
        XCTAssertNil(ModelChoice.find("gpt-5.6-terra"))
        XCTAssertTrue(ModelChoice.find("gpt-6-sol")!.efforts.contains("none"))
        XCTAssertTrue(ModelChoice.find("gpt-6-luna")!.efforts.contains("none"))
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
        card.model = "gpt-6-sol"
        XCTAssertTrue(card.effortLocked)
    }

    func testPinnedLegacyModelRemainsExactButIsNotOffered() throws {
        var card = AgentCard(id: "fixture", title: "Fixture")
        try card.apply(state: state([
            "accepted_turns": .number(1),
            "settings": .object(["model": .string("gpt-5.6-sol"), "thinking": .string("high")]),
        ]))
        XCTAssertEqual(card.model, "gpt-5.6-sol")
        XCTAssertTrue(card.modelLocked)
        XCTAssertNil(ModelChoice.find(card.model))
    }
    func testAcceptedEventDoesNotUnlockBetweenCompletionAndStateRefresh() throws {
        var card = AgentCard(id: "fixture", title: "Fixture")
        try card.apply(state: state())
        card.apply(events: [try AgentEvent(.object(["type": .string("turn_accepted"), "cursor": .string("11"), "turn_id": .string("turn")]))])
        card.apply(events: [try AgentEvent(.object(["type": .string("turn_completed"), "cursor": .string("12"), "turn_id": .string("turn")]))])
        XCTAssertFalse(card.isRunning); XCTAssertTrue(card.modelLocked)
    }
    private var route: JSON { .object(["model": .string("gpt-6-sol"), "backend": .string("cloudflare"), "thinking": .string("high")]) }
    private func event(_ type: String, _ cursor: String, _ extra: [String: JSON] = [:]) throws -> AgentEvent {
        try AgentEvent(.object(["type": .string(type), "cursor": .string(cursor), "turn_id": .string("turn")]
            .merging(extra, uniquingKeysWith: { _, new in new })))
    }
    func testLiveRoutePublishesActualProviderWithoutStatePollAndSurvivesReplay() throws {
        var card = AgentCard(id: "fixture", title: "Fixture")
        try card.apply(state: state(["model_routing_enabled": .bool(true), "model_routing_automatic": .bool(true)]))
        let selected = try event("event", "12", ["event": .object(["type": .string("run.started"), "payload": .object([:])]), "model_route": route, "model_routing_automatic": .bool(true)])
        card.apply(events: [try event("turn_accepted", "11"), selected])
        XCTAssertEqual(card.provider, "cloudflare"); XCTAssertEqual(card.model, "gpt-6-sol")
        XCTAssertEqual(card.thinking, "high"); XCTAssertTrue(card.modelPinned); XCTAssertTrue(card.routingAutomatic)
        XCTAssertTrue(card.isRunning)
        card.apply(events: [try event("turn_failed", "13", ["error": .string("Responses invalid provider stream")])])
        XCTAssertFalse(card.isRunning); XCTAssertEqual(card.status, "Failed"); XCTAssertTrue(card.modelLocked)
        // Neither delayed state nor route replay reintroduces a Stop button.
        try card.apply(state: state(["latest_event_cursor": .string("11"), "active_turns": .array([.string("turn")])]))
        card.apply(events: [selected])
        XCTAssertFalse(card.isRunning); XCTAssertEqual(card.provider, "cloudflare"); XCTAssertEqual(card.status, "Failed")
    }
    func testRouteFromDelayedStateIsAppliedWithoutResurrectingFailedTurn() throws {
        var card = AgentCard(id: "fixture", title: "Fixture")
        try card.apply(state: state())
        card.apply(events: [try event("turn_accepted", "11"), try event("turn_failed", "13")])
        try card.apply(state: state(["latest_event_cursor": .string("12"), "active_turns": .array([.string("turn")]),
            "model_route": route, "model_routing_enabled": .bool(true), "model_routing_automatic": .bool(true)]))
        XCTAssertEqual(card.provider, "cloudflare"); XCTAssertEqual(card.model, "gpt-6-sol")
        XCTAssertFalse(card.isRunning); XCTAssertEqual(card.status, "Failed"); XCTAssertEqual(card.stateCursor.rawValue, "13")
    }
    func testReplayedRouteIsAppliedAfterNewerStateAndCannotReplaceNewerRouteMetadata() throws {
        var card = AgentCard(id: "fixture", title: "Fixture")
        try card.apply(state: state(["latest_event_cursor": .string("15"), "model_routing_enabled": .bool(true)]))
        card.apply(events: [try event("event", "12", ["event": .object(["type": .string("run.started"), "payload": .object([:])]), "model_route": route])])
        XCTAssertEqual(card.provider, "cloudflare"); XCTAssertFalse(card.routingAutomatic)
        try card.apply(state: state(["latest_event_cursor": .string("16"), "model_route": route, "model_routing_automatic": .bool(true)]))
        card.apply(events: [try event("event", "12", ["event": .object(["type": .string("run.started"), "payload": .object([:])]), "model_route": route])])
        XCTAssertTrue(card.routingAutomatic)
    }

}
