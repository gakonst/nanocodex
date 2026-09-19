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
        // Scroll the stacked notification display fully into view.
        springboard.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.65))
            .press(forDuration: 0.1, thenDragTo: springboard.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.35)))
    }
    private func capture(_ name: String) {
        let screenshot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        screenshot.name = name; screenshot.lifetime = .keepAlways; add(screenshot)
    }
    func testRunningThreadsStaySilentAndActivityLinksStillOpen() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["--demo"]
        app.launchEnvironment["NANOCODEX_DEMO_ACTIVITY"] = "1"
        let profile = "notifications-" + UUID().uuidString
        app.launchEnvironment["NANOCODEX_DEMO_PROFILE"] = profile
        app.launch()
        if springboard.alerts.buttons["Allow"].waitForExistence(timeout: 5) { springboard.alerts.buttons["Allow"].tap() }
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 20))
        openNotifications()
        let inbox = thread("Build the agent inbox"), data = thread("Tighten the fuel forecast")
        capture("agent-running-threads-stay-silent")
        XCTAssertFalse(inbox.waitForExistence(timeout: 5), springboard.debugDescription)
        XCTAssertFalse(data.exists, springboard.debugDescription)
        // Refreshing and backgrounding again must remain silent.
        app.activate()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 10))
        openNotifications()
        XCTAssertFalse(inbox.exists)
        XCTAssertFalse(data.exists)
        // Activity links remain valid independently of notification delivery.
        app.open(URL(string: "nanocodex://activity?account=demo.\(profile)&agent=data")!)
        XCTAssertTrue(app.buttons["conversation-title:data"].waitForExistence(timeout: 20))
        XCTAssertTrue(app.buttons["conversation-title:data"].isSelected)
        capture("agent-thread-notification-opened")
        // XCTest preserves demo configuration when launching an explicit URL.
        // A system cold-launch tap does not preserve test launch environment.
        app.terminate()
        app.open(URL(string: "nanocodex://activity?account=demo.\(profile)&agent=inbox")!)
        XCTAssertTrue(app.buttons["conversation-title:inbox"].waitForExistence(timeout: 20))
        XCTAssertTrue(app.buttons["conversation-title:inbox"].isSelected)
        openNotifications()
        XCTAssertFalse(inbox.exists)
        app.activate()
        app.terminate()
        app.launchEnvironment["NANOCODEX_DEMO_ACTIVITY"] = "0"
        app.launch()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 10))
        openNotifications()
        XCTAssertFalse(inbox.exists)
        XCTAssertFalse(data.exists)
        app.activate()
    }
}

extension AgentNotificationUITests {
    func testVoiceWidgetLinkOpensRecorderWithoutSendingInDemo() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["--demo"]
        app.launchEnvironment["NANOCODEX_DEMO_PROFILE"] = "quick-voice-" + UUID().uuidString
        app.launch()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 20))
        app.open(URL(string: "nanocodex://voice/new")!)
        XCTAssertTrue(app.staticTexts["New voice task"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.segmentedControls.buttons["English"].exists)
        XCTAssertTrue(app.segmentedControls.buttons["Ελληνικά"].exists)
        XCTAssertTrue(app.staticTexts["quickVoiceStatus"].label.contains("Sign in"))
        XCTAssertFalse(app.buttons["Send in new conversation"].isEnabled)
        app.buttons["Cancel"].tap()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 10))
    }
}
