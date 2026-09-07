import XCTest

final class InboxUITests: XCTestCase {
    override func setUp() { super.setUp(); continueAfterFailure = false }
    func testRemoteScreenControlAndReconnect() throws {
        guard let origin = ProcessInfo.processInfo.environment["NANOCODEX_TEST_REMOTE_ORIGIN"] else {
            throw XCTSkip("Requires signing into the isolated local account and a published Wayland test desktop")
        }
        XCTAssertTrue(URL(string: origin)?.host?.hasSuffix(".localhost") == true)
        let app = XCUIApplication(); app.launch()
        let screens = app.buttons["Remote screens"]
        XCTAssertTrue(screens.waitForExistence(timeout: 20)); screens.tap()
        let desktopName = ProcessInfo.processInfo.environment["NANOCODEX_TEST_REMOTE_MACHINE_NAME"] ?? "Wayland desktop test"
        let desktop = app.buttons.containing(.staticText, identifier: desktopName).firstMatch
        XCTAssertTrue(desktop.waitForExistence(timeout: 15)); desktop.tap()
        let control = app.buttons["Take control"]
        XCTAssertTrue(control.waitForExistence(timeout: 10))
        let connected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: control)
        XCTAssertEqual(XCTWaiter.wait(for: [connected], timeout: 25), .completed)
        control.tap()
        XCTAssertTrue(app.buttons["Release control"].waitForExistence(timeout: 5))
        let canvas = app.otherElements["remote-canvas"]
        XCTAssertTrue(canvas.waitForExistence(timeout: 5)); canvas.tap()
        let remoteReturn = app.buttons.matching(NSPredicate(format: "label == %@", "Return")).firstMatch
        remoteReturn.tap()
        let text = app.textFields["Type on remote screen"]
        text.tap(); text.typeText("touch /workspace/ios-native-control-evidence")
        app.buttons["Send"].tap(); remoteReturn.tap()
        capture(app, "remote-ios-control")
        app.buttons["Release control"].tap()
        app.buttons["Done"].tap(); app.terminate(); app.launch()
        XCTAssertTrue(screens.waitForExistence(timeout: 20)); screens.tap()
        XCTAssertTrue(desktop.waitForExistence(timeout: 15)); desktop.tap()
        XCTAssertTrue(control.waitForExistence(timeout: 10))
        let reconnected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: control)
        XCTAssertEqual(XCTWaiter.wait(for: [reconnected], timeout: 25), .completed)
        control.tap(); XCTAssertTrue(app.buttons["Release control"].waitForExistence(timeout: 5))
        capture(app, "remote-ios-reconnected")
        app.buttons["Release control"].tap(); app.buttons["Done"].tap()
    }

    @MainActor
    func testLiveTerminalProgressAndFailure() async throws {
        try await verifyLiveTerminal(historyOnly: false)
    }

    @MainActor
    func testLiveTerminalReceiptHistory() async throws {
        try await verifyLiveTerminal(historyOnly: true)
    }

    @MainActor
    private func verifyLiveTerminal(historyOnly: Bool) async throws {
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
        let agentID: String, marker: String, successTurnID: String, failureTurnID: String
        if historyOnly {
            guard let savedAgent = environment["NANOCODEX_LIVE_TEST_HISTORY_AGENT"],
                  let savedMarker = environment["NANOCODEX_LIVE_TEST_HISTORY_MARKER"],
                  let savedSuccess = environment["NANOCODEX_LIVE_TEST_SUCCESS_TURN"],
                  let savedFailure = environment["NANOCODEX_LIVE_TEST_FAILURE_TURN"] else {
                throw XCTSkip("Requires an explicitly selected completed success/failure fixture.")
            }
            agentID = savedAgent; marker = savedMarker; successTurnID = savedSuccess; failureTurnID = savedFailure
            print("E12_HISTORY_AGENT \(agentID) \(marker)")
        } else {
            let created = try await request("/v1/agents")
            agentID = try XCTUnwrap(created["agent_id"] as? String)
            marker = "E12 progress " + String(UUID().uuidString.prefix(8))
            successTurnID = UUID().uuidString.lowercased(); failureTurnID = UUID().uuidString.lowercased()
            print("E12_LIVE_AGENT \(agentID) \(marker)")
            _ = try await request("/v1/agents/\(agentID)/turns", body: [
                "id": successTurnID,
                "input": "\(marker). In one Cloudflare Linux sandbox, run exactly: printf 'E12_START\\n'; sleep 60; printf 'E12_MID\\n'; sleep 60; printf 'E12_DONE\\n'. Use exec_command with yield_time_ms 1000, then poll the same process until exit. Do not use a connected device."
            ])
        }
        func openThread(turnID: String, commandText: String) throws {
            app.terminate(); app.launch()
            try require(app.buttons["Browse agents"].waitForExistence(timeout: 30), "The inbox did not connect")
            let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: app.buttons["Browse agents"])
            try require(XCTWaiter.wait(for: [ready], timeout: 10) == .completed, "The inbox did not finish loading")
            let title = app.staticTexts["agent-title"]
            if !title.label.contains(marker) {
                app.buttons["Browse agents"].tap()
                try require(app.otherElements["inbox-sidebar"].waitForExistence(timeout: 10), "The agent sidebar did not open")
                let listing = app.collectionViews.firstMatch
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
                let selected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS %@ AND hittable == true", marker), object: title)
                try require(XCTWaiter.wait(for: [selected], timeout: 10) == .completed, "The requested agent did not open")
            }
            title.tap()
            let conversation = app.scrollViews["conversation"]
            try require(conversation.waitForExistence(timeout: 5), "The conversation did not open")
            let group = app.otherElements["message-activity-" + turnID]
            let activity = group.buttons["activity-disclosure"]
            for _ in 0..<10 {
                if activity.exists && activity.isHittable { break }
                conversation.swipeDown()
            }
            try require(activity.waitForExistence(timeout: 10) && activity.isHittable, "The turn's Activity did not appear")
            activity.tap()
            let timeline = group.scrollViews["activity-timeline"]
            try require(timeline.waitForExistence(timeout: 5), "Activity did not expand")
            for _ in 0..<30 {
                if command(commandText).exists && command(commandText).isHittable { break }
                timeline.swipeUp()
            }
        }
        func command(_ text: String) -> XCUIElement {
            app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Run command,' AND label CONTAINS %@", text)).firstMatch
        }
        func revealOutput(_ predicate: NSPredicate, command: XCUIElement) throws {
            let detailID = command.identifier.replacingOccurrences(of: "activity-step-", with: "activity-detail-")
            let detail = app.scrollViews[detailID]
            let turnID = command.identifier.replacingOccurrences(of: "activity-step-", with: "").components(separatedBy: "::")[0]
            let group = app.otherElements["message-activity-" + turnID]
            let timeline = group.scrollViews["activity-timeline"]
            let conversation = app.scrollViews["conversation"]
            try require(detail.waitForExistence(timeout: 5), "Command details did not expand")
            // Drag the outer margin, then the timeline margin, so nested scroll views
            // cannot forward a gesture to the conversation and hide the result.
            func drag(x: CGFloat, from: CGFloat, to: CGFloat) {
                let origin = app.coordinate(withNormalizedOffset: .zero)
                origin.withOffset(CGVector(dx: x, dy: from)).press(forDuration: 0.01,
                    thenDragTo: origin.withOffset(CGVector(dx: x, dy: to)))
            }
            for _ in 0..<6 {
                let top = timeline.frame.minY
                let target = max(conversation.frame.minY + 110, 180)
                if abs(top - target) < 30 { break }
                let start = app.frame.midY
                drag(x: 8, from: start, to: start + max(-250, min(250, target - top)))
            }
            for _ in 0..<8 {
                let viewport = timeline.frame.intersection(conversation.frame).intersection(app.frame)
                let delta = detail.frame.maxY - viewport.maxY + 8
                if delta <= 12 && detail.frame.minY >= viewport.minY { break }
                let movement = delta > 0 ? -min(180, delta) : min(180, viewport.minY - detail.frame.minY)
                drag(x: timeline.frame.minX + 12, from: viewport.midY, to: viewport.midY + movement)
            }
            let output = detail.staticTexts.matching(predicate).firstMatch
            try require(output.waitForExistence(timeout: 5), "The command result did not contain the expected output")
            func visibleTail() -> Bool {
                let viewport = detail.frame.intersection(timeline.frame).intersection(conversation.frame).intersection(app.frame)
                return !viewport.isNull && output.frame.maxY > viewport.minY && output.frame.maxY <= viewport.maxY + 4
            }
            for _ in 0..<40 {
                if visibleTail() { break }
                let viewport = detail.frame.intersection(timeline.frame).intersection(conversation.frame).intersection(app.frame)
                try require(viewport.height > 50, "The command detail viewport was not visible")
                drag(x: viewport.midX, from: viewport.maxY - 15, to: viewport.minY + 15)
            }
            try require(visibleTail(), "The command output tail was not visible")
        }
        try openThread(turnID: successTurnID, commandText: "E12_START")
        let success = command("E12_START")
        try require(success.waitForExistence(timeout: 90), "The command card did not appear")
        if !historyOnly {
            try require(success.label.contains("Running"), "The yielded command must remain Running")
            success.tap()
            try revealOutput(NSPredicate(format: "label BEGINSWITH %@", "E12_START\n"), command: success)
            capture(app, "E12-live-running")
            try openThread(turnID: successTurnID, commandText: "E12_START")
            try require(command("E12_START").waitForExistence(timeout: 15), "The command was lost after relaunch")
            try require(command("E12_START").label.contains("Running"), "Running status was lost after relaunch")
        }
        let completed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS 'Completed'"), object: command("E12_START"))
        await fulfillment(of: [completed], timeout: 180)
        try require(command("E12_START").label.contains("Completed"), "The command did not complete")
        command("E12_START").tap()
        try revealOutput(NSPredicate(format: "label == %@", "E12_START\nE12_MID\nE12_DONE\n"), command: command("E12_START"))
        try require(app.staticTexts["Elapsed (seconds)"].exists, "Final elapsed time was missing")
        capture(app, "E12-live-completed")
        if !historyOnly {
            _ = try await request("/v1/agents/\(agentID)/turns", body: [
                "id": failureTurnID,
                "input": "In the same sandbox, run exactly: printf 'E12_FAIL_START\\n'; sleep 30; for i in $(seq 1 300); do printf 'E12_STDOUT_%04d\\n' \"$i\"; printf 'E12_STDERR_%04d\\n' \"$i\" >&2; done; printf 'E12_EXPECTED_FAILURE\\n' >&2; exit 7. Use exec_command with yield_time_ms 1000 and max_output_tokens 10000, then poll until exit. This is an intentional failure fixture; do not retry or fix it."
            ])
            try openThread(turnID: failureTurnID, commandText: "E12_FAIL_START")
            let failed = command("E12_FAIL_START")
            try require(failed.waitForExistence(timeout: 60), "The failure fixture command did not appear")
            try require(failed.label.contains("Running") || failed.label.contains("Failed"), "The failure fixture reported an unexpected status")
            let failure = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS 'Failed'"), object: failed)
            await fulfillment(of: [failure], timeout: 90)
            try require(failed.label.contains("Failed"), "Nonzero exit did not produce Failed status")
        }
        try openThread(turnID: failureTurnID, commandText: "E12_FAIL_START")
        try require(command("E12_FAIL_START").waitForExistence(timeout: 15) && command("E12_FAIL_START").label.contains("Failed"), "Failed status was lost after relaunch")
        command("E12_FAIL_START").tap()
        try revealOutput(NSPredicate(format: "label CONTAINS 'E12_STDOUT_0300' AND label CONTAINS 'E12_STDERR_0300' AND label ENDSWITH %@", "E12_EXPECTED_FAILURE\n"), command: command("E12_FAIL_START"))
        try require(app.staticTexts["7"].exists, "The final exit code was not retained")
        try require(app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Command progress,'")).count == 0, "Empty polls created separate cards")
        capture(app, "E12-live-failed-after-relaunch")
    }

    func testScheduledJobsShowAllAgentsAndOpenChatsWithoutChangingDrafts() {
        let app = launch()
        selectAgentFromList(app, title: "Make long sessions bulletproof")
        let title = app.staticTexts["agent-title"].label
        composer(app).tap(); composer(app).typeText("Keep my draft while I check jobs")
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.scrollViews["conversation"].waitForExistence(timeout: 5))
        app.buttons["Done"].tap()
        sidebarButton(app, "inbox-scheduled-jobs").tap()
        let active = app.buttons["scheduled-job-durability/daily-check"]
        let paused = app.buttons["scheduled-job-data/daily-check"]
        XCTAssertTrue(active.waitForExistence(timeout: 10))
        XCTAssertEqual(app.sheets.count, 0, "Scheduled jobs opens directly from the inbox as a navigation page")
        XCTAssertTrue(paused.exists, "Identical job IDs from different agents remain separate")
        XCTAssertTrue(app.staticTexts["Create new scheduled jobs by asking an agent in chat."].exists)
        XCTAssertFalse(app.buttons["New schedule"].exists)
        capture(app, "scheduled-jobs-account-list")
        active.tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label ENDSWITH %@", "Europe/Athens")).firstMatch.waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label ENDSWITH %@", "0 9 * * *")).firstMatch.exists)
        XCTAssertTrue(app.buttons["scheduled-job-latest-run"].exists)
        capture(app, "scheduled-job-detail")
        app.buttons["scheduled-job-source-chat"].tap()
        XCTAssertTrue(app.scrollViews["conversation"].waitForExistence(timeout: 5))
        XCTAssertEqual(composer(app).value as? String, "Keep my draft while I check jobs")
        app.buttons["Done"].tap()
        XCTAssertEqual(app.staticTexts["agent-title"].label, title)
        sidebarButton(app, "inbox-scheduled-jobs").tap()
        paused.tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label ENDSWITH %@", "Continue source chat")).firstMatch.waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Last skipped"].exists)
        capture(app, "scheduled-job-paused")
        app.navigationBars.buttons["Scheduled jobs"].tap()
        active.tap(); app.buttons["scheduled-job-latest-run"].tap()
        XCTAssertTrue(app.scrollViews["conversation"].waitForExistence(timeout: 5))
        app.buttons["Done"].tap()
        XCTAssertEqual(app.staticTexts["agent-title"].label, "Build the agent inbox")
    }

    func testScheduledJobsEmptyStatePointsToChat() {
        let app = launch(["NANOCODEX_DEMO_EMPTY_SCHEDULES": "1"])
        sidebarButton(app, "inbox-scheduled-jobs").tap()
        XCTAssertTrue(app.staticTexts["No scheduled jobs yet"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Ask an agent to run a task on a schedule. It will appear here."].exists)
        XCTAssertFalse(app.buttons["New schedule"].exists)
        capture(app, "scheduled-jobs-empty")
        app.navigationBars.buttons["Inbox"].tap()
        sidebarButton(app, "Account settings").tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.sheets.count, 0, "Settings is a full page, not a bottom sheet")
        XCTAssertTrue(app.buttons["Connect account"].exists)
        capture(app, "inbox-settings-page")
        app.navigationBars.buttons["Inbox"].tap()
        XCTAssertTrue(sidebarButton(app, "inbox-scheduled-jobs").isHittable)
    }

    func testLiveScheduledJobsLoadAfterRelaunch() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_SCHEDULES_UI_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in simulator or device for real schedule reads.")
        }
        let app = XCUIApplication()
        for pass in 1...2 {
            app.launch()
            XCTAssertTrue(app.buttons["Browse agents"].waitForExistence(timeout: 20))
            sidebarButton(app, "inbox-scheduled-jobs").tap()
            let loaded = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
                app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "scheduled-job-")).count > 0
                    || app.staticTexts["No scheduled jobs yet"].exists
            }, object: app)
            XCTAssertEqual(XCTWaiter.wait(for: [loaded], timeout: 90), .completed)
            XCTAssertFalse(app.staticTexts["Some jobs may be missing or out of date"].exists)
            XCTAssertFalse(app.staticTexts["Sample jobs · Demo"].exists)
            let first = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "scheduled-job-")).firstMatch
            if first.exists {
                first.tap()
                XCTAssertTrue(app.buttons["scheduled-job-source-chat"].waitForExistence(timeout: 5))
            }
            capture(app, "scheduled-jobs-live-\(pass)")
            app.terminate()
        }
    }
    private func launch(_ environment: [String: String] = [:]) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["--demo"]
        app.launchEnvironment = ["NANOCODEX_DEMO_COMPLETE_AFTER_MS": "120000", "NANOCODEX_DEMO_DELAY_MS": "600"].merging(environment) { _, new in new }
        app.launch()
        XCTAssertTrue(app.staticTexts["agent-title"].waitForExistence(timeout: 10))
        return app
    }
    func testLiveAccountCreatesConversationAndCompletesTask() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else {
            throw XCTSkip("Opt-in journey requires a signed-in physical device.")
        }
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.buttons["Browse agents"].waitForExistence(timeout: 20), "The phone must already be signed in.")
        capture(app, "live-01-connected-account")
        let create = sidebarButton(app, "New agent")
        create.tap()
        // The empty conversation is usable before server creation completes.
        XCTAssertTrue(self.composer(app).isEnabled)
        XCTAssertEqual(app.staticTexts["agent-title"].label, "New agent")
        XCTAssertEqual(app.staticTexts["agent-preview"].label, "Send a message to begin.")
        XCTAssertTrue(composer(app).waitForExistence(timeout: 20))
        XCTAssertTrue(app.staticTexts["agent-title"].waitForExistence(timeout: 10))
        capture(app, "live-01b-created-conversation")
        let input = "Phone check " + String(UUID().uuidString.prefix(8)) + ". Use a terminal command to calculate 17 * 23. Do not change files. Reply with exactly: Phone check: 391"
        queue(app, input)
        XCTAssertTrue(app.staticTexts["card-user-message"].waitForExistence(timeout: 10))
        XCTAssertEqual(app.staticTexts["card-user-message"].label, input)
        capture(app, "live-02-task-sent")
        let response = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS %@", "Phone check: 391"), object: app.staticTexts["agent-preview"])
        XCTAssertEqual(XCTWaiter.wait(for: [response], timeout: 90), .completed)
        capture(app, "live-03-task-completed")
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.scrollViews["conversation"].waitForExistence(timeout: 5))
        let followUp = "Reply with exactly: Follow-up check: 392"
        queue(app, followUp)
        XCTAssertTrue(app.scrollViews["conversation"].staticTexts["Follow-up check: 392"].waitForExistence(timeout: 60))
        capture(app, "live-04-conversation-follow-up")
        app.buttons["Done"].tap()
        let title = app.staticTexts["agent-title"].label
        app.terminate(); app.launch()
        selectAgentFromList(app, title: title)
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.scrollViews["conversation"].staticTexts["Follow-up check: 392"].waitForExistence(timeout: 15))
        capture(app, "live-05-durable-history-after-relaunch")
        app.buttons["Done"].tap()
        checkLiveVoice(app)
    }
    func testLiveHandConnectsAutomaticallyAndRunsFiles() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else { throw XCTSkip("Requires a signed-in device or simulator.") }
        let app = XCUIApplication(); app.launch()
        XCTAssertTrue(app.buttons["Browse agents"].waitForExistence(timeout: 20))
        sidebarButton(app, "Account settings").tap()
        XCTAssertTrue(app.staticTexts["Hand connected"].waitForExistence(timeout: 30), "This device must become a Hand without enabling it.")
        capture(app, "automatic-hand-connected")
        app.navigationBars.buttons["Inbox"].tap()
        let create = sidebarButton(app, "New agent"); create.tap()
        let created = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.composer(app).isEnabled && app.staticTexts["agent-title"].exists && app.staticTexts["agent-title"].label == "New agent"
                && app.staticTexts["agent-preview"].label == "Send a message to begin."
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [created], timeout: 20), .completed)
        let file = "automatic-hand-" + UUID().uuidString.lowercased() + ".txt"
        queue(app, "Use this iPhone's already-connected Hand named iPhone. Discover its native write_file and read_file tools. Write exactly IPHONE_HAND_WORKS to \(file), then read it back. Use the iPhone Hand, not /brain or a Mac. Reply with the contents. Do not ask about setup.")
        let response = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS %@", "IPHONE_HAND_WORKS"), object: app.staticTexts["agent-preview"])
        XCTAssertEqual(XCTWaiter.wait(for: [response], timeout: 120), .completed)
        capture(app, "automatic-hand-real-file-roundtrip")
        let title = app.staticTexts["agent-title"].label
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["Browse agents"].waitForExistence(timeout: 20))
        sidebarButton(app, "Account settings").tap()
        XCTAssertTrue(app.staticTexts["Hand connected"].waitForExistence(timeout: 30))
        app.navigationBars.buttons["Inbox"].tap()
        selectAgentFromList(app, title: title)
        queue(app, "Read \(file) from the same iPhone Hand again. Reply with its contents and COLD_LAUNCH_HAND_OK.")
        let restored = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS %@", "COLD_LAUNCH_HAND_OK"), object: app.staticTexts["agent-preview"])
        XCTAssertEqual(XCTWaiter.wait(for: [restored], timeout: 120), .completed)
        capture(app, "automatic-hand-restored-file-after-cold-launch")
    }
    func testLiveHandDisableSurvivesRelaunchAndBackground() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else { throw XCTSkip("Requires a signed-in device or simulator.") }
        let app = XCUIApplication(); app.launch()
        func settings() {
            XCTAssertTrue(app.buttons["Browse agents"].waitForExistence(timeout: 30))
            sidebarButton(app, "Account settings").tap()
            XCTAssertTrue(app.switches["device-hand-enabled"].waitForExistence(timeout: 10))
        }
        func setEnabled(_ enabled: Bool) {
            let row = app.switches["device-hand-enabled"]
            if (row.value as? String == "1") != enabled {
                // Target the visible control in the labelled row. Tapping its
                // nested accessibility switch can trigger a scroll before the
                // synthesized touch lands on iOS 26.
                row.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.5)).tap()
            }
            let changed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "value == %@", enabled ? "1" : "0"), object: row)
            XCTAssertEqual(XCTWaiter.wait(for: [changed], timeout: 5), .completed)
        }
        settings()
        let wasEnabled = app.switches["device-hand-enabled"].value as? String == "1"
        addTeardownBlock {
            app.activate()
            if !app.switches["device-hand-enabled"].exists { settings() }
            setEnabled(wasEnabled)
        }
        setEnabled(false)
        XCTAssertTrue(app.staticTexts["Hand disabled"].waitForExistence(timeout: 5))
        app.terminate(); app.launch(); settings()
        XCTAssertEqual(app.switches["device-hand-enabled"].value as? String, "0")
        XCTAssertTrue(app.staticTexts["Hand disabled"].exists)
        capture(app, "hand-disabled-after-cold-launch")
        setEnabled(true)
        XCTAssertTrue(app.staticTexts["Hand connected"].waitForExistence(timeout: 30))
        #if os(iOS)
        XCUIDevice.shared.press(.home)
        XCTAssertTrue(app.wait(for: .runningBackground, timeout: 5))
        // Exceed the app's bounded lease. Foregrounding must establish a fresh
        // ready connection rather than leaving the old socket marked connected.
        let elapsed = expectation(description: "iOS background lease expires")
        DispatchQueue.main.asyncAfter(deadline: .now() + 30) { elapsed.fulfill() }
        wait(for: [elapsed], timeout: 35)
        app.activate()
        XCTAssertTrue(app.staticTexts["Hand connected"].waitForExistence(timeout: 30))
        capture(app, "hand-reconnected-after-background-expiry")
        #endif
    }
    func testLiveHandContinuesUserTaskWhileBackgrounded() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_HAND_TASK_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in physical phone on iOS 26 or later.")
        }
        let app = XCUIApplication(); app.launch()
        XCTAssertTrue(app.buttons["Browse agents"].waitForExistence(timeout: 30))
        sidebarButton(app, "Account settings").tap()
        guard app.switches["device-hand-enabled"].value as? String == "1" else { throw XCTSkip("This device's Hand was explicitly disabled.") }
        XCTAssertTrue(app.staticTexts["Hand connected"].waitForExistence(timeout: 30))
        app.navigationBars.buttons["Inbox"].tap()
        let create = sidebarButton(app, "New agent"); create.tap()
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.composer(app).isEnabled && app.staticTexts["agent-title"].label == "New agent"
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 20), .completed)
        let marker = "HAND_BG_" + UUID().uuidString.replacingOccurrences(of: "-", with: "")
        let file = marker.lowercased() + ".txt"
        print("Background Hand proof file: " + file)
        queue(app, "Background Hand check \(marker). Use accountInfo and tool_search to find this phone's connected native iPhone write_file and read_file tools. Use that iPhone Hand, not a Mac or /brain. Perform 8 sequential rounds: wait 6 seconds using bash sleep, write ROUND_n to \(file) on the iPhone, then read it back. Do not parallelize or skip rounds. After round 8 write exactly \(marker) to the same iPhone file, read it, and reply with only its contents.")
        sidebarButton(app, "Account settings").tap()
        XCTAssertTrue(app.staticTexts["Hand working in background"].waitForExistence(timeout: 15), app.debugDescription)
        capture(app, "hand-task-background-runtime-granted")
        #if os(iOS)
        XCUIDevice.shared.press(.home)
        XCTAssertTrue(app.wait(for: .runningBackground, timeout: 5))
        print("Background Hand window begins: \(Date().timeIntervalSince1970)")
        let elapsed = expectation(description: "Hand task runs beyond the short background lease")
        DispatchQueue.main.asyncAfter(deadline: .now() + 110) { elapsed.fulfill() }
        wait(for: [elapsed], timeout: 115)
        print("Background Hand window ends: \(Date().timeIntervalSince1970)")
        capture(XCUIApplication(bundleIdentifier: "com.apple.springboard"), "hand-task-system-progress")
        app.activate()
        #endif
        app.navigationBars.buttons["Inbox"].tap()
        let completed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS %@", marker), object: app.staticTexts["agent-preview"])
        XCTAssertEqual(XCTWaiter.wait(for: [completed], timeout: 90), .completed)
        capture(app, "hand-task-background-file-result")
    }
    func testLiveRunAgentShortcutOffersAccountAgents() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_HAND_SHORTCUT_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in phone and Shortcuts.")
        }
        let app = XCUIApplication(); app.launch()
        XCTAssertTrue(app.buttons["Browse agents"].waitForExistence(timeout: 30))
        let title = app.staticTexts["agent-title"].label
        let shortcuts = XCUIApplication(bundleIdentifier: "com.apple.shortcuts")
        shortcuts.launch()
        let create = shortcuts.navigationBars.buttons["Create Shortcut"]
        if !create.waitForExistence(timeout: 2) {
            if shortcuts.buttons["Cancel"].exists { shortcuts.buttons["Cancel"].tap() }
            if shortcuts.navigationBars.buttons["BackButton"].exists { shortcuts.navigationBars.buttons["BackButton"].tap() }
        }
        XCTAssertTrue(create.waitForExistence(timeout: 10), shortcuts.debugDescription)
        create.tap()
        let search = shortcuts.searchFields.firstMatch
        XCTAssertTrue(search.waitForExistence(timeout: 10))
        search.tap(); search.typeText("Run Agent Task")
        let action = shortcuts.cells["Run Agent Task"].firstMatch
        XCTAssertTrue(action.waitForExistence(timeout: 10), shortcuts.debugDescription)
        action.tap()
        capture(shortcuts, "hand-task-shortcuts-action")
        let summary = shortcuts.otherElements.matching(NSPredicate(format: "label BEGINSWITH %@ AND label CONTAINS %@", "Ask", "Request")).firstMatch
        XCTAssertTrue(summary.waitForExistence(timeout: 5), shortcuts.debugDescription)
        summary.coordinate(withNormalizedOffset: CGVector(dx: 0.3, dy: 0.25)).tap()
        let agent = shortcuts.tables.staticTexts.matching(NSPredicate(format: "label == %@", title)).firstMatch
        XCTAssertTrue(agent.waitForExistence(timeout: 30), shortcuts.debugDescription)
        capture(shortcuts, "hand-task-shortcuts-account-agents")
    }
    func testLiveVideoAttachmentDraftSendAndHistory() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_VIDEO_UI_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in phone with VideoAudioCheck.mp4 copied into the app Documents folder.")
        }
        guard let title = ProcessInfo.processInfo.environment["NANOCODEX_VIDEO_AGENT_TITLE"], !title.isEmpty else {
            throw XCTSkip("Requires a dedicated, already-created video test conversation with a READY reply.")
        }
        let app = XCUIApplication(); app.launch()
        selectAgentFromList(app, title: title)
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS %@", "READY"), object: app.staticTexts["agent-preview"])
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 60), .completed)
        app.buttons["add-attachments"].tap(); app.buttons["choose-files"].tap()
        XCTAssertTrue(app.buttons["Cancel"].firstMatch.waitForExistence(timeout: 10))
        let file = app.staticTexts.matching(NSPredicate(format: "label == %@ OR label == %@", "VideoAudioCheck.mp4", "VideoAudioCheck")).firstMatch
        if !file.exists || !file.isHittable {
            let browse = app.tabBars["DOC.browsingModeTabBar"].buttons["Browse"]
            XCTAssertTrue(browse.waitForExistence(timeout: 5)); browse.tap()
            if !file.waitForExistence(timeout: 2) {
                let onDevice = app.cells.matching(NSPredicate(format: "label == %@ OR identifier == %@", "On My iPhone", "On My iPhone")).firstMatch
                if !onDevice.waitForExistence(timeout: 2) {
                    let locations = app.navigationBars.buttons["Browse"].firstMatch
                    if locations.exists { locations.tap() }
                }
                XCTAssertTrue(onDevice.waitForExistence(timeout: 5)); onDevice.tap()
                let folder = app.cells.containing(NSPredicate(format: "label == %@ OR label == %@", "Centaur", "Nanocodex Inbox")).firstMatch
                XCTAssertTrue(folder.waitForExistence(timeout: 5)); folder.tap()
            }
        }
        XCTAssertTrue(file.waitForExistence(timeout: 10)); file.tap()
        let open = app.buttons["Open"].firstMatch
        if open.waitForExistence(timeout: 2) { open.tap() }
        let preview = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "preview-video-")).firstMatch
        XCTAssertTrue(preview.waitForExistence(timeout: 30))
        XCTAssertTrue(app.staticTexts["video-analysis-description"].exists)
        XCTAssertTrue(app.buttons["send"].isEnabled)
        let id = preview.identifier
        preview.tap()
        XCTAssertTrue(app.descendants(matching: .any)["video-player"].waitForExistence(timeout: 5))
        capture(app, "video-02-native-clip-preview")
        app.buttons["Done"].tap()
        app.terminate(); app.launch(); selectAgentFromList(app, title: title)
        XCTAssertTrue(app.buttons[id].waitForExistence(timeout: 10), "Keep the original clip with the saved draft.")
        XCTAssertEqual(app.staticTexts["agent-title"].label, title)
        capture(app, "video-03-restored-draft")
        queue(app, "Use tools to compute the SHA-256 of the attached original video at its /brain path. Reply ORIGINAL_FILE_OK and the computed digest. Do not infer bytes from the filename or metadata.")
        let answer = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            let text = app.scrollViews["card-content"].staticTexts.allElementsBoundByIndex.map(\.label).joined(separator: "\n").lowercased()
            return app.staticTexts["agent-title"].label == title && text.contains("original_file_ok") && text.contains("5e9ac6c51375c596547b61d338fd72623e03f2b861a4c45b62f08bc1ef253ca1")
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [answer], timeout: 120), .completed)
        XCTAssertEqual(app.keyboards.count, 0)
        capture(app, "video-04-original-file-digest")
        app.terminate(); app.launch(); selectAgentFromList(app, title: title)
        app.staticTexts["agent-title"].tap()
        let video = app.descendants(matching: .any)["message-video"].firstMatch
        XCTAssertTrue(video.waitForExistence(timeout: 15))
        XCTAssertFalse(app.sliders["video-frame-slider"].firstMatch.exists)
        let play = app.buttons["play-original-video"].firstMatch
        XCTAssertTrue(play.waitForExistence(timeout: 10)); play.tap()
        XCTAssertTrue(app.descendants(matching: .any)["video-player"].waitForExistence(timeout: 30))
        capture(app, "video-05-original-playback-after-reload")
        app.navigationBars["VideoAudioCheck.mp4"].buttons["Done"].tap()
    }
    func testLiveImageAttachmentPickersDraftAndHistory() throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["NANOCODEX_INBOX_LIVE"] == "1",
              let title = environment["NANOCODEX_ATTACHMENT_AGENT_TITLE"], !title.isEmpty else {
            throw XCTSkip("Requires a signed-in phone, a unique short Reply READY prompt as the agent title, and AttachmentCheck4827.png in the app's shared Documents folder.")
        }
        XCTAssertLessThan(title.count, 54, "Keep the exact first prompt short enough to remain the agent title.")
        let app = XCUIApplication()
        app.launch()
        if environment["NANOCODEX_ATTACHMENT_EXISTING"] == "1" {
            selectAgentFromList(app, title: title)
        } else {
            let create = sidebarButton(app, "New agent")
            XCTAssertTrue(create.waitForExistence(timeout: 20), "The phone must already be signed in.")
            create.tap()
            let created = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
                self.composer(app).isEnabled && app.staticTexts["agent-title"].exists
                    && app.staticTexts["agent-title"].label == "New agent"
                    && app.staticTexts["agent-preview"].exists
                    && app.staticTexts["agent-preview"].label == "Send a message to begin."
            }, object: app)
            XCTAssertEqual(XCTWaiter.wait(for: [created], timeout: 20), .completed)
            queue(app, title)
        }
        let seeded = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.staticTexts["agent-title"].label == title
                && app.staticTexts["agent-preview"].label.uppercased().contains("READY")
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [seeded], timeout: 90), .completed)
        print("Live attachment UI conversation title: \(title)")

        app.buttons["add-attachments"].tap()
        app.buttons["choose-photos"].tap()
        let cancelPhotos = app.buttons["Cancel"].firstMatch
        XCTAssertTrue(cancelPhotos.waitForExistence(timeout: 10), "Open the native Photos picker.")
        capture(app, "live-attachment-01-photos-picker")
        cancelPhotos.tap()
        gone(cancelPhotos)
        XCTAssertFalse(app.scrollViews["composer-attachments"].exists)

        let removals = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "remove-attachment-"))
        func waitForAttachment() {
            let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
                removals.count == 1 && app.scrollViews["composer-attachments"].exists
                    && !app.descendants(matching: .any)["preparing-attachments"].exists
            }, object: app)
            XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 20), .completed)
            XCTAssertFalse(app.staticTexts["attachment-error"].exists)
        }
        func chooseFixture() {
            app.buttons["add-attachments"].tap()
            app.buttons["choose-files"].tap()
            XCTAssertTrue(app.buttons["Cancel"].firstMatch.waitForExistence(timeout: 10), "Open the native Files picker.")
            let file = app.staticTexts.matching(NSPredicate(format: "label == %@ OR label == %@", "AttachmentCheck4827.png", "AttachmentCheck4827")).firstMatch
            if !file.exists || !file.isHittable {
                // Recents also exposes location names as file metadata. Switch
                // tabs before finding the actual location cell.
                let browse = app.tabBars["DOC.browsingModeTabBar"].buttons["Browse"]
                XCTAssertTrue(browse.waitForExistence(timeout: 5))
                browse.tap()
                if !file.waitForExistence(timeout: 2) {
                    let onDevice = app.cells.matching(NSPredicate(format: "label == %@ OR identifier == %@", "On My iPhone", "On My iPhone")).firstMatch
                    if !onDevice.waitForExistence(timeout: 2) {
                        let locations = app.navigationBars.buttons["Browse"].firstMatch
                        if locations.exists { locations.tap() }
                    }
                    XCTAssertTrue(onDevice.waitForExistence(timeout: 5), "Browse the real On My iPhone file location.")
                    onDevice.tap()
                    let folder = app.cells.containing(.staticText, identifier: "Nanocodex Inbox").firstMatch
                    XCTAssertTrue(folder.waitForExistence(timeout: 5), "The app's standard Documents sharing must be enabled.")
                    folder.tap()
                }
            }
            XCTAssertTrue(file.waitForExistence(timeout: 10), "Preload AttachmentCheck4827.png in Documents before running this journey.")
            capture(app, "live-attachment-02-files-picker")
            file.tap()
            let open = app.buttons["Open"].firstMatch
            if open.waitForExistence(timeout: 2) {
                let enabled = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: open)
                XCTAssertEqual(XCTWaiter.wait(for: [enabled], timeout: 5), .completed)
                open.tap()
            }
            waitForAttachment()
        }

        chooseFixture()
        XCTAssertTrue(app.buttons["send"].isEnabled, "An image-only message is sendable after preparation.")
        capture(app, "live-attachment-03-image-draft")
        removals.firstMatch.tap()
        gone(app.scrollViews["composer-attachments"])
        XCTAssertFalse(app.buttons["send"].isEnabled, "Removing the only image restores an empty draft.")
        chooseFixture()
        let attachmentID = removals.firstMatch.identifier
        let prompt = "Read the attached image. Reply with its exact heading, then the three shapes from left to right, giving each color followed by its shape. Do not use tools."
        composer(app).tap()
        composer(app).typeText(prompt)
        XCTAssertEqual(composer(app).value as? String, prompt)
        app.terminate()
        app.launch()
        selectAgentFromList(app, title: title)
        waitForAttachment()
        XCTAssertEqual(removals.firstMatch.identifier, attachmentID, "Restore the original image reference after relaunch.")
        XCTAssertEqual(composer(app).value as? String, prompt)
        capture(app, "live-attachment-04-restored-draft")

        app.buttons["send"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["card-image"].waitForExistence(timeout: 10), "Show the submitted image immediately.")
        capture(app, "live-attachment-05-image-sent")
        let answer = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            let text = app.staticTexts["agent-preview"].label.lowercased()
            return ["attachment check 4827", "red square", "blue circle", "green triangle"].allSatisfy(text.contains)
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [answer], timeout: 120), .completed, "The actual managed agent must read the heading and colored shapes from the image.")
        capture(app, "live-attachment-06-real-image-reply")

        assertStoredImageHistory(app, title: title, prompt: prompt)
    }

    func testLiveImageAttachmentReopensHistory() throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["NANOCODEX_INBOX_LIVE"] == "1",
              let title = environment["NANOCODEX_ATTACHMENT_AGENT_TITLE"], !title.isEmpty else {
            throw XCTSkip("Requires the existing real image-attachment validation conversation.")
        }
        assertStoredImageHistory(XCUIApplication(), title: title,
            prompt: "Read the attached image. Reply with its exact heading, then the three shapes from left to right, giving each color followed by its shape. Do not use tools.")
    }

    private func assertStoredImageHistory(_ app: XCUIApplication, title: String, prompt: String) {
        app.terminate()
        app.launch()
        selectAgentFromList(app, title: title)
        app.staticTexts["agent-title"].tap()
        let conversation = app.scrollViews["conversation"]
        XCTAssertTrue(conversation.waitForExistence(timeout: 10))
        let image = conversation.descendants(matching: .any)["message-image"]
        for _ in 0..<4 {
            if image.isHittable { break }
            conversation.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.3)).press(forDuration: 0.05,
                thenDragTo: conversation.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.65)),
                withVelocity: .slow, thenHoldForDuration: 0.2)
        }
        XCTAssertTrue(image.waitForExistence(timeout: 20), "Reload the sent image from durable managed-agent history.")
        XCTAssertTrue(image.isHittable)
        XCTAssertTrue(conversation.staticTexts.matching(NSPredicate(format: "label == %@", prompt)).firstMatch.exists)
        XCTAssertFalse(app.scrollViews["composer-attachments"].exists)
        capture(app, "live-attachment-07-durable-image-history")
    }

    func testLiveStopActsImmediatelyFromTheSendButton() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in physical iPhone.")
        }
        let app = XCUIApplication(); app.launch()
        let create = sidebarButton(app, "New agent")
        XCTAssertTrue(create.waitForExistence(timeout: 20)); create.tap()
        let created = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.composer(app).isEnabled && app.staticTexts["agent-title"].label == "New agent"
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [created], timeout: 20), .completed)
        let title = "Stop check " + String(UUID().uuidString.prefix(8)) + ". Reply READY"
        queue(app, title)
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.staticTexts["agent-title"].label == title && app.staticTexts["agent-preview"].label.uppercased().contains("READY")
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 90), .completed)
        print("Live stop UI conversation title: \(title)")
        queue(app, "Run a terminal command that sleeps for 60 seconds. Do not change files. I will stop this turn from the app.")
        let action = app.buttons["send"]
        let running = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            action.label == "Stop turn" && action.isEnabled
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [running], timeout: 30), .completed)
        capture(app, "live-stop-01-running")
        action.tap()
        XCTAssertFalse(app.sheets["Stop this turn?"].exists)
        let stopped = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.staticTexts["Stopped"].exists && action.label == "Send message" && !action.isEnabled
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [stopped], timeout: 30), .completed)
        capture(app, "live-stop-02-stopped")
    }

    func testLiveCameraCaptureAndCancel() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in physical iPhone with a camera.")
        }
        let app = XCUIApplication()
        app.launch()
        let create = sidebarButton(app, "New agent")
        XCTAssertTrue(create.waitForExistence(timeout: 20))
        create.tap()
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.composer(app).isEnabled && app.staticTexts["agent-title"].label == "New agent"
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 20), .completed)
        let title = "Camera check " + String(UUID().uuidString.prefix(8)) + ". Reply READY"
        queue(app, title)
        let named = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.staticTexts["agent-title"].label == title && app.staticTexts["agent-preview"].label.uppercased().contains("READY")
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [named], timeout: 90), .completed)
        print("Live camera UI conversation title: \(title)")
        app.buttons["add-attachments"].tap()
        XCTAssertTrue(app.buttons["choose-camera"].exists)
        XCTAssertTrue(app.buttons["choose-photos"].exists)
        XCTAssertTrue(app.buttons["choose-files"].exists)
        capture(app, "camera-01-attachment-menu")
        app.buttons["choose-camera"].tap()
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let permission = springboard.alerts.firstMatch
        if permission.waitForExistence(timeout: 3) {
            let allow = permission.buttons.matching(NSPredicate(format: "label IN %@", ["Allow", "OK"])).firstMatch
            if allow.exists { allow.tap() }
        }
        let dismissCamera = app.buttons.matching(NSPredicate(format: "identifier == %@ OR label IN %@", "DismissImagePickerButton", ["Dismiss", "Cancel"])).firstMatch
        XCTAssertTrue(dismissCamera.waitForExistence(timeout: 10), app.debugDescription)
        capture(app, "camera-02-native-camera")
        dismissCamera.tap()
        XCTAssertTrue(app.buttons["add-attachments"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.scrollViews["composer-attachments"].exists)
        app.buttons["add-attachments"].tap(); app.buttons["choose-camera"].tap()
        let shutter = app.buttons.matching(NSPredicate(format: "identifier IN %@ OR label IN %@", ["PhotoCapture", "TakePicture", "Take Picture"], ["Take Picture", "Take Photo"])).firstMatch
        XCTAssertTrue(shutter.waitForExistence(timeout: 10), app.debugDescription)
        shutter.tap()
        let use = app.buttons.matching(NSPredicate(format: "label IN %@", ["Use Photo", "Use"])).firstMatch
        XCTAssertTrue(use.waitForExistence(timeout: 10))
        use.tap()
        let remove = app.buttons["Remove Camera photo.jpg"]
        XCTAssertTrue(remove.waitForExistence(timeout: 20))
        XCTAssertTrue(app.buttons["send"].isEnabled)
        capture(app, "camera-03-prepared-photo")
        remove.tap()
        XCTAssertFalse(app.scrollViews["composer-attachments"].exists)
        XCTAssertFalse(app.buttons["send"].isEnabled)
    }

    func testLiveComposerSendsWhileReadingEarlierHistory() throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["NANOCODEX_INBOX_LIVE"] == "1",
              let title = environment["NANOCODEX_PERFORMANCE_AGENT_TITLE"], !title.isEmpty else {
            throw XCTSkip("Requires a signed-in account and the existing real history agent's title.")
        }
        let appearance = environment["NANOCODEX_INBOX_RESTORE_APPEARANCE"] == "light" ? .light : XCUIDevice.shared.appearance
        addTeardownBlock { XCUIDevice.shared.appearance = appearance }
        XCUIDevice.shared.appearance = .dark
        let app = XCUIApplication()
        app.launch()
        selectAgentFromList(app, title: title)
        app.staticTexts["agent-title"].tap()
        let conversation = app.scrollViews["conversation"]
        XCTAssertTrue(conversation.waitForExistence(timeout: 10))
        let rows = conversation.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH %@", "message-"))
        XCTAssertTrue(rows.firstMatch.waitForExistence(timeout: 20))
        let latestID = rows.allElementsBoundByIndex.last(where: { $0.isHittable })?.identifier
        var earlierID: String?
        for _ in 0..<24 {
            // Release after holding still, so reaching older history does not
            // fling the enclosing sheet into an interactive dismissal.
            conversation.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.3)).press(forDuration: 0.05,
                thenDragTo: conversation.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.65)),
                withVelocity: .slow, thenHoldForDuration: 0.2)
            if let earlier = rows.allElementsBoundByIndex.first(where: {
                $0.identifier != latestID && $0.isHittable && $0.frame.minY >= conversation.frame.minY && $0.frame.minY < conversation.frame.midY
            }) { earlierID = earlier.identifier; break }
        }
        let anchorID = try XCTUnwrap(earlierID, "Find an earlier actual message with its top visible")
        let anchor = rows.matching(identifier: anchorID).firstMatch
        let input = composer(app)
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: input)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 10), .completed, "The composer must be usable after scrolling settles")
        XCTAssertTrue(anchor.isHittable)
        capture(app, "live-reading-01-earlier-history")
        let y = anchor.frame.minY
        let previousValue = input.value as? String ?? ""
        let originalDraft = previousValue == input.placeholderValue ? "" : previousValue
        addTeardownBlock { [self] in
            let field = composer(app)
            guard field.exists else { return }
            let value = field.value as? String ?? ""
            if (value == field.placeholderValue ? "" : value) != originalDraft {
                field.tap()
                field.typeKey("a", modifierFlags: .command)
                field.typeText(originalDraft.isEmpty ? XCUIKeyboardKey.delete.rawValue : originalDraft)
            }
        }
        let task = "Use a terminal command to calculate 19 * 23. Do not change files. Reply with exactly: Reading check: 437"
        input.tap()
        input.typeKey("a", modifierFlags: .command)
        input.typeText(task)
        XCTAssertEqual(composer(app).value as? String, task)
        capture(app, "live-reading-02-keyboard-and-draft")
        XCTAssertTrue(anchor.isHittable)
        XCTAssertEqual(anchor.frame.minY, y, accuracy: 4, "Typing must preserve the earlier message's position")
        XCTAssertTrue(app.buttons["send"].isHittable)
        XCTAssertLessThanOrEqual(app.buttons["send"].frame.maxY, app.keyboards.firstMatch.frame.minY)
        app.buttons["send"].tap()
        let cleared = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            let field = self.composer(app)
            let value = field.value as? String ?? ""
            return value.isEmpty || value == field.placeholderValue
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [cleared], timeout: 5), .completed, "Sending immediately clears the accepted local draft")
        XCTAssertFalse(app.buttons["send"].isEnabled)
        if app.staticTexts["pending-message"].exists { XCTAssertEqual(app.staticTexts["pending-message"].label, task) }
        capture(app, "live-reading-03-sent-with-position-retained")
        XCTAssertTrue(anchor.isHittable)
        XCTAssertEqual(anchor.frame.minY, y, accuracy: 4, "Sending must preserve the earlier message's position")

        // Only after checking geometry, move through actual history to the reply.
        let reply = conversation.staticTexts["Reading check: 437"]
        let deadline = Date().addingTimeInterval(90)
        while Date() < deadline {
            if reply.waitForExistence(timeout: 1), reply.isHittable { break }
            conversation.swipeUp()
        }
        XCTAssertTrue(reply.isHittable, "The managed agent must complete the real terminal task")
        gone(app.buttons["Stop turn"], timeout: 30)
        gone(app.staticTexts["pending-message"])
        capture(app, "live-reading-04-real-completed-reply")
    }
    func testLiveAccountRestoresSessionAcrossColdLaunches() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else {
            throw XCTSkip("Opt-in journey requires a signed-in physical device.")
        }
        let app = XCUIApplication()
        for index in 1...3 {
            app.terminate()
            app.launch()
            XCTAssertFalse(app.descendants(matching: .any)["phone-onboarding"].exists, "A saved account must not show the sign-in form while restoring")
            XCTAssertTrue(app.buttons["Browse agents"].waitForExistence(timeout: 20), "Cold launch must restore the signed-in inbox")
            XCTAssertFalse(app.textFields["phone-number"].exists)
            XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "identifier == %@ AND label == %@", "connection", "Demo")).firstMatch.exists)
            capture(app, "live-cold-launch-\(index)")
        }
    }
    func testLiveVoiceConnectsMinimizesAndStops() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_VOICE_UI_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in device for native voice service evidence.")
        }
        let app = XCUIApplication()
        app.launchEnvironment["NANOCODEX_VOICE_TIMING"] = "1"
        app.launch()
        XCTAssertTrue(app.buttons["Browse agents"].waitForExistence(timeout: 20))
        defer {
            if app.buttons["end-voice"].exists { app.buttons["end-voice"].tap() }
            else if app.buttons["end-voice-compact"].exists { app.buttons["end-voice-compact"].tap() }
        }
        for pass in 1...2 {
            XCTAssertTrue(app.buttons["start-voice"].waitForExistence(timeout: 10))
            let began = ProcessInfo.processInfo.systemUptime
            app.buttons["start-voice"].tap()
            XCTAssertTrue(app.buttons["mute-voice"].waitForExistence(timeout: 10))
            // Exercise microphone activation on the first call. The second
            // retains the distinct muted-during-connection readiness path.
            if pass == 2, app.buttons["mute-voice"].label == "Mute microphone" { app.buttons["mute-voice"].tap() }
            let readyStatuses = pass == 1 ? ["Listening", "Speaking", "Working on it"] : ["Microphone muted"]
            let connected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label IN %@", readyStatuses), object: app.staticTexts["voice-status"])
            let result = XCTWaiter.wait(for: [connected], timeout: 55)
            if result != .completed { capture(app, "voice-live-start-failed-\(pass)") }
            XCTAssertEqual(result, .completed,
                           app.staticTexts["voice-error"].exists ? app.staticTexts["voice-error"].label : "Voice did not become active; status: \(app.staticTexts["voice-status"].label)")
            print("VOICE_UI_READY pass=\(pass) started_muted=\(pass == 2) elapsed_ms=\(Int((ProcessInfo.processInfo.systemUptime - began) * 1_000))")
            XCTAssertTrue(app.descendants(matching: .any).matching(identifier: "voice-orb").firstMatch.exists)
            if pass == 1 {
                XCTAssertEqual(app.buttons["mute-voice"].label, "Mute microphone")
                capture(app, "voice-live-connected-unmuted")
                app.buttons["mute-voice"].tap()
                XCTAssertEqual(app.buttons["mute-voice"].label, "Unmute microphone")
            }
            capture(app, "voice-live-connected-\(pass)")
            app.buttons["close-voice"].tap()
            XCTAssertTrue(app.buttons["end-voice-compact"].waitForExistence(timeout: 5))
            capture(app, "voice-live-minimized-\(pass)")
            app.buttons["end-voice-compact"].tap()
            gone(app.buttons["end-voice-compact"])
        }
    }

    private func checkLiveVoice(_ app: XCUIApplication) {
        XCTAssertTrue(app.buttons["start-voice"].waitForExistence(timeout: 10))
        app.buttons["start-voice"].tap()
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let alert = springboard.alerts.firstMatch
        if alert.waitForExistence(timeout: 4) {
            if alert.buttons["Allow"].exists { alert.buttons["Allow"].tap() }
            else if alert.buttons["OK"].exists { alert.buttons["OK"].tap() }
        }
        let live = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label IN %@", ["Listening", "Speaking", "Working on it"]), object: app.staticTexts["voice-status"])
        XCTAssertEqual(XCTWaiter.wait(for: [live], timeout: 50), .completed)
        capture(app, "live-06-voice-connected")
        app.buttons["mute-voice"].tap()
        XCTAssertEqual(app.buttons["mute-voice"].label, "Unmute microphone")
        app.buttons["close-voice"].tap()
        XCTAssertTrue(app.buttons["end-voice-compact"].waitForExistence(timeout: 5))
        capture(app, "live-07-voice-minimized")
        app.buttons["end-voice-compact"].tap()
        gone(app.buttons["end-voice-compact"])
        capture(app, "live-08-voice-ended")
    }
    private func composer(_ app: XCUIApplication) -> XCUIElement {
        app.textFields["composer"].exists ? app.textFields["composer"] : app.textViews["composer"]
    }
    private func sidebarButton(_ app: XCUIApplication, _ label: String) -> XCUIElement {
        if !app.otherElements["inbox-sidebar"].exists {
            let browse = app.buttons["Browse agents"]
            XCTAssertTrue(browse.waitForExistence(timeout: 20))
            browse.tap()
        }
        let action = app.buttons[label].firstMatch
        XCTAssertTrue(action.waitForExistence(timeout: 5))
        XCTAssertTrue(action.isHittable, "Sidebar actions stay accessible while agent history scrolls")
        return action
    }
    private func selectAgentFromList(_ app: XCUIApplication, title: String) {
        let browse = app.buttons["Browse agents"]
        XCTAssertTrue(browse.waitForExistence(timeout: 20))
        let available = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: browse)
        XCTAssertEqual(XCTWaiter.wait(for: [available], timeout: 5), .completed)
        browse.tap()
        XCTAssertTrue(app.otherElements["inbox-sidebar"].waitForExistence(timeout: 10))
        XCTAssertEqual(app.sheets.count, 0, "Agent history opens from the left sidebar")
        let listReady = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.collectionViews.firstMatch.exists || app.tables.firstMatch.exists
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [listReady], timeout: 10), .completed, "The agent browser must expose its native List")
        let list = app.collectionViews.firstMatch.exists ? app.collectionViews.firstMatch : app.tables.firstMatch
        let row = list.buttons.containing(.staticText, identifier: title).firstMatch
        let deadline = Date().addingTimeInterval(40)
        // SwiftUI List materializes only nearby rows. Search the actual list,
        // including a previously retained scroll position, with finite bounds.
        for direction in 0..<2 {
            for step in 0...12 {
                if row.exists && row.isHittable {
                    row.tap()
                    gone(app.otherElements["inbox-sidebar"])
                    let selected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@ AND hittable == true", title), object: app.staticTexts["agent-title"])
                    XCTAssertEqual(XCTWaiter.wait(for: [selected], timeout: 10), .completed, "The exact requested conversation must be selected")
                    return
                }
                guard step < 12, Date() < deadline else { break }
                if direction == 0 { list.swipeUp() } else { list.swipeDown() }
            }
        }
        capture(app, "agent-list-exact-title-not-found")
        XCTFail("The real agent titled '\(title)' was not hittable after bounded list scrolling")
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
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            let send = app.buttons["send"]
            return send.isEnabled && send.isHittable && self.composer(app).value as? String == text
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 20), .completed)
        app.buttons["send"].tap()
        let submitted = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in self.composer(app).value as? String != text }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [submitted], timeout: 5), .completed, "The tap must submit the draft before checking queue state")
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
    func testStopDoesNotWaitForFollowUpSubmission() {
        let app = launch(["NANOCODEX_DEMO_DELAY_MS": "8000", "NANOCODEX_DEMO_CANCEL_DELAY_MS": "100"])
        selectInbox(app); queue(app, "Slow submission")
        let stop = app.buttons["send"]
        XCTAssertEqual(stop.label, "Stop turn")
        XCTAssertTrue(stop.isEnabled, "Sending a follow-up must not lock Stop")
        stop.tap()
        XCTAssertFalse(app.sheets["Stop this turn?"].exists)
        XCTAssertTrue(app.staticTexts["Stopped"].waitForExistence(timeout: 4))
    }
    func testFirstMessageCanBeStoppedBeforeAdmission() {
        let app = launch(["NANOCODEX_DEMO_DELAY_MS": "8000", "NANOCODEX_DEMO_CANCEL_DELAY_MS": "100"])
        sidebarButton(app, "New agent").tap()
        XCTAssertEqual(app.staticTexts["agent-title"].label, "New agent")
        queue(app, "Cancel the first submission")
        XCTAssertEqual(app.buttons["send"].label, "Stop turn")
        XCTAssertTrue(app.buttons["send"].isEnabled)
        app.buttons["send"].tap()
        XCTAssertTrue(app.staticTexts["Stopped"].waitForExistence(timeout: 4))
        let resurrected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", "Stop turn"), object: app.buttons["send"])
        resurrected.isInverted = true
        XCTAssertEqual(XCTWaiter.wait(for: [resurrected], timeout: 9), .completed)
        capture(app, "first-send-stopped-before-admission")
    }
    func testCancelDuringSubmissionCannotReturnOnLateAcknowledgement() {
        let app = launch(["NANOCODEX_DEMO_DELAY_MS": "8000", "NANOCODEX_DEMO_CANCEL_DELAY_MS": "100"])
        selectInbox(app); queue(app, "Cancel before acknowledgement")
        let cancel = app.buttons["Cancel queued message"]
        XCTAssertTrue(cancel.isEnabled, "Cancel bypasses the send request")
        cancel.tap(); gone(app.staticTexts["pending-message"])
        let resurrected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: app.staticTexts["pending-message"])
        resurrected.isInverted = true
        XCTAssertEqual(XCTWaiter.wait(for: [resurrected], timeout: 9), .completed)
        XCTAssertEqual(app.buttons["send"].label, "Stop turn", "The original turn remains running")
        capture(app, "cancel-during-send-no-resurrection")
    }
    func testStopIntentSurvivesColdLaunch() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString, "NANOCODEX_DEMO_CANCEL_DELAY_MS": "5000"])
        selectInbox(app)
        app.buttons["send"].tap()
        XCTAssertEqual(app.buttons["send"].label, "Stopping turn")
        app.terminate(); app.launch(); selectInbox(app)
        XCTAssertEqual(app.buttons["send"].label, "Stopping turn")
        XCTAssertTrue(app.staticTexts["Stopped"].waitForExistence(timeout: 10))
        XCTAssertEqual(app.buttons["send"].label, "Send message")
        capture(app, "stop-restored-and-confirmed")
    }
    func testQueuedMessageCanBeCancelledWhileSteering() {
        let app = launch(["NANOCODEX_DEMO_CANCEL_DELAY_MS": "3000"])
        selectInbox(app); queue(app, "Withdraw this correction")
        XCTAssertTrue(app.buttons["steer-now"].waitForExistence(timeout: 5))
        app.buttons["steer-now"].tap()
        let cancel = app.buttons["Cancel queued message"]
        XCTAssertTrue(cancel.isEnabled, "Steering must not disable cancelling the queued message")
        cancel.tap(); gone(app.staticTexts["pending-message"], timeout: 10)
        XCTAssertTrue(app.staticTexts["Stopped"].waitForExistence(timeout: 10))
    }
    func testLiveCancelQueuedMessageThenSteerItsSuccessor() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else { throw XCTSkip("Requires a signed-in physical phone.") }
        let app = XCUIApplication(); app.launch()
        let create = sidebarButton(app, "New agent")
        XCTAssertTrue(create.waitForExistence(timeout: 30)); create.tap()
        let created = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.composer(app).isEnabled && app.staticTexts["agent-title"].label == "New agent"
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [created], timeout: 20), .completed)
        let marker = String(UUID().uuidString.prefix(8))
        let title = "Steer check \(marker). Reply READY"
        print("Live steering UI conversation title: " + title)
        queue(app, title)
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", "READY"), object: app.staticTexts["agent-preview"])
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 90), .completed)
        queue(app, "Keep running terminal commands `sleep 10` until 90 seconds have elapsed. Do not finish early or change files. I will interrupt this from the app.")
        let running = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.buttons["send"].label == "Stop turn" && app.buttons["send"].isEnabled
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [running], timeout: 30), .completed)
        queue(app, "Reply only REMOVE_\(marker)")
        XCTAssertTrue(app.buttons["steer-now"].waitForExistence(timeout: 15))
        let sendReady = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in app.buttons["send"].isEnabled }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [sendReady], timeout: 10), .completed)
        queue(app, "Reply only STEERED_\(marker)")
        let two = XCTNSPredicateExpectation(predicate: NSPredicate(format: "count == 2"), object: app.staticTexts.matching(identifier: "pending-message"))
        XCTAssertEqual(XCTWaiter.wait(for: [two], timeout: 15), .completed)
        app.buttons["Cancel queued message"].firstMatch.tap()
        let one = XCTNSPredicateExpectation(predicate: NSPredicate(format: "count == 1"), object: app.staticTexts.matching(identifier: "pending-message"))
        XCTAssertEqual(XCTWaiter.wait(for: [one], timeout: 30), .completed)
        XCTAssertTrue(app.buttons["steer-now"].waitForExistence(timeout: 15))
        app.buttons["steer-now"].tap()
        let reply = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", "STEERED_" + marker), object: app.staticTexts["agent-preview"])
        XCTAssertEqual(XCTWaiter.wait(for: [reply], timeout: 90), .completed)
        gone(app.staticTexts["pending-message"])
        capture(app, "live-steer-after-queued-cancellation")
        app.terminate(); app.launch()
        selectAgentFromList(app, title: title)
        thread(app, contains: "Reply only STEERED_" + marker)
        XCTAssertTrue(app.staticTexts["STEERED_" + marker].exists)
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
    func testInteractiveVoiceRequiresAccountAndPreservesTypedDraft() {
        let app = launch(); selectInbox(app)
        composer(app).tap(); composer(app).typeText("Existing draft.")
        app.buttons["start-voice"].tap()
        XCTAssertTrue(app.staticTexts["voice-panel"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Sign in to use interactive voice."].waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts["voice-status"].label, "Voice paused")
        XCTAssertTrue(app.buttons["retry-voice"].isEnabled)
        XCTAssertFalse(app.buttons["end-voice"].exists)
        XCTAssertFalse(app.alerts.firstMatch.exists, "Demo voice must not ask for microphone access")
        capture(app, "08-interactive-voice-sign-in")
        app.buttons["retry-voice"].tap()
        XCTAssertTrue(app.staticTexts["Sign in to use interactive voice."].waitForExistence(timeout: 5))
        XCUIDevice.shared.press(.home); app.activate()
        XCTAssertTrue(app.buttons["close-voice"].waitForExistence(timeout: 5))
        app.buttons["close-voice"].tap()
        gone(app.staticTexts["voice-panel"])
        XCTAssertTrue(app.scrollViews["conversation"].waitForExistence(timeout: 5))
        XCTAssertEqual(composer(app).value as? String, "Existing draft.")
        app.buttons["Done"].tap()
        XCTAssertFalse(app.staticTexts["pending-message"].exists)
        app.otherElements["agent-card"].swipeLeft(); selectInbox(app)
        XCTAssertEqual(composer(app).value as? String, "Existing draft.")
        capture(app, "09-voice-draft-preserved")
    }
    func testVoiceConversationStreamsBothSpeakersWhileMinimized() {
        let app = launch(["NANOCODEX_DEMO_VOICE": "1", "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        composer(app).tap(); composer(app).typeText("Keep my typed draft.")
        app.buttons["start-voice"].tap()
        XCTAssertTrue(app.staticTexts["voice-panel"].waitForExistence(timeout: 5))
        let connecting = app.descendants(matching: .any).matching(identifier: "voice-connecting").firstMatch
        XCTAssertTrue(connecting.waitForExistence(timeout: 5))
        XCTAssertFalse(app.otherElements["voice-orb"].exists, "Connecting must not look ready before the session is active")
        XCTAssertFalse(app.scrollViews["voice-conversation"].exists)
        capture(app, "voice-01-connecting-spinner")
        let orb = app.descendants(matching: .any).matching(identifier: "voice-orb").firstMatch
        XCTAssertTrue(orb.waitForExistence(timeout: 20))
        gone(connecting)
        XCTAssertEqual(app.staticTexts["voice-status"].label, "Listening")
        capture(app, "voice-02-active-orb")
        app.buttons["mute-voice"].tap()
        XCTAssertEqual(app.buttons["mute-voice"].label, "Unmute microphone")
        app.buttons["voice-return-chat"].tap()
        let conversation = app.scrollViews["conversation"]
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        XCTAssertEqual(composer(app).value as? String, "Keep my typed draft.")
        let user = conversation.staticTexts["voice-transcript-user"]
        let assistant = conversation.staticTexts["voice-transcript-assistant"]
        XCTAssertTrue(user.waitForExistence(timeout: 5))
        XCTAssertEqual(user.label, "Can you hear", "Show input directly in chat before turn.done")
        XCTAssertFalse(assistant.exists)
        XCTAssertFalse(app.otherElements["voice-transcript-compact"].exists)
        XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "realtime_delegation")).firstMatch.exists)
        XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "internal-only")).firstMatch.exists)
        capture(app, "voice-03-chat-user-partial")
        XCTAssertTrue(assistant.waitForExistence(timeout: 20))
        XCTAssertEqual(assistant.label, "I can", "Show output directly in chat before turn.done")
        XCTAssertEqual(user.label, "Can you hear me?")
        XCTAssertEqual(conversation.staticTexts.matching(identifier: "voice-transcript-user").count, 1)
        let grew = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", "I can hear you"), object: assistant)
        XCTAssertEqual(XCTWaiter.wait(for: [grew], timeout: 12), .completed)
        capture(app, "voice-04-chat-assistant-partial")
        let completed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", "I can hear you clearly."), object: assistant)
        XCTAssertEqual(XCTWaiter.wait(for: [completed], timeout: 12), .completed)
        app.buttons["start-voice"].tap()
        XCTAssertTrue(orb.waitForExistence(timeout: 5))
        app.buttons["end-voice"].tap()
        gone(app.staticTexts["voice-panel"])
        XCTAssertTrue(conversation.staticTexts["I can hear you clearly."].exists, "Stopping retains spoken text in chat")
        let durable = conversation.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH %@", "message-demo-voice:")).firstMatch
        XCTAssertTrue(durable.waitForExistence(timeout: 10))
        XCTAssertEqual(conversation.staticTexts.matching(NSPredicate(format: "label == %@", "I can hear you clearly.")).count, 1, "Durable voice history replaces its pending final without duplication")
        XCTAssertEqual(composer(app).value as? String, "Keep my typed draft.")
        capture(app, "voice-05-ended-durable-chat")
        XCTAssertFalse(app.alerts.firstMatch.exists, "Transcript fixtures never request microphone access")
    }
    func testReadableToolActivityAndUnlabelledReplies() {
        let app = launch(["NANOCODEX_DEMO_MANY_TOOLS": "1"]); selectInbox(app)
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 5))
        let conversation = app.scrollViews["conversation"]
        XCTAssertFalse(conversation.staticTexts["nanocodex"].exists)
        XCTAssertFalse(conversation.staticTexts["exec_command"].exists)
        XCTAssertFalse(conversation.staticTexts["Run command"].exists)
        XCTAssertEqual(conversation.buttons.matching(identifier: "activity-disclosure").count, 1)
        XCTAssertTrue(conversation.staticTexts["Build the agent inbox"].isHittable)
        XCTAssertFalse(conversation.staticTexts["Checking the remaining steps."].exists)
        XCTAssertFalse(conversation.staticTexts["Thinking"].exists)
        capture(app, "10-readable-activity")
        conversation.buttons["activity-disclosure"].tap()
        let step = conversation.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "activity-step-tool-")).firstMatch
        XCTAssertTrue(step.waitForExistence(timeout: 3))
        XCTAssertFalse(conversation.staticTexts["Command"].exists, "Tool payloads need a second disclosure")
        capture(app, "11-activity-timeline")
        step.tap()
        XCTAssertTrue(conversation.staticTexts.matching(identifier: "Command").firstMatch.waitForExistence(timeout: 3))
        XCTAssertTrue(conversation.staticTexts.matching(identifier: "Exit code").firstMatch.exists)
        XCTAssertTrue(conversation.staticTexts.matching(identifier: "swift test --package-path apple/InboxCore").firstMatch.exists)
        capture(app, "11-activity-details")
        for _ in 0..<5 { if conversation.buttons["activity-disclosure"].isHittable { break }; conversation.swipeDown() }
        conversation.buttons["activity-disclosure"].tap()
        XCTAssertFalse(conversation.staticTexts.matching(identifier: "Run command").firstMatch.exists)
        XCTAssertTrue(conversation.staticTexts["Build the agent inbox"].isHittable)
    }
    func testSendButtonBecomesStopOnlyForAnEmptyRunningDraft() {
        let app = launch(); selectInbox(app)
        let action = app.buttons["send"]
        XCTAssertEqual(action.label, "Stop turn")
        XCTAssertEqual(app.buttons.matching(identifier: "Stop turn").count, 1)
        let actionX = action.frame.midX
        composer(app).tap(); composer(app).typeText("Keep going")
        XCTAssertEqual(action.label, "Queue message")
        XCTAssertFalse(app.buttons["Stop turn"].exists)
        XCTAssertEqual(action.frame.midX, actionX, accuracy: 1)
        composer(app).typeKey("a", modifierFlags: .command)
        composer(app).typeText(XCUIKeyboardKey.delete.rawValue)
        XCTAssertEqual(action.label, "Stop turn")
        XCTAssertTrue(app.keyboards.firstMatch.exists, "Clearing the draft keeps the keyboard open")
        capture(app, "composer-stop-on-right")
        action.tap()
        XCTAssertFalse(app.sheets["Stop this turn?"].exists)
        let stopped = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            action.label == "Send message" && !action.isEnabled
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [stopped], timeout: 5), .completed)
    }

    func testLiveMarkdownAndKeyboard() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in device.")
        }
        let app = XCUIApplication(); app.launch()
        XCTAssertTrue(app.buttons["Browse agents"].waitForExistence(timeout: 20))
        sidebarButton(app, "New agent").tap()
        queue(app, "Reply exactly with this Markdown, without an enclosing code fence:\n# Markdown verified\n\n**Bold text** and `inline code`.\n\n- First item\n- Second item\n\n```swift\nlet answer = 42\n```\n\n| Name | Value |\n| --- | --- |\n| Answer | 42 |")
        gone(app.keyboards.firstMatch)
        let card = app.scrollViews["card-content"]
        XCTAssertTrue(card.staticTexts["Markdown verified"].waitForExistence(timeout: 90))
        let completed = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.buttons["send"].label == "Send message" && !app.buttons["send"].isEnabled
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [completed], timeout: 90), .completed)
        card.swipeUp()
        XCTAssertTrue(card.staticTexts["Bold text and inline code."].exists)
        XCTAssertTrue(card.staticTexts["First item"].exists)
        capture(app, "live-markdown-inbox-keyboard-dismissed")
        for _ in 0..<3 {
            if card.buttons["Copy code"].isHittable { break }
            card.swipeUp()
        }
        XCTAssertTrue(card.buttons["Copy code"].isHittable)
        capture(app, "live-markdown-code-and-table")
        card.swipeDown(); card.swipeDown()
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.scrollViews["conversation"].waitForExistence(timeout: 5))
        queue(app, "Reply exactly with: **Follow-up verified**")
        gone(app.keyboards.firstMatch)
        let conversation = app.scrollViews["conversation"]
        XCTAssertTrue(conversation.staticTexts["Follow-up verified"].waitForExistence(timeout: 90))
        conversation.swipeUp()
        capture(app, "live-markdown-conversation-keyboard-dismissed")
        app.buttons["Done"].tap()
    }

    func testMarkdownRendersInInboxAndConversation() {
        let app = launch(["NANOCODEX_DEMO_MARKDOWN": "1"])
        let card = app.scrollViews["card-content"]
        let heading = card.staticTexts["Markdown check"]
        XCTAssertTrue(heading.waitForExistence(timeout: 5), "Keep the start of replies longer than 1,400 characters")
        XCTAssertTrue(card.staticTexts["Read bold, italic, and inline code with a link."].exists)
        XCTAssertTrue(card.staticTexts["First item"].exists)
        XCTAssertFalse(card.staticTexts["# Markdown check"].exists)
        capture(app, "markdown-inbox")
        for _ in 0..<3 {
            if card.buttons["Copy code"].isHittable { break }
            card.swipeUp()
        }
        XCTAssertTrue(card.buttons["Copy code"].isHittable)
        XCTAssertTrue(card.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "let marker = \"**literal**\"")).firstMatch.exists)
        capture(app, "markdown-inbox-code")
        card.swipeDown(); card.swipeDown()
        app.staticTexts["agent-title"].tap()
        let conversation = app.scrollViews["conversation"]
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        for _ in 0..<6 {
            if conversation.staticTexts["Markdown check"].isHittable { break }
            conversation.swipeDown()
        }
        XCTAssertTrue(conversation.staticTexts["Markdown check"].isHittable)
        XCTAssertTrue(conversation.staticTexts["First item"].exists)
        capture(app, "markdown-conversation")
    }

    func testLongMarkdownConversationStartsAtLatest() {
        let app = launch(["NANOCODEX_DEMO_RENDER_PROFILE": "1"])
        app.staticTexts["agent-title"].tap()
        let conversation = app.scrollViews["conversation"]
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        let latest = conversation.staticTexts["Review note 80"]
        XCTAssertTrue(latest.waitForExistence(timeout: 5))
        XCTAssertTrue(latest.isHittable, "Lazy rich messages must open at the latest reply")
        let initialY = latest.frame.minY
        composer(app).tap()
        composer(app).typeText("Keep the latest reply in view")
        XCTAssertTrue(latest.isHittable)
        XCTAssertEqual(latest.frame.minY, initialY, accuracy: 4, "Opening the keyboard retains the visible reply")
        capture(app, "long-markdown-latest-with-keyboard")
        app.buttons["Done"].tap()
        XCTAssertEqual(composer(app).value as? String, "Keep the latest reply in view")
    }

    func testGeneratedAttachmentsStayVisibleWhileInternalToolOutputStaysInActivity() {
        func assertNoInternalOutput(_ scope: XCUIElement, file: StaticString = #filePath, line: UInt = #line) {
            for marker in ["INTERNAL_MEMORY_RECORD", "INTERNAL_COMMAND_OUTPUT", "INTERNAL_WAIT_OUTPUT", "Script completed", "\"memories\""] {
                XCTAssertFalse(scope.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", marker)).firstMatch.exists,
                               "Internal tool output escaped Activity: " + marker, file: file, line: line)
            }
        }
        let app = launch(["NANOCODEX_DEMO_GENERATED_OUTPUTS": "1"])
        let cardImage = app.descendants(matching: .any).matching(identifier: "generated-image-loaded").firstMatch
        XCTAssertTrue(cardImage.waitForExistence(timeout: 10), "The inbox card renders actual emitted PNG bytes")
        assertNoInternalOutput(app)
        capture(app, "generated-output-inbox-image")
        app.staticTexts["agent-title"].tap()
        let conversation = app.scrollViews["conversation"]
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        XCTAssertTrue(conversation.staticTexts["Generated chart"].waitForExistence(timeout: 10))
        XCTAssertTrue(conversation.staticTexts["The three bars are ready to review."].exists)
        XCTAssertTrue((conversation.buttons["activity-disclosure"].value as? String ?? "").contains("Collapsed"))
        assertNoInternalOutput(conversation)
        let images = conversation.descendants(matching: .any).matching(identifier: "generated-image-loaded")
        XCTAssertEqual(images.count, 1, "Inner MCP and outer exec results share one visible image")
        for _ in 0..<4 { if images.firstMatch.isHittable { break }; conversation.swipeDown() }
        XCTAssertTrue(images.firstMatch.isHittable)
        assertNoInternalOutput(conversation)
        capture(app, "generated-output-conversation-image")
        let audio = conversation.buttons["generated-audio-play"]
        for _ in 0..<4 { if audio.isHittable { break }; conversation.swipeUp() }
        XCTAssertTrue(audio.waitForExistence(timeout: 10), "Embedded WAV bytes become a playable audio control")
        XCTAssertTrue(audio.isEnabled)
        XCTAssertTrue(conversation.buttons["chart.csv"].exists, "Embedded CSV bytes become a downloadable file")
        XCTAssertFalse(conversation.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "data:image/png;base64,")).firstMatch.exists)
        XCTAssertFalse(conversation.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "UklGR")).firstMatch.exists)
        assertNoInternalOutput(conversation)
        capture(app, "generated-output-audio-and-file")
        let activity = conversation.buttons["activity-disclosure"]
        for _ in 0..<6 { if activity.isHittable { break }; conversation.swipeDown() }
        activity.tap()
        let memory = conversation.buttons["activity-step-generated-turn::tool:memory"]
        XCTAssertTrue(memory.waitForExistence(timeout: 5))
        memory.tap()
        XCTAssertTrue(conversation.staticTexts["INTERNAL_MEMORY_RECORD"].waitForExistence(timeout: 5), "Tool details remain available when explicitly opened")
        capture(app, "memory-details-in-activity")
        for _ in 0..<6 { if activity.isHittable { break }; conversation.swipeDown() }
        activity.tap()
        assertNoInternalOutput(conversation)
    }

    func testThinkingRendersMarkdownAndHighlightedCode() {
        let app = launch(["NANOCODEX_DEMO_THINKING_MARKDOWN": "1"])
        app.staticTexts["agent-title"].tap()
        let conversation = app.scrollViews["conversation"]
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        conversation.buttons["activity-disclosure"].tap()
        conversation.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "activity-step-thinking-")).firstMatch.tap()
        let detail = conversation.scrollViews.matching(NSPredicate(format: "identifier BEGINSWITH %@", "activity-detail-thinking-")).firstMatch
        XCTAssertTrue(detail.staticTexts["Reasoning check"].waitForExistence(timeout: 5))
        XCTAssertTrue(detail.staticTexts["Check both paths and answer before continuing."].exists)
        XCTAssertTrue(detail.staticTexts["Preserve the draft"].exists)
        XCTAssertFalse(detail.staticTexts["## Reasoning check"].exists)
        capture(app, "thinking-markdown")
        let code = detail.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "let answer = 42")).firstMatch
        for _ in 0..<4 {
            if code.isHittable && detail.buttons["Copy code"].isHittable { break }
            detail.swipeUp()
        }
        XCTAssertTrue(detail.buttons["Copy code"].isHittable)
        XCTAssertTrue(code.isHittable)
        capture(app, "thinking-highlighted-swift")
        detail.buttons["Copy code"].tap()
        XCTAssertTrue(detail.buttons["Copied"].waitForExistence(timeout: 3))
    }

    func testSendingDismissesKeyboardInInboxAndConversation() {
        let app = launch()
        sidebarButton(app, "New agent").tap()
        composer(app).tap(); composer(app).typeText("First message")
        XCTAssertTrue(app.keyboards.firstMatch.exists)
        app.buttons["send"].tap()
        XCTAssertTrue(app.staticTexts["card-user-message"].waitForExistence(timeout: 5))
        gone(app.keyboards.firstMatch)
        XCTAssertTrue(app.buttons["Browse agents"].isHittable)
        XCTAssertEqual(app.staticTexts["card-user-message"].label, "First message")
        XCTAssertFalse(app.scrollViews["pending-messages"].exists)
        XCTAssertEqual(app.buttons["send"].label, "Stop turn")
        capture(app, "inbox-sent-keyboard-dismissed")
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.scrollViews["conversation"].waitForExistence(timeout: 5))
        queue(app, "Follow-up message")
        gone(app.keyboards.firstMatch)
        XCTAssertTrue(app.staticTexts["pending-message"].waitForExistence(timeout: 5))
        capture(app, "conversation-queued-keyboard-dismissed")
    }
    func testToolFailureIsReadable() {
        let app = launch(["NANOCODEX_DEMO_TOOL_ERROR": "1"]); selectInbox(app)
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 5))
        let conversation = app.scrollViews["conversation"]
        XCTAssertTrue(conversation.buttons["activity-disclosure"].value as? String == "1 step, 1 issue, Collapsed")
        conversation.buttons["activity-disclosure"].tap()
        conversation.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "activity-step-tool-")).firstMatch.tap()
        let failure = conversation.staticTexts["The browser disconnected. Reconnect it and try again."]
        let visible = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: failure)
        XCTAssertEqual(XCTWaiter.wait(for: [visible], timeout: 5), .completed)
        capture(app, "12-activity-failure")
    }
    func testLongThreadKeepsPlaceAcrossUpdatesHistoryAndForeground() {
        let app = launch(["NANOCODEX_DEMO_LONG_THREAD": "1", "NANOCODEX_DEMO_HISTORY_DELAY_MS": "6000"]); selectInbox(app)
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
        XCTAssertFalse(app.buttons["load-older"].exists)
        let loading = app.descendants(matching: .any)["loading-older"].firstMatch
        let earlier = conversation.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", "Earlier note ")).firstMatch
        for _ in 0..<12 {
            if loading.exists || earlier.exists { break }
            conversation.swipeDown()
        }
        // XCTest can wait for scrolling to idle until the page has already
        // arrived. Completed pagination is also valid evidence; don't require
        // catching a transient progress indicator after the request finished.
        XCTAssertTrue(loading.exists || earlier.exists, "Reaching earlier history loads the next page automatically")
        if loading.exists {
            let first = conversation.staticTexts.allElementsBoundByIndex.first {
                $0.isHittable && $0.label.hasPrefix("Progress note ") && $0.frame.minY >= conversation.frame.minY
            }!
            let firstLabel = first.label
            let before = first.frame.minY
            gone(loading)
            let retained = conversation.staticTexts[firstLabel]
            XCTAssertTrue(retained.isHittable)
            XCTAssertEqual(retained.frame.minY, before, accuracy: 4, "Prepending history must not jump away from the current messages")
        }
        for _ in 0..<4 { if earlier.isHittable { break }; conversation.swipeDown() }
        XCTAssertTrue(earlier.isHittable)
        capture(app, "15-older-history-loaded")
        let update = conversation.staticTexts["I found one more edge case in the retry path."]
        for _ in 0..<12 { if update.isHittable { break }; conversation.swipeUp() }
        XCTAssertTrue(update.isHittable)
        capture(app, "23-latest-output-after-reading")
    }
    func testConversationComposerKeepsReadingPositionAndSharesDraft() {
        let app = launch(["NANOCODEX_DEMO_LONG_THREAD": "1"]); selectInbox(app)
        composer(app).tap(); composer(app).typeText("Review")
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 5))
        XCTAssertEqual(composer(app).value as? String, "Review")
        let conversation = app.scrollViews["conversation"]
        conversation.swipeDown(); conversation.swipeDown()
        let anchor = conversation.staticTexts.allElementsBoundByIndex.first {
            $0.isHittable && $0.label.hasPrefix("Progress note ") && $0.frame.minY >= conversation.frame.minY
        }!
        let label = anchor.label, y = anchor.frame.minY
        XCTAssertTrue(composer(app).isHittable, "The composer stays available above older messages")
        composer(app).tap(); composer(app).typeText(" the earlier messages")
        capture(app, "24-conversation-draft-while-reading")
        let submitted = (composer(app).value as? String ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        XCTAssertTrue(submitted.contains("Review")); XCTAssertTrue(submitted.contains("the earlier messages"))
        XCTAssertTrue(conversation.staticTexts[label].isHittable)
        XCTAssertEqual(conversation.staticTexts[label].frame.minY, y, accuracy: 4, "Opening the keyboard must preserve the message I am reading")
        XCTAssertLessThanOrEqual(app.buttons["send"].frame.maxY, app.keyboards.firstMatch.frame.minY)
        app.buttons["send"].tap()
        XCTAssertTrue(app.buttons["steer-now"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts["pending-message"].label, submitted)
        XCTAssertTrue(conversation.staticTexts[label].isHittable)
        XCTAssertEqual(conversation.staticTexts[label].frame.minY, y, accuracy: 4, "Sending must keep the current reading position")
        gone(app.keyboards.firstMatch)
        app.buttons["steer-now"].tap(); gone(app.staticTexts["pending-message"])
        capture(app, "25-conversation-follow-up-queued")
        let reply = conversation.staticTexts["Working on: " + submitted]
        for _ in 0..<16 { if reply.isHittable { break }; scrollVisibleConversation(app, upward: true) }
        XCTAssertTrue(reply.isHittable)
        XCTAssertEqual(conversation.staticTexts.matching(NSPredicate(format: "label == %@", submitted)).count, 1)
        composer(app).tap(); composer(app).typeText("Keep this next draft")
        app.buttons["Done"].tap()
        XCTAssertEqual(composer(app).value as? String, "Keep this next draft")
    }
    func testLiveCardSwipesRetainUserMessagesWithReplies() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in device or simulator with an existing conversation.")
        }
        let app = XCUIApplication(); app.launch()
        XCTAssertTrue(app.buttons["filter-All"].waitForExistence(timeout: 30))
        sidebarButton(app, "New agent").tap()
        let originalInput = "Swipe cache check " + String(UUID().uuidString.prefix(8)) + ". Reply with exactly SWIPE_CACHE_OK."
        queue(app, originalInput)
        let completed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS %@", "SWIPE_CACHE_OK"), object: app.staticTexts["agent-preview"])
        XCTAssertEqual(XCTWaiter.wait(for: [completed], timeout: 90), .completed)
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 5))
        app.buttons["Done"].tap()
        let original = app.staticTexts["agent-title"].label
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["Browse agents"].waitForExistence(timeout: 30))
        selectAgentFromList(app, title: original)
        XCTAssertEqual(app.staticTexts["card-user-message"].label, originalInput)
        for index in 0..<3 {
            // Drag the floating status bar so a reply's horizontal code/table scroller
            // cannot consume the navigation gesture.
            let status = app.otherElements["agent-card-header"]
            status.coordinate(withNormalizedOffset: CGVector(dx: 0.8, dy: 0.5))
                .press(forDuration: 0.01, thenDragTo: status.coordinate(withNormalizedOffset: CGVector(dx: 0.1, dy: 0.5)))
            XCTAssertNotEqual(app.staticTexts["agent-title"].label, original)
            selectAgentFromList(app, title: original)
            XCTAssertEqual(app.staticTexts["agent-title"].label, original)
            XCTAssertEqual(app.staticTexts["card-user-message"].label, originalInput)
            XCTAssertTrue(app.staticTexts["agent-preview"].label.contains("SWIPE_CACHE_OK"))
            let attachment = XCTAttachment(screenshot: app.screenshot())
            attachment.name = "live-card-exchange-\(index)"; attachment.lifetime = .keepAlways; add(attachment)
        }
    }
    func testVerticalCardScrollAndRapidSwipesKeepNavigationStable() {
        let app = launch(["NANOCODEX_DEMO_LONG_PREVIEW": "1"])
        app.buttons["filter-All"].tap()
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
        let status = app.otherElements["agent-card-header"]
        let start = status.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        start.press(forDuration: 0.01, thenDragTo: start.withOffset(CGVector(dx: 0, dy: -180)))
        XCTAssertFalse(app.buttons["Done"].exists)
        XCTAssertEqual(app.staticTexts["agent-title"].label, "New agent", "The floating status bar creates an agent even when the preview scrolls")
    }

    func testNewThreadPullIgnoresShortDragsAndFlicks() {
        let app = launch(["NANOCODEX_DEMO_LONG_PREVIEW": "1"])
        let original = app.staticTexts["agent-title"].label
        let status = app.otherElements["agent-card-header"]
        for distance in [CGFloat(45), 95, 140] {
            let start = status.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
            start.press(forDuration: 0.01, thenDragTo: start.withOffset(CGVector(dx: 0, dy: -distance)),
                withVelocity: .fast, thenHoldForDuration: 0)
            XCTAssertEqual(app.staticTexts["agent-title"].label, original, "Short flicks must not create a thread")
            gone(app.otherElements["new-thread-pull-indicator"])
        }
        let partial = status.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        partial.press(forDuration: 0.01, thenDragTo: partial.withOffset(CGVector(dx: 0, dy: -140)),
            withVelocity: .slow, thenHoldForDuration: 1)
        XCTAssertEqual(app.staticTexts["agent-title"].label, original, "A partial pull can be held and released safely")
        capture(app, "new-thread-pull-cancelled")

        let deliberate = status.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        deliberate.press(forDuration: 0.01, thenDragTo: deliberate.withOffset(CGVector(dx: 0, dy: -210)),
            withVelocity: .slow, thenHoldForDuration: 1)
        XCTAssertEqual(app.staticTexts["agent-title"].label, "New agent")
        XCTAssertEqual(app.staticTexts["agent-preview"].label, "Send a message to begin.")
        capture(app, "new-thread-pull-created")
    }

    func testSwipeUpCreatesAgentAndSidebarKeepsNavigationAccessible() {
        let app = launch()
        let original = app.staticTexts["agent-title"].label
        composer(app).tap(); composer(app).typeText("Keep my original draft")
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 5))
        app.buttons["Done"].tap()
        XCTAssertFalse(app.buttons["Refresh agents"].exists)
        XCTAssertFalse(app.buttons["New agent"].exists)
        XCTAssertFalse(app.buttons["Account settings"].exists)
        app.otherElements["agent-card"].swipeUp()
        XCTAssertEqual(app.staticTexts["agent-title"].label, "New agent")
        XCTAssertEqual(app.staticTexts["agent-preview"].label, "Send a message to begin.")
        XCTAssertFalse(app.scrollViews["conversation"].exists)
        capture(app, "swipe-up-new-agent")
        _ = sidebarButton(app, "Account settings")
        let sidebar = app.otherElements["inbox-sidebar"]
        XCTAssertEqual(app.sheets.count, 0)
        XCTAssertLessThan(sidebar.frame.width, app.frame.width)
        XCTAssertEqual(sidebar.frame.minX, app.frame.minX, accuracy: 1)
        XCTAssertTrue(app.buttons["New agent"].isHittable)
        XCTAssertTrue(app.buttons["inbox-scheduled-jobs"].isHittable)
        capture(app, "left-sidebar-navigation")
        app.buttons["Close sidebar"].tap()
        gone(sidebar)
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.02, dy: 0.45))
            .press(forDuration: 0.01, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.65, dy: 0.45)))
        XCTAssertTrue(sidebar.waitForExistence(timeout: 5), "An edge swipe reveals the sidebar")
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.92, dy: 0.45))
            .press(forDuration: 0.01, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.3, dy: 0.45)))
        gone(sidebar)
        selectAgentFromList(app, title: original)
        XCTAssertEqual(app.staticTexts["agent-title"].label, original, "Swiping up keeps the previous agent in history")
        XCTAssertEqual(composer(app).value as? String, "Keep my original draft")
    }
    func testCardShowsSentMessageAndInboxReachesZero() {
        let app = launch()
        sidebarButton(app, "New agent").tap()
        queue(app, "Remember the message I just sent")
        XCTAssertTrue(app.staticTexts["card-user-message"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts["card-user-message"].label, "Remember the message I just sent")
        XCTAssertTrue(app.staticTexts["card-user-message"].isHittable)
        capture(app, "26-card-user-message")
        app.buttons["filter-Inbox"].tap()
        for _ in 0..<6 {
            if app.staticTexts["Nothing in your inbox"].exists { break }
            app.otherElements["agent-card"].swipeLeft()
        }
        XCTAssertTrue(app.staticTexts["Nothing in your inbox"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.otherElements["agent-card"].exists)
        capture(app, "27-inbox-zero")
        app.buttons["View all agents"].tap()
        XCTAssertTrue(app.staticTexts["agent-title"].waitForExistence(timeout: 5))
    }
    func testCreateStopAndEmptyRunningFilter() {
        let app = launch()
        sidebarButton(app, "New agent").tap()
        XCTAssertEqual(app.staticTexts["agent-title"].label, "New agent")
        XCTAssertFalse(app.buttons["send"].isEnabled)
        queue(app, "Start a checklist")
        gone(app.staticTexts["pending-message"])
        XCTAssertTrue(app.buttons["Stop turn"].isEnabled)
        capture(app, "18-new-agent-running")
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 5)); app.buttons["Done"].tap()
        for _ in 0..<3 {
            app.buttons["filter-Running"].tap()
            app.buttons["Stop turn"].tap()
            Thread.sleep(forTimeInterval: 1)
        }
        XCTAssertTrue(app.staticTexts["Nothing running"].waitForExistence(timeout: 5))
        capture(app, "19-empty-running-inbox")
        app.buttons["View all agents"].tap()
        XCTAssertTrue(app.staticTexts["agent-title"].exists)
    }
    func testNewConversationOpensAndSendsDuringSlowCreation() {
        let app = launch(["NANOCODEX_DEMO_CREATE_DELAY_MS": "10000"])
        sidebarButton(app, "New agent").tap()
        XCTAssertEqual(app.staticTexts["agent-title"].label, "New agent")
        XCTAssertTrue(composer(app).isEnabled)
        queue(app, "A message before creation finishes")
        XCTAssertEqual(app.staticTexts["card-user-message"].label, "A message before creation finishes")
        composer(app).tap(); composer(app).typeText("Keep my next draft")
        let admitted = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.buttons["send"].isEnabled
                && app.staticTexts["agent-preview"].label != "Sending…"
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [admitted], timeout: 15), .completed)
        XCTAssertEqual(composer(app).value as? String, "Keep my next draft")
        XCTAssertTrue(app.keyboards.firstMatch.exists, "Creation must keep the active composer focused.")
        capture(app, "instant-conversation-send-and-draft")
    }
    func testLateCreationDoesNotNavigateAwayFromAnotherConversation() {
        let app = launch(["NANOCODEX_DEMO_CREATE_DELAY_MS": "5000"])
        let original = app.staticTexts["agent-title"].label
        sidebarButton(app, "New agent").tap()
        selectAgentFromList(app, title: original)
        composer(app).tap(); composer(app).typeText("Keep this conversation selected")
        Thread.sleep(forTimeInterval: 6)
        XCTAssertEqual(app.staticTexts["agent-title"].label, original)
        XCTAssertEqual(composer(app).value as? String, "Keep this conversation selected")
        capture(app, "instant-conversation-keeps-selection")
    }
    func testNewConversationCreationFailureRetainsDraftAndRetries() {
        let app = launch(["NANOCODEX_DEMO_CREATE_DELAY_MS": "1500", "NANOCODEX_DEMO_FAIL_ONCE": "create"])
        sidebarButton(app, "New agent").tap()
        composer(app).tap(); composer(app).typeText("Keep my draft through retry")
        XCTAssertTrue(app.buttons["retry-creation"].waitForExistence(timeout: 5))
        XCTAssertEqual(composer(app).value as? String, "Keep my draft through retry")
        app.buttons["retry-creation"].tap()
        gone(app.buttons["retry-creation"])
        Thread.sleep(forTimeInterval: 2)
        XCTAssertEqual(composer(app).value as? String, "Keep my draft through retry")
        app.buttons["send"].tap()
        XCTAssertEqual(app.staticTexts["card-user-message"].label, "Keep my draft through retry")
        capture(app, "instant-conversation-retry")
    }
    func testCancelBeforeCreationDoesNotSendCancelledMessage() {
        let app = launch(["NANOCODEX_DEMO_CREATE_DELAY_MS": "20000"])
        sidebarButton(app, "New agent").tap()
        queue(app, "Do not send this message")
        app.buttons["Stop turn"].tap()
        queue(app, "Send only this replacement")
        let admitted = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.staticTexts["agent-preview"].label != "Sending…"
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [admitted], timeout: 25), .completed)
        XCTAssertEqual(app.staticTexts["card-user-message"].label, "Send only this replacement")
        app.staticTexts["agent-title"].tap()
        XCTAssertTrue(app.scrollViews["conversation"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["Do not send this message"].exists)
        XCTAssertTrue(app.staticTexts["Send only this replacement"].exists)
        capture(app, "instant-conversation-cancel-before-creation")
    }
    func testUnfinishedConversationDraftSurvivesRelaunch() {
        let app = launch(["NANOCODEX_DEMO_CREATE_DELAY_MS": "60000", "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        sidebarButton(app, "New agent").tap()
        composer(app).tap(); composer(app).typeText("Keep this unfinished conversation")
        app.terminate()
        app.launchEnvironment["NANOCODEX_DEMO_CREATE_DELAY_MS"] = "0"
        app.launch()
        selectAgentFromList(app, title: "New agent")
        XCTAssertEqual(composer(app).value as? String, "Keep this unfinished conversation")
        capture(app, "instant-conversation-restored-draft")
    }
    func testInvalidAccountAndReturnToDemo() {
        let app = launch()
        sidebarButton(app, "Account settings").tap()
        app.buttons["Connect account"].tap()
        XCTAssertTrue(app.textFields["phone-number"].waitForExistence(timeout: 5))
        capture(app, "19-phone-sign-in")
        XCTAssertFalse(app.secureTextFields["Account API key"].exists)
        app.textFields["phone-number"].tap(); app.textFields["phone-number"].typeText("555")
        app.buttons["sign-in-submit"].tap()
        XCTAssertTrue(app.staticTexts["Enter a valid phone number and check the selected country."].waitForExistence(timeout: 5))
        XCTAssertFalse(app.textFields["verification-code"].exists)
        capture(app, "20-invalid-account")
        app.swipeUp()
        app.buttons["Explore the demo"].tap()
        XCTAssertTrue(app.staticTexts["agent-title"].waitForExistence(timeout: 5))
    }
    func testPhoneNumberUsesDeviceRegionAndHonorsInternationalPaste() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "-AppleLocale", "en_GR", "-AppleLanguages", "(en)"]
        app.launch()
        XCTAssertTrue(app.staticTexts["agent-title"].waitForExistence(timeout: 10))
        sidebarButton(app, "Account settings").tap(); app.buttons["Connect account"].tap()
        let phone = app.textFields["phone-number"]
        XCTAssertTrue(phone.waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["phone-country"].label.contains("Greece"))
        phone.tap(); phone.typeText("6971234567")
        XCTAssertEqual(app.staticTexts["sign-in-hint"].label, "We’ll text a code to +306971234567.")
        capture(app, "24-country-inferred")
        phone.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: 10) + "+12025550123")
        XCTAssertEqual(app.staticTexts["sign-in-hint"].label, "We’ll text a code to +12025550123.")
        XCTAssertTrue(app.buttons["sign-in-submit"].isEnabled)
        capture(app, "25-international-phone")
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
    func testPerformanceSavedAccountResponsiveColdLaunch() throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["NANOCODEX_INBOX_PERFORMANCE"] == "1", environment["NANOCODEX_INBOX_LIVE"] == "1" else {
            throw XCTSkip("Opt-in launch profiling requires a signed-in device and both performance/live flags.")
        }
        let app = XCUIApplication()
        let options = XCTMeasureOptions()
        options.iterationCount = 5
        options.invocationOptions = [.manuallyStart, .manuallyStop]
        // Process-cold launches; filesystem/OS caches remain. XCTest also runs
        // one discarded warm-up. This metric ends at main-thread responsiveness,
        // independently of automation wall time or subsequent account loading.
        measure(metrics: [XCTApplicationLaunchMetric(waitUntilResponsive: true),
            XCTOSSignpostMetric(subsystem: "xyz.paradigm.centaur", category: "Performance", name: "RestoreAccount")], options: options) {
            app.terminate()
            startMeasuring()
            app.launch()
            XCTAssertTrue(app.buttons["Browse agents"].waitForExistence(timeout: 20), "Restore the saved account after each launch")
            stopMeasuring()
            XCTAssertFalse(app.textFields["phone-number"].exists)
            XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "identifier == %@ AND label == %@", "connection", "Demo")).firstMatch.exists)
        }
        app.terminate()
    }

    func testPerformanceDemoConversationRendering() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_PERFORMANCE"] == "1" else {
            throw XCTSkip("Opt-in deterministic rendering profile; does not use a saved account.")
        }
        let app = launch(["NANOCODEX_DEMO_RENDER_PROFILE": "1", "NANOCODEX_DEMO_PROFILE": "render-audit-" + UUID().uuidString])
        app.staticTexts["agent-title"].tap()
        let conversation = app.scrollViews["conversation"]
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        XCTAssertTrue(conversation.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", "Review note ")).firstMatch.waitForExistence(timeout: 5))
        capture(app, "profile-long-markdown-before-typing")
        composer(app).tap()
        let options = XCTMeasureOptions()
        options.iterationCount = 3
        var expectedDraft = ""
        measure(metrics: [XCTCPUMetric(application: app), XCTMemoryMetric(application: app)], options: options) {
            let input = composer(app)
            input.tap()
            let addition = (expectedDraft.isEmpty ? "" : " ") + "Keep this draft while reviewing earlier messages."
            expectedDraft += addition
            input.typeText(addition)
            XCTAssertEqual(input.value as? String, expectedDraft)
        }
        capture(app, "profile-long-markdown-conversation")
    }

    func testPerformanceInboxInteractionJourney() throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["NANOCODEX_INBOX_PERFORMANCE"] == "1", environment["NANOCODEX_INBOX_LIVE"] == "1",
              let title = environment["NANOCODEX_PERFORMANCE_AGENT_TITLE"], !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw XCTSkip("Opt-in live profiling requires a signed-in account, both performance/live flags, and an actual managed-agent title.")
        }
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.buttons["Browse agents"].waitForExistence(timeout: 20))
        defer { app.terminate() }
        // App-target metrics capture this process; keep it alive for every iteration.
        let conversation = app.scrollViews["conversation"]
        func waitForCard() {
            let cardTitle = app.staticTexts["agent-title"]
            XCTAssertTrue(cardTitle.waitForExistence(timeout: 10))
            let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@ AND hittable == true", title), object: cardTitle)
            XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 10), .completed)
        }
        func browseAgent() {
            XCTAssertFalse(conversation.exists, "Dismiss the conversation before browsing agents")
            selectAgentFromList(app, title: title)
            waitForCard()
        }
        func closeConversation() {
            app.buttons["Done"].tap()
            gone(conversation)
            waitForCard()
        }
        func draftText() -> String {
            let input = composer(app)
            let value = input.value as? String ?? ""
            return value == input.placeholderValue ? "" : value
        }
        func replaceDraft(_ text: String) {
            let input = composer(app)
            input.tap()
            input.typeKey("a", modifierFlags: .command)
            input.typeText(text.isEmpty ? XCUIKeyboardKey.delete.rawValue : text)
            XCTAssertEqual(draftText(), text)
        }
        let options = XCTMeasureOptions()
        options.iterationCount = 3
        options.invocationOptions = [.manuallyStart, .manuallyStop]
        var metrics: [XCTMetric] = [XCTCPUMetric(application: app), XCTMemoryMetric(application: app)]
        if #available(iOS 26.0, macOS 26.0, *) { metrics.append(XCTHitchMetric(application: app)) }
        measure(metrics: metrics, options: options) {
            // Restore the same real agent and its history outside each interval.
            // Process-cold launch is measured by the separate launch benchmark.
            if conversation.exists { closeConversation() }
            XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "identifier == %@ AND label == %@", "connection", "Demo")).firstMatch.exists)
            browseAgent()
            app.staticTexts["agent-title"].tap()
            XCTAssertTrue(conversation.waitForExistence(timeout: 10))
            let message = conversation.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH %@", "message-")).firstMatch
            XCTAssertTrue(message.waitForExistence(timeout: 20), "Load actual conversation messages before measuring")
            XCTAssertTrue(composer(app).waitForExistence(timeout: 5))
            let originalDraft = draftText()
            defer {
                if composer(app).exists, draftText() != originalDraft { replaceDraft(originalDraft) }
            }

            startMeasuring()
            for _ in 0..<3 {
                conversation.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.3)).press(forDuration: 0.05,
                    thenDragTo: conversation.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.65)),
                    withVelocity: .slow, thenHoldForDuration: 0.2)
            }
            // The existing real title is temporary local input; never send or queue it.
            replaceDraft(title)
            replaceDraft("")
            for _ in 0..<3 {
                conversation.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.65)).press(forDuration: 0.05,
                    thenDragTo: conversation.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.3)),
                    withVelocity: .slow, thenHoldForDuration: 0.2)
            }
            closeConversation()
            XCTAssertFalse(conversation.exists)
            app.staticTexts["agent-title"].tap()
            XCTAssertTrue(conversation.waitForExistence(timeout: 10))
            closeConversation()
            browseAgent()
            app.staticTexts["agent-title"].tap()
            XCTAssertTrue(conversation.waitForExistence(timeout: 10))
            stopMeasuring()

            // Restore pre-existing local input outside the measured interval.
            // Automation wall time is not a UI-response latency metric.
            if draftText() != originalDraft { replaceDraft(originalDraft) }
            XCTAssertEqual(draftText(), originalDraft)
        }
    }

    private func capture(_ app: XCUIApplication, _ name: String) {
        // Native sheet/disclosure animations can outlive accessibility queries.
        // Capture their settled layout, not an intermediate clipped frame.
        Thread.sleep(forTimeInterval: 0.4)
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name; attachment.lifetime = .keepAlways; add(attachment)
    }
    private func scrollVisibleConversation(_ app: XCUIApplication, upward: Bool) {
        let conversation = app.scrollViews["conversation"]
        let frame = conversation.frame
        let top = frame.minY + 36
        let bottom = min(frame.maxY, composer(app).frame.minY) - 28
        let height = max(40, bottom - top)
        let origin = app.coordinate(withNormalizedOffset: .zero)
        let low = origin.withOffset(CGVector(dx: frame.midX, dy: top + height * 0.85))
        let high = origin.withOffset(CGVector(dx: frame.midX, dy: top + height * 0.15))
        (upward ? low : high).press(forDuration: 0.05, thenDragTo: upward ? high : low, withVelocity: .slow, thenHoldForDuration: 0.2)
    }
}
