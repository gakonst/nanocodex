import XCTest

final class InboxUITests: XCTestCase {
    private func launch(_ environment: [String: String] = [:]) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["--demo"]
        app.launchEnvironment = ["NANOCODEX_DEMO_COMPLETE_AFTER_MS": "120000", "NANOCODEX_DEMO_DELAY_MS": "600"].merging(environment) { _, new in new }
        app.launch()
        XCTAssertTrue(app.staticTexts["agent-title"].waitForExistence(timeout: 10))
        return app
    }
    private func composer(_ app: XCUIApplication) -> XCUIElement {
        app.textFields["composer"].exists ? app.textFields["composer"] : app.textViews["composer"]
    }
    private func selectInbox(_ app: XCUIApplication) {
        let browse = app.buttons["Browse agents"]
        let visible = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: browse)
        XCTAssertEqual(XCTWaiter.wait(for: [visible], timeout: 5), .completed)
        browse.tap()
        app.buttons.containing(.staticText, identifier: "Build the agent inbox").firstMatch.tap()
        XCTAssertTrue(app.staticTexts["agent-title"].waitForExistence(timeout: 3))
        XCTAssertEqual(app.staticTexts["agent-title"].label, "Build the agent inbox")
    }
    private func queue(_ app: XCUIApplication, _ text: String) {
        composer(app).tap(); composer(app).typeText(text)
        app.buttons["send"].tap()
    }
    private func gone(_ element: XCUIElement, timeout: TimeInterval = 8) {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: element)
        XCTAssertEqual(XCTWaiter.wait(for: [expectation], timeout: timeout), .completed)
    }
    private func thread(_ app: XCUIApplication, contains text: String) {
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 5))
        let conversation = app.scrollViews["conversation"]
        let message = conversation.staticTexts[text]
        if !message.isHittable { conversation.swipeUp() }
        XCTAssertTrue(message.waitForExistence(timeout: 5))
        XCTAssertEqual(conversation.staticTexts.matching(NSPredicate(format: "label == %@", text)).count, 1)
    }
    func testSwipeDraftAndSteerJourney() {
        let app = launch()
        capture(app, "01-agent-inbox")
        let original = app.staticTexts["agent-title"].label
        composer(app).tap(); composer(app).typeText("Check the reconnect boundary")
        capture(app, "02-agent-draft")
        app.otherElements["agent-card"].swipeLeft()
        XCTAssertNotEqual(app.staticTexts["agent-title"].label, original)
        app.otherElements["agent-card"].press(forDuration: 1)
        app.buttons["Previous agent"].tap()
        XCTAssertEqual(app.staticTexts["agent-title"].label, original)
        XCTAssertEqual(composer(app).value as? String, "Check the reconnect boundary")
        capture(app, "03-draft-restored")
        selectInbox(app)
        app.buttons["Stop turn"].tap()
        XCTAssertTrue(app.buttons["Cancel"].waitForExistence(timeout: 3)); app.buttons["Cancel"].tap()
        composer(app).tap(); composer(app).typeText("Prioritize reconnect and keep the UI minimal")
        XCTAssertTrue(app.staticTexts["agent-preview"].exists)
        XCTAssertLessThanOrEqual(app.buttons["send"].frame.maxY, app.keyboards.firstMatch.frame.minY)
        capture(app, "04-steer-running-agent")
        app.buttons["send"].tap()
        XCTAssertTrue(app.buttons["steer-now"].waitForExistence(timeout: 5))
        let pending = app.scrollViews["pending-messages"]
        let input = app.otherElements["composer-input"]
        XCTAssertLessThanOrEqual(pending.frame.maxY, input.frame.minY + 1)
        XCTAssertLessThanOrEqual(input.frame.minY - pending.frame.maxY, 2, "Queue touches the composer")
        capture(app, "07-queued-message")
        app.otherElements["agent-card"].swipeLeft()
        XCTAssertFalse(app.staticTexts["pending-message"].exists)
        selectInbox(app)
        thread(app, contains: "Prioritize reconnect and keep the UI minimal")
        capture(app, "05-conversation")
        app.buttons["Done"].tap()
        app.buttons["steer-now"].doubleTap()
        gone(app.staticTexts["pending-message"])
        thread(app, contains: "Prioritize reconnect and keep the UI minimal")
        app.buttons["Done"].tap()
        let current = app.staticTexts["agent-title"].label
        app.otherElements["agent-card"].swipeRight()
        XCTAssertNotEqual(app.staticTexts["agent-title"].label, current)
        capture(app, "06-next-running-agent")
    }
    func testFailedSubmissionRetainsMessageAndRetriesOnce() {
        let app = launch(["NANOCODEX_DEMO_FAIL_ONCE": "submit"])
        selectInbox(app); queue(app, "Retry only once")
        XCTAssertTrue(app.buttons["retry-pending"].waitForExistence(timeout: 5))
        app.otherElements["agent-card"].swipeLeft(); selectInbox(app)
        app.buttons["retry-pending"].doubleTap()
        XCTAssertTrue(app.buttons["steer-now"].waitForExistence(timeout: 5))
        thread(app, contains: "Retry only once")
    }
    func testFailedCancellationCanRetryWithoutDuplicateInput() {
        let app = launch(["NANOCODEX_DEMO_FAIL_ONCE": "cancel"])
        selectInbox(app); queue(app, "Keep the captured target")
        XCTAssertTrue(app.buttons["steer-now"].waitForExistence(timeout: 5))
        app.buttons["steer-now"].tap()
        XCTAssertTrue(app.staticTexts["Cancellation unconfirmed. The queued message is retained; try again."].waitForExistence(timeout: 5))
        app.buttons["steer-now"].tap(); gone(app.staticTexts["pending-message"])
        thread(app, contains: "Keep the captured target")
    }
    func testPendingSurvivesRelaunchAndCanBeCancelled() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        selectInbox(app); queue(app, "Survive restart")
        XCTAssertTrue(app.buttons["steer-now"].waitForExistence(timeout: 5))
        app.terminate(); app.launch()
        XCTAssertTrue(app.staticTexts["agent-title"].waitForExistence(timeout: 10))
        selectInbox(app)
        XCTAssertTrue(app.buttons["steer-now"].waitForExistence(timeout: 5))
        app.buttons["Cancel queued message"].tap(); gone(app.staticTexts["pending-message"])
        XCTAssertTrue(app.buttons["Stop turn"].exists, "Cancelling the queued message keeps its predecessor running")
        thread(app, contains: "Survive restart")
    }
    func testQueuedMessageStartsNaturallyWhileReadingThread() {
        let app = launch(["NANOCODEX_DEMO_COMPLETE_AFTER_MS": "2500"])
        selectInbox(app); queue(app, "Continue naturally")
        thread(app, contains: "Continue naturally")
        XCTAssertTrue(app.staticTexts["Working on: Continue naturally"].waitForExistence(timeout: 8))
        app.buttons["Done"].tap()
        XCTAssertEqual(app.staticTexts["agent-title"].label, "Build the agent inbox")
        XCTAssertFalse(app.staticTexts["pending-message"].exists)
    }
    func testCancellingFirstOfTwoQueuedMessagesKeepsSecondSteerable() {
        let app = launch(); selectInbox(app)
        queue(app, "First queued message")
        XCTAssertTrue(app.buttons["steer-now"].waitForExistence(timeout: 5))
        queue(app, "Second queued message")
        let cancel = app.buttons["Cancel queued message"].firstMatch
        let enabled = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: cancel)
        XCTAssertEqual(XCTWaiter.wait(for: [enabled], timeout: 5), .completed)
        cancel.tap()
        let one = XCTNSPredicateExpectation(predicate: NSPredicate(format: "count == 1"), object: app.staticTexts.matching(identifier: "pending-message"))
        XCTAssertEqual(XCTWaiter.wait(for: [one], timeout: 5), .completed)
        XCTAssertEqual(app.staticTexts["pending-message"].label, "Second queued message")
        app.buttons["steer-now"].tap(); gone(app.staticTexts["pending-message"])
        thread(app, contains: "Second queued message")
        XCTAssertTrue(app.staticTexts["Working on: Second queued message"].exists)
    }
    func testFinishedAgentStaysPinnedUntilThreadDismisses() {
        let app = launch(["NANOCODEX_DEMO_FINISH_IN_THREAD": "1"])
        selectInbox(app); app.buttons["filter-Running"].tap()
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 5))
        // Keep reading while the demo turn finishes.
        let title = app.navigationBars["Build the agent inbox"]
        XCTAssertTrue(title.waitForExistence(timeout: 5))
        app.scrollViews.firstMatch.swipeUp(); app.scrollViews.firstMatch.swipeDown()
        XCTAssertTrue(title.exists)
        app.buttons["Done"].tap()
        XCTAssertEqual(app.staticTexts["agent-title"].label, "Tighten the fuel forecast")
    }
    func testVoiceSlideDownPreservesAgentDraft() {
        let app = launch(); selectInbox(app)
        composer(app).tap(); composer(app).typeText("Existing draft.")
        app.buttons["Voice input"].tap()
        XCTAssertTrue(app.staticTexts["voice-transcript"].waitForExistence(timeout: 5))
        capture(app, "08-voice-input")
        let voiceHeader = app.staticTexts["Voice · Demo"].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        voiceHeader.press(forDuration: 0.05, thenDragTo: voiceHeader.withOffset(CGVector(dx: 0, dy: 260)))
        gone(app.staticTexts["voice-transcript"])
        XCTAssertEqual(composer(app).value as? String, "Existing draft.\nCheck reconnect first, then simplify the inbox")
        app.otherElements["agent-card"].swipeLeft(); selectInbox(app)
        XCTAssertTrue((composer(app).value as? String)?.contains("Check reconnect first") == true)
        app.buttons["send"].tap()
        XCTAssertTrue(app.buttons["steer-now"].waitForExistence(timeout: 5))
        capture(app, "09-voice-queued")
    }
    func testVoiceBackgroundSavesDraftWithoutDuplicatingOnDismiss() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        selectInbox(app); app.buttons["Voice input"].tap()
        XCTAssertTrue(app.staticTexts["voice-transcript"].waitForExistence(timeout: 5))
        XCUIDevice.shared.press(.home)
        app.activate()
        XCTAssertTrue(app.buttons["Use voice draft"].waitForExistence(timeout: 5))
        app.buttons["Use voice draft"].tap()
        XCTAssertEqual(composer(app).value as? String, "Check reconnect first, then simplify the inbox")
        app.terminate(); app.launch(); selectInbox(app)
        XCTAssertEqual(composer(app).value as? String, "Check reconnect first, then simplify the inbox")
    }
    func testVoiceUnavailableKeepsTypedDraft() {
        let app = launch(["NANOCODEX_DEMO_VOICE_ERROR": "1"])
        composer(app).tap(); composer(app).typeText("Keep my typing")
        app.buttons["Voice input"].tap()
        XCTAssertTrue(app.staticTexts["voice-error"].waitForExistence(timeout: 5))
        app.buttons["Use voice draft"].tap()
        XCTAssertEqual(composer(app).value as? String, "Keep my typing")
    }
    func testReadableToolActivityAndUnlabelledReplies() {
        let app = launch(); selectInbox(app)
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 5))
        let conversation = app.scrollViews["conversation"]
        XCTAssertFalse(conversation.staticTexts["nanocodex"].exists)
        XCTAssertFalse(conversation.staticTexts["exec_command"].exists)
        XCTAssertTrue(conversation.staticTexts["Run command"].exists)
        capture(app, "10-readable-activity")
        conversation.staticTexts["Run command"].tap()
        XCTAssertTrue(conversation.staticTexts["Command"].waitForExistence(timeout: 3))
        XCTAssertTrue(conversation.staticTexts["Exit code"].exists)
        XCTAssertTrue(conversation.staticTexts["swift test --package-path apple/InboxCore"].exists)
        capture(app, "11-activity-details")
    }
    func testToolFailureIsReadable() {
        let app = launch(["NANOCODEX_DEMO_TOOL_ERROR": "1"]); selectInbox(app)
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 5))
        let conversation = app.scrollViews["conversation"]
        XCTAssertTrue(conversation.staticTexts["Failed"].exists)
        conversation.staticTexts["Run command"].tap()
        XCTAssertTrue(conversation.staticTexts["The browser disconnected. Reconnect it and try again."].exists)
        capture(app, "12-activity-failure")
    }
    private func capture(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name; attachment.lifetime = .keepAlways; add(attachment)
    }
}
