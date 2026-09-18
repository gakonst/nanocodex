import XCTest

final class ComponentGalleryUITests: XCTestCase {
    private func launch(_ arguments: [String] = []) -> XCUIApplication {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["--component-gallery"] + arguments
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
        app.segmentedControls["gallery-style"].buttons["Nanocodex"].tap()
        XCTAssertEqual(composer(app).value as? String, "Keep this opening-set draft")
        capture(app, "component-custom-style")
        app.buttons["gallery-conversations"].tap()
        XCTAssertTrue(app.buttons["gallery-row:crate"].waitForExistence(timeout: 5))
        capture(app, "component-custom-sidebar")
        app.buttons["gallery-row:crate"].tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Late-night crate")).firstMatch.waitForExistence(timeout: 5))
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

    func testNanocodexNativeComposerAndNavigation() {
        let app = launch(["--component-native-style"])
        capture(app, "nanocodex-light")
        app.buttons["gallery-conversations"].tap()
        XCTAssertTrue(app.buttons["gallery-row:crate"].waitForExistence(timeout: 5))
        capture(app, "nanocodex-sidebar")
        app.buttons["gallery-row:booth"].tap()
        app.buttons["add-attachments"].tap()
        XCTAssertTrue(app.navigationBars["Add to conversation"].waitForExistence(timeout: 5))
        capture(app, "nanocodex-attachments")
        app.buttons["Done"].tap()
        composer(app).tap(); composer(app).typeText("Keep the next transition gentle")
        capture(app, "nanocodex-composer")
        app.buttons["gallery-send"].tap()
        XCTAssertTrue(app.buttons["gallery-stop"].waitForExistence(timeout: 5))
        capture(app, "nanocodex-live")
        app.buttons["gallery-stop"].tap()
        let stopped = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: app.buttons["gallery-stop"])
        XCTAssertEqual(XCTWaiter.wait(for: [stopped], timeout: 5), .completed)
        capture(app, "nanocodex-stopped")
        composer(app).tap()
        composer(app).typeText(String(repeating: "A longer draft to refine the transitions and keep the room moving.\n", count: 8))
        XCTAssertTrue(app.buttons["expand-composer"].waitForExistence(timeout: 5))
        app.buttons["expand-composer"].tap()
        XCTAssertTrue(app.textViews["expanded-composer"].waitForExistence(timeout: 5))
        capture(app, "nanocodex-expanded-composer")
        app.buttons["collapse-composer"].tap()
        XCTAssertTrue((composer(app).value as? String)?.contains("A longer draft") == true)
        app.buttons["gallery-scenarios"].tap()
        XCTAssertTrue(app.buttons["Interrupt next send"].waitForExistence(timeout: 5))
        app.buttons["Interrupt next send"].tap()
        app.buttons["gallery-send"].tap()
        XCTAssertTrue(app.buttons["gallery-retry"].waitForExistence(timeout: 5))
        XCTAssertTrue(composer(app).isHittable, "Long pending input must leave the editor visible")
        XCTAssertLessThan(composer(app).frame.maxY, app.frame.maxY)
        XCTAssertTrue(app.buttons["gallery-conversations"].isHittable)
        capture(app, "nanocodex-long-pending")
        app.buttons["gallery-retry"].tap()
        XCTAssertTrue(app.buttons["gallery-stop"].waitForExistence(timeout: 5))
        let completed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: app.buttons["gallery-stop"])
        XCTAssertEqual(XCTWaiter.wait(for: [completed], timeout: 20), .completed)
        let reply = app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "The same conversation state works")).firstMatch
        XCTAssertTrue(reply.waitForExistence(timeout: 5))
        XCTAssertTrue(reply.isHittable, "New replies follow the latest message after sending a long draft")
        capture(app, "nanocodex-completed-reply")
    }

    func testNanocodexDarkPresentation() {
        let app = launch(["--component-native-style", "--component-dark"])
        capture(app, "nanocodex-dark")
        composer(app).tap(); composer(app).typeText("Keep this draft in dark mode")
        capture(app, "nanocodex-dark-composer")
        app.buttons["gallery-conversations"].tap()
        XCTAssertTrue(app.buttons["gallery-row:crate"].waitForExistence(timeout: 5))
        capture(app, "nanocodex-dark-sidebar")
    }

}
