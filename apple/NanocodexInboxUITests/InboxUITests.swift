import XCTest

final class InboxUITests: XCTestCase {
    override func setUp() { super.setUp(); continueAfterFailure = false }

    @MainActor
    func testLiveTerminalProgressAndFailure() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard let key = environment["NANOCODEX_LIVE_TEST_API_KEY"], !key.isEmpty else {
            throw XCTSkip("Requires an explicitly supplied managed account key and a live service.")
        }
        let origin = environment["NANOCODEX_LIVE_TEST_ORIGIN"] ?? "https://nanocodex.gakonst.workers.dev"
        func request(_ path: String, body: [String: Any]? = nil) async throws -> [String: Any] {
            var request = URLRequest(url: URL(string: origin + path)!)
            request.httpMethod = "POST"
            request.setValue("Bearer " + key, forHTTPHeaderField: "Authorization")
            request.setValue("Nanocodex-E12-iPhone", forHTTPHeaderField: "User-Agent")
            if let body {
                request.setValue("application/json", forHTTPHeaderField: "Content-Type")
                request.httpBody = try JSONSerialization.data(withJSONObject: body)
            }
            let (data, response) = try await URLSession.shared.data(for: request)
            XCTAssertTrue((200..<300).contains((response as? HTTPURLResponse)?.statusCode ?? 0))
            return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        }
        let app = XCUIApplication()
        app.launch()
        func require(_ condition: Bool, _ message: String) throws {
            guard condition else {
                capture(app, "E12-failure")
                XCTFail(message)
                throw NSError(domain: "E12", code: 1, userInfo: [NSLocalizedDescriptionKey: message])
            }
        }
        let credential = app.secureTextFields["Account API key"]
        if credential.waitForExistence(timeout: 5) {
            credential.tap(); credential.typeText(key)
            app.buttons["Connect account"].tap()
        }
        try require(app.buttons["Browse agents"].waitForExistence(timeout: 30), "The inbox did not connect")
        let created = try await request("/v1/agents")
        let agentID = try XCTUnwrap(created["agent_id"] as? String)
        let marker = "E12 progress " + String(UUID().uuidString.prefix(8))
        print("E12_LIVE_AGENT \(agentID) \(marker)")
        _ = try await request("/v1/agents/\(agentID)/turns", body: [
            "id": UUID().uuidString.lowercased(),
            "input": "\(marker). In one Cloudflare Linux sandbox, run exactly: printf 'E12_START\\n'; sleep 60; printf 'E12_MID\\n'; sleep 60; printf 'E12_DONE\\n'. Use exec_command with yield_time_ms 1000, then poll the same process until exit. Do not use a connected device."
        ])
        func openThread() throws {
            app.terminate(); app.launch()
            try require(app.buttons["Browse agents"].waitForExistence(timeout: 30), "The inbox did not connect")
            let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: app.buttons["Refresh agents"])
            try require(XCTWaiter.wait(for: [ready], timeout: 30) == .completed, "The inbox did not finish loading")
            let title = app.staticTexts["agent-title"]
            if !title.label.contains(marker) {
                app.buttons["Browse agents"].tap()
                let listing = app.collectionViews.firstMatch
                if !listing.waitForExistence(timeout: 5) {
                    app.buttons["Browse agents"].tap()
                }
                try require(listing.waitForExistence(timeout: 5), "The agent list did not open")
                let entry = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", marker)).firstMatch
                for _ in 0..<30 {
                    if entry.exists && entry.isHittable { break }
                    listing.swipeUp(velocity: .fast)
                }
                guard entry.exists && entry.isHittable else {
                    capture(app, "E12-agent-selection-failure")
                    XCTFail("The created live agent was not listed")
                    throw NSError(domain: "E12", code: 1)
                }
                entry.tap()
            }
            title.tap()
            try require(app.scrollViews["conversation"].waitForExistence(timeout: 5), "The conversation did not open")
        }
        func command(_ text: String) -> XCUIElement {
            app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Run command,' AND label CONTAINS %@", text)).firstMatch
        }
        try openThread()
        let success = command("E12_START")
        try require(success.waitForExistence(timeout: 90), "The command card did not appear")
        try require(success.label.contains("Running"), "The yielded command must remain Running")
        success.tap()
        try require(app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", "E12_START\n")).firstMatch.waitForExistence(timeout: 10), "Initial command output was not visible")
        capture(app, "E12-live-running")
        try openThread()
        try require(command("E12_START").waitForExistence(timeout: 15), "The command was lost after relaunch")
        try require(command("E12_START").label.contains("Running"), "Running status was lost after relaunch")
        let completed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS 'Completed'"), object: command("E12_START"))
        await fulfillment(of: [completed], timeout: 180)
        try require(command("E12_START").label.contains("Completed"), "The command did not complete")
        command("E12_START").tap()
        try require(app.staticTexts["E12_START\nE12_MID\nE12_DONE\n"].waitForExistence(timeout: 5), "Polling output was not folded into the original command")
        try require(app.staticTexts["Elapsed (seconds)"].exists, "Final elapsed time was missing")
        capture(app, "E12-live-completed")
        _ = try await request("/v1/agents/\(agentID)/turns", body: [
            "id": UUID().uuidString.lowercased(),
            "input": "In the same sandbox, run exactly: printf 'E12_FAIL_START\\n'; sleep 30; for i in $(seq 1 300); do printf 'E12_STDOUT_%04d\\n' \"$i\"; printf 'E12_STDERR_%04d\\n' \"$i\" >&2; done; printf 'E12_EXPECTED_FAILURE\\n' >&2; exit 7. Use exec_command with yield_time_ms 1000 and max_output_tokens 10000, then poll until exit. This is an intentional failure fixture; do not retry or fix it."
        ])
        try openThread()
        let failed = command("E12_FAIL_START")
        try require(failed.waitForExistence(timeout: 60), "The failure fixture command did not appear")
        try require(failed.label.contains("Running") || failed.label.contains("Failed"), "The failure fixture reported an unexpected status")
        let failure = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS 'Failed'"), object: failed)
        await fulfillment(of: [failure], timeout: 90)
        try require(failed.label.contains("Failed"), "Nonzero exit did not produce Failed status")
        try openThread()
        try require(command("E12_FAIL_START").waitForExistence(timeout: 15) && command("E12_FAIL_START").label.contains("Failed"), "Failed status was lost after relaunch")
        command("E12_FAIL_START").tap()
        let output = app.staticTexts.matching(NSPredicate(format: "label CONTAINS 'E12_STDOUT_0300' AND label CONTAINS 'E12_STDERR_0300' AND label ENDSWITH %@", "E12_EXPECTED_FAILURE\n")).firstMatch
        try require(output.waitForExistence(timeout: 5), "Large stdout/stderr lost their final output")
        try require(app.staticTexts["7"].exists, "The final exit code was not retained")
        try require(app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Command progress,'")).count == 0, "Empty polls created separate cards")
        capture(app, "E12-live-failed-after-relaunch")
    }
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
        let attached = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            input.frame.minY - pending.frame.maxY <= 2 && pending.frame.maxY <= input.frame.minY + 1
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [attached], timeout: 5), .completed)
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
        let next = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", "Tighten the fuel forecast"), object: app.staticTexts["agent-title"])
        XCTAssertEqual(XCTWaiter.wait(for: [next], timeout: 5), .completed)
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
        XCTAssertTrue(conversation.staticTexts.matching(identifier: "swift test --package-path apple/InboxCore").firstMatch.exists)
        capture(app, "11-activity-details")
    }
    func testToolFailureIsReadable() {
        let app = launch(["NANOCODEX_DEMO_TOOL_ERROR": "1"]); selectInbox(app)
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 5))
        let conversation = app.scrollViews["conversation"]
        XCTAssertTrue(conversation.staticTexts["Failed"].exists)
        conversation.staticTexts["Run command"].tap()
        let failure = conversation.staticTexts["The browser disconnected. Reconnect it and try again."]
        let visible = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: failure)
        XCTAssertEqual(XCTWaiter.wait(for: [visible], timeout: 5), .completed)
        capture(app, "12-activity-failure")
    }
    func testLongThreadKeepsPlaceAcrossUpdatesHistoryAndForeground() {
        let app = launch(["NANOCODEX_DEMO_LONG_THREAD": "1"]); selectInbox(app)
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 5))
        let conversation = app.scrollViews["conversation"]
        let last = conversation.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", "Progress note 36.")).firstMatch
        XCTAssertTrue(last.waitForExistence(timeout: 5)); XCTAssertTrue(last.isHittable, "Open the thread at its latest messages")
        conversation.swipeDown(); conversation.swipeDown()
        let anchor = conversation.staticTexts.allElementsBoundByIndex.first { $0.isHittable && $0.label.hasPrefix("Progress note ") }!
        let label = anchor.label, y = anchor.frame.minY
        capture(app, "13-reading-history")
        Thread.sleep(forTimeInterval: 14)
        XCTAssertTrue(conversation.staticTexts[label].isHittable)
        XCTAssertEqual(conversation.staticTexts[label].frame.minY, y, accuracy: 4, "New output must not move what I am reading")
        XCUIDevice.shared.press(.home); app.activate()
        XCTAssertTrue(conversation.staticTexts[label].waitForExistence(timeout: 5))
        XCTAssertEqual(conversation.staticTexts[label].frame.minY, y, accuracy: 4, "Foregrounding must keep the thread and its position")
        capture(app, "14-thread-resumed")
        for _ in 0..<8 { if app.buttons["load-older"].isHittable { break }; conversation.swipeDown() }
        XCTAssertTrue(app.buttons["load-older"].isHittable)
        let first = conversation.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", "Progress note 1.")).firstMatch
        let before = first.frame.minY
        app.buttons["load-older"].tap()
        gone(app.buttons["load-older"])
        XCTAssertTrue(first.isHittable)
        XCTAssertEqual(first.frame.minY, before, accuracy: 4, "Prepending history must not jump away from the current messages")
        capture(app, "15-older-history-loaded")
        let update = conversation.staticTexts["Live update arrived while you were reading."]
        for _ in 0..<12 { if update.isHittable { break }; conversation.swipeUp() }
        XCTAssertTrue(update.isHittable)
        capture(app, "23-latest-output-after-reading")
    }
    func testVerticalCardScrollAndRapidSwipesKeepNavigationStable() {
        let app = launch(["NANOCODEX_DEMO_LONG_THREAD": "1"])
        let title = app.staticTexts["agent-title"].label
        app.scrollViews["card-content"].swipeUp(); app.scrollViews["card-content"].swipeDown()
        XCTAssertEqual(app.staticTexts["agent-title"].label, title)
        XCTAssertFalse(app.buttons["Done"].exists, "Scrolling the card must not open the thread")
        capture(app, "16-card-scroll")
        for _ in 0..<6 {
            let original = app.staticTexts["agent-title"].label
            let card = app.otherElements["agent-card"]
            let start = card.coordinate(withNormalizedOffset: CGVector(dx: 0.8, dy: 0.5))
            start.press(forDuration: 0.01, thenDragTo: card.coordinate(withNormalizedOffset: CGVector(dx: 0.1, dy: 0.5)))
            XCTAssertNotEqual(app.staticTexts["agent-title"].label, original)
            XCTAssertTrue(app.staticTexts["agent-preview"].exists)
        }
        capture(app, "17-rapid-swipes")
    }
    func testCreateStopAndEmptyRunningFilter() {
        let app = launch()
        app.buttons["New agent"].tap()
        XCTAssertEqual(app.staticTexts["agent-title"].label, "New agent")
        XCTAssertFalse(app.buttons["send"].isEnabled)
        queue(app, "Start a checklist")
        gone(app.staticTexts["pending-message"])
        XCTAssertTrue(app.buttons["Stop turn"].isEnabled)
        capture(app, "18-new-agent-running")
        for _ in 0..<3 {
            app.buttons["filter-Running"].tap()
            app.buttons["Stop turn"].tap()
            let stops = app.buttons.matching(identifier: "Stop turn")
            stops.element(boundBy: stops.count - 1).tap()
            Thread.sleep(forTimeInterval: 1)
        }
        XCTAssertTrue(app.staticTexts["Nothing running"].waitForExistence(timeout: 5))
        capture(app, "19-empty-running-inbox")
        app.buttons["View all agents"].tap()
        XCTAssertTrue(app.staticTexts["agent-title"].exists)
    }
    func testInvalidAccountAndReturnToDemo() {
        let app = launch()
        app.buttons["Account settings"].tap()
        app.buttons["Connect account"].tap()
        XCTAssertTrue(app.secureTextFields["Account API key"].waitForExistence(timeout: 5))
        app.secureTextFields["Account API key"].tap(); app.secureTextFields["Account API key"].typeText("invalid")
        app.buttons["Connect account"].tap()
        XCTAssertTrue(app.staticTexts["Enter a Nanocodex account API key."].waitForExistence(timeout: 5))
        capture(app, "20-invalid-account")
        app.buttons["Explore the demo"].tap()
        XCTAssertTrue(app.staticTexts["agent-title"].waitForExistence(timeout: 5))
    }
    func testManyQueuedMessagesRemainScrollableWithKeyboard() {
        let app = launch(); selectInbox(app)
        for index in 1...4 {
            queue(app, "Queued instruction \(index)")
            let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: app.buttons["Cancel queued message"].firstMatch)
            XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 5), .completed)
        }
        app.scrollViews["pending-messages"].swipeUp()
        XCTAssertTrue(app.staticTexts["Queued instruction 4"].isHittable)
        composer(app).tap(); composer(app).typeText("A longer draft\nthat spans several lines\nand stays above the keyboard.")
        XCTAssertLessThanOrEqual(app.buttons["send"].frame.maxY, app.keyboards.firstMatch.frame.minY)
        app.scrollViews["pending-messages"].swipeDown()
        XCTAssertTrue(app.buttons["steer-now"].isHittable)
        capture(app, "21-queue-with-keyboard")
    }
    func testVoiceStopAndReviewWithoutSending() {
        let app = launch(); selectInbox(app)
        app.buttons["Voice input"].tap()
        XCTAssertTrue(app.buttons["Stop recording"].waitForExistence(timeout: 5))
        app.buttons["Stop recording"].tap()
        XCTAssertFalse(app.buttons["Stop recording"].exists)
        XCTAssertTrue(app.staticTexts["Review in the composer"].exists)
        capture(app, "22-voice-stopped")
        app.buttons["Use voice draft"].tap()
        XCTAssertEqual(composer(app).value as? String, "Check reconnect first, then simplify the inbox")
        XCTAssertFalse(app.staticTexts["pending-message"].exists)
    }
    private func capture(_ app: XCUIApplication, _ name: String) {
        // Native sheet/disclosure animations can outlive accessibility queries.
        // Capture their settled layout, not an intermediate clipped frame.
        Thread.sleep(forTimeInterval: 0.4)
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name; attachment.lifetime = .keepAlways; add(attachment)
    }
}
