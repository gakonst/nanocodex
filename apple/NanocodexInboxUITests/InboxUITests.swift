import XCTest

final class InboxUITests: XCTestCase {
    func testSwipeDraftAndSteerJourney() throws {
        let app = XCUIApplication()
        app.launchArguments = ["--demo"]
        app.launch()
        XCTAssertTrue(app.staticTexts["agent-title"].waitForExistence(timeout: 10))
        capture(app, "01-agent-inbox")
        let original = app.staticTexts["agent-title"].label
        let composer = app.textFields["composer"].exists ? app.textFields["composer"] : app.textViews["composer"]
        composer.tap(); composer.typeText("Check the reconnect boundary")
        capture(app, "02-agent-draft")
        app.buttons["later"].tap()
        XCTAssertNotEqual(app.staticTexts["agent-title"].label, original)
        app.buttons["Previous agent"].tap()
        XCTAssertEqual(app.staticTexts["agent-title"].label, original)
        XCTAssertEqual(composer.value as? String, "Check the reconnect boundary")
        capture(app, "03-draft-restored")
        app.buttons["send"].tap()
        XCTAssertTrue(app.staticTexts["notice"].label.contains("Demo action"))
        app.buttons["filter-Running"].tap()
        XCTAssertTrue(app.staticTexts["agent-title"].exists)
        app.buttons["Message mode and turn controls"].tap()
        app.buttons["Queue a follow-up"].tap()
        XCTAssertEqual(app.buttons["send"].label, "Send follow-up")
        app.buttons["Message mode and turn controls"].tap()
        app.buttons["Steer current turn"].tap()
        XCTAssertEqual(app.buttons["send"].label, "Send steering")
        app.buttons["Message mode and turn controls"].tap()
        app.buttons["Stop turn"].tap()
        XCTAssertTrue(app.buttons["Cancel"].waitForExistence(timeout: 3))
        app.buttons["Cancel"].tap()
        composer.tap(); composer.typeText("Prioritize reconnect and keep the UI minimal")
        XCTAssertTrue(app.staticTexts["agent-preview"].exists)
        XCTAssertGreaterThanOrEqual(app.otherElements["agent-card"].frame.minY, app.frame.minY + 44)
        XCTAssertLessThanOrEqual(app.buttons["send"].frame.maxY, app.keyboards.firstMatch.frame.minY)
        capture(app, "04-steer-running-agent")
        app.buttons["send"].tap()
        XCTAssertTrue(app.staticTexts["notice"].label.contains("Direction updated"))
        app.buttons["open-thread"].tap()
        XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 5))
        capture(app, "05-conversation")
        app.buttons["Done"].tap()
        #if os(iOS)
        let runningTitle = app.staticTexts["agent-title"].label
        app.otherElements["agent-card"].swipeLeft()
        XCTAssertNotEqual(app.staticTexts["agent-title"].label, runningTitle)
        #endif
        capture(app, "06-next-running-agent")
    }
    private func capture(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
