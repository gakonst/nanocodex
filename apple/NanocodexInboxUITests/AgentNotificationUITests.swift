import XCTest

final class AgentNotificationUITests: XCTestCase {
    private let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
    private func thread(_ title: String) -> XCUIElement {
        springboard.buttons.matching(NSPredicate(format: "identifier == %@ AND label CONTAINS %@", "ShortLook.Platter.Content.Seamless", title)).firstMatch
    }
    private func openNotifications() {
        XCUIDevice.shared.press(.home)
        springboard.coordinate(withNormalizedOffset: CGVector(dx: 0.1, dy: 0.001))
            .press(forDuration: 0.1, thenDragTo: springboard.coordinate(withNormalizedOffset: CGVector(dx: 0.1, dy: 0.8)))
        // Expand iOS's stacked notification display before acting on a card.
        springboard.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.65))
            .press(forDuration: 0.1, thenDragTo: springboard.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.35)))
    }
    private func capture(_ name: String) {
        let screenshot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        screenshot.name = name; screenshot.lifetime = .keepAlways; add(screenshot)
    }
    func testSeparateRunningThreadsSwipeClearAndColdLink() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["--demo"]
        app.launchEnvironment["NANOCODEX_DEMO_ACTIVITY"] = "1"
        let profile = "notifications-" + UUID().uuidString
        app.launchEnvironment["NANOCODEX_DEMO_PROFILE"] = profile
        app.launch()
        if springboard.alerts.buttons["Allow"].waitForExistence(timeout: 5) { springboard.alerts.buttons["Allow"].tap() }
        XCTAssertTrue(app.buttons["tab-overview"].waitForExistence(timeout: 20))
        openNotifications()
        let inbox = thread("Build the agent inbox"), data = thread("Tighten the fuel forecast")
        capture("agent-thread-notifications-initial")
        XCTAssertTrue(inbox.waitForExistence(timeout: 10), springboard.debugDescription)
        XCTAssertTrue(data.waitForExistence(timeout: 10), springboard.debugDescription)
        capture("agent-thread-notifications")
        let frame = inbox.frame
        springboard.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: frame.maxX - 20, dy: frame.midY))
            .press(forDuration: 0.1, thenDragTo: springboard.coordinate(withNormalizedOffset: .zero)
                .withOffset(CGVector(dx: frame.minX + 20, dy: frame.midY)))
        if springboard.buttons["Clear"].waitForExistence(timeout: 2) { springboard.buttons["Clear"].tap() }
        XCTAssertFalse(inbox.exists, springboard.debugDescription)
        XCTAssertTrue(data.exists)
        // A foreground refresh must not reinsert the cleared thread.
        app.activate()
        XCTAssertTrue(app.buttons["tab-overview"].waitForExistence(timeout: 10))
        openNotifications()
        XCTAssertFalse(inbox.exists)
        XCTAssertTrue(data.exists)
        // iOS 26's simulator drops synthesized notification taps. Exercise
        // the exact destination here; native activation is checked manually.
        app.open(URL(string: "nanocodex://activity?account=demo.\(profile)&agent=data")!)
        XCTAssertTrue(app.buttons["browser-tab:data"].waitForExistence(timeout: 20))
        XCTAssertTrue(app.buttons["browser-tab:data"].isSelected)
        capture("agent-thread-notification-opened")
        // XCTest preserves demo configuration when launching an explicit URL.
        // A system cold-launch tap does not preserve test launch environment.
        app.terminate()
        app.open(URL(string: "nanocodex://activity?account=demo.\(profile)&agent=inbox")!)
        XCTAssertTrue(app.buttons["browser-tab:inbox"].waitForExistence(timeout: 20))
        XCTAssertTrue(app.buttons["browser-tab:inbox"].isSelected)
        openNotifications()
        XCTAssertFalse(inbox.exists)
        app.activate()
        app.terminate()
        app.launchEnvironment["NANOCODEX_DEMO_ACTIVITY"] = "0"
        app.launch()
        XCTAssertTrue(app.buttons["tab-overview"].waitForExistence(timeout: 10))
        openNotifications()
        XCTAssertFalse(inbox.exists)
        XCTAssertFalse(data.exists)
        app.activate()
    }
}
