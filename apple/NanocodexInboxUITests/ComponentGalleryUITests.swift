import XCTest

final class ComponentGalleryUITests: XCTestCase {
    private func launch() -> XCUIApplication {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["--component-gallery"]
        app.launch()
        XCTAssertTrue(app.textFields["gallery-composer"].waitForExistence(timeout: 10)
                      || app.textViews["gallery-composer"].waitForExistence(timeout: 5))
        return app
    }
    private func composer(_ app: XCUIApplication) -> XCUIElement {
        let field = app.textFields["gallery-composer"]
        return field.exists ? field : app.textViews["gallery-composer"]
    }
    private func capture(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name; attachment.lifetime = .keepAlways; add(attachment)
    }

    func testUnstyledAndCustomPresentationsShareState() {
        let app = launch()
        capture(app, "component-unstyled")
        composer(app).tap(); composer(app).typeText("Keep this opening-set draft")
        app.segmentedControls["gallery-style"].buttons["DJ Booth style"].tap()
        XCTAssertEqual(composer(app).value as? String, "Keep this opening-set draft")
        capture(app, "component-custom-style")
        app.buttons["gallery-conversations"].tap()
        XCTAssertTrue(app.buttons["gallery-row:crate"].waitForExistence(timeout: 5))
        capture(app, "component-custom-sidebar")
        app.buttons["gallery-row:crate"].tap()
        XCTAssertTrue(app.staticTexts["Late-night crate"].waitForExistence(timeout: 5))
        composer(app).tap(); composer(app).typeText("A separate late-night draft")
        capture(app, "component-independent-draft")
        app.buttons["gallery-conversations"].tap()
        app.buttons["gallery-row:booth"].tap()
        XCTAssertEqual(composer(app).value as? String, "Keep this opening-set draft")
        app.segmentedControls["gallery-style"].buttons["Unstyled"].tap()
        XCTAssertEqual(composer(app).value as? String, "Keep this opening-set draft")
        app.buttons["gallery-send"].tap()
        XCTAssertTrue(app.buttons["gallery-stop"].waitForExistence(timeout: 5))
        let finished = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: app.buttons["gallery-stop"])
        XCTAssertEqual(XCTWaiter.wait(for: [finished], timeout: 20), .completed)
        capture(app, "component-completed-reply")
    }

    func testPendingRetryHistoryAndStop() {
        let app = launch()
        app.buttons["gallery-earlier"].tap()
        XCTAssertTrue(app.buttons["gallery-latest"].waitForExistence(timeout: 5))
        capture(app, "component-older-history")
        app.buttons["gallery-latest"].tap()
        app.buttons["gallery-scenarios"].tap()
        XCTAssertTrue(app.buttons["Interrupt next send"].waitForExistence(timeout: 5))
        app.buttons["Interrupt next send"].tap()
        XCTAssertTrue(app.staticTexts["gallery-interruption-armed"].waitForExistence(timeout: 5))
        composer(app).tap(); composer(app).typeText("Bring in more percussion")
        app.buttons["gallery-send"].tap()
        XCTAssertTrue(app.buttons["gallery-retry"].waitForExistence(timeout: 5))
        capture(app, "component-pending-retry")
        app.buttons["gallery-retry"].tap()
        XCTAssertTrue(app.buttons["gallery-stop"].waitForExistence(timeout: 5))
        capture(app, "component-live-turn")
        app.buttons["gallery-stop"].tap()
        let stopped = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: app.buttons["gallery-stop"])
        XCTAssertEqual(XCTWaiter.wait(for: [stopped], timeout: 5), .completed)
        capture(app, "component-stopped-turn")
    }
}
