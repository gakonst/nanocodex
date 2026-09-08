import XCTest

final class ContextUITests: XCTestCase {
    override func setUp() { super.setUp(); continueAfterFailure = false }
    private func launch() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["--demo"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString, "NANOCODEX_DEMO_FAIL_ONCE": "submit", "NANOCODEX_DEMO_COMPLETE_AFTER_MS": "120000"]
        app.launch()
        XCTAssertTrue(app.buttons["add-attachments"].waitForExistence(timeout: 10))
        return app
    }
    private func assistantText(_ app: XCUIApplication, matching predicate: NSPredicate) -> XCUIElement {
        app.scrollViews["conversation"].otherElements.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message-assistant-"))
            .staticTexts.matching(predicate).firstMatch
    }
    private func openContext(_ app: XCUIApplication) {
        app.buttons["add-attachments"].tap()
        XCTAssertTrue(app.buttons["Context from other apps"].waitForExistence(timeout: 5))
        app.buttons["Context from other apps"].tap()
        XCTAssertTrue(app.switches["context-enabled"].waitForExistence(timeout: 5))
    }
    private func toggleCapture(_ app: XCUIApplication) {
        let row = app.switches["context-enabled"]
        for _ in 0..<6 {
            if row.isHittable { break }
            app.swipeDown()
        }
        let control = row.switches.firstMatch
        if control.exists { control.tap() } else { row.tap() }
    }
    func testLiveShortcutsMessageCanBeQueriedThroughHand() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_CONTEXT_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in phone and NANOCODEX_CONTEXT_LIVE=1.")
        }
        let app = XCUIApplication(); app.launch()
        XCTAssertTrue(app.buttons["tab-overview"].waitForExistence(timeout: 30))
        openContext(app)
        if app.switches["context-enabled"].value as? String != "1" { toggleCapture(app) }
        let message = "museum" + String(UUID().uuidString.lowercased().filter { $0.isLetter }.prefix(10))
        try runCaptureShortcut(app, message: message)
        app.buttons["Done"].tap()
        app.buttons["new-conversation"].tap()
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.staticTexts["agent-title"].label == "New agent"
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 20), .completed)
        let title = "Message Hand check " + String(UUID().uuidString.prefix(8)) + ". Reply READY"
        print("Live message Hand UI conversation title: " + title)
        let composer = app.textFields["composer"]
        composer.tap(); composer.typeText(title); app.buttons["send"].tap()
        let seeded = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: self.assistantText(app, matching: NSPredicate(format: "label == %@", "READY")))
        XCTAssertEqual(XCTWaiter.wait(for: [seeded], timeout: 60), .completed)
        composer.tap()
        composer.typeText("Use my connected iPhone Hand to find the most recently captured message from Shared containing museum. Read it and reply with its complete text only.")
        app.buttons["send"].tap()
        let answer = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: self.assistantText(app, matching: NSPredicate(format: "label ==[c] %@", message)))
        XCTAssertEqual(XCTWaiter.wait(for: [answer], timeout: 120), .completed)
        attach(app, name: "context-live-phone-hand-answer")
    }
    private func runCaptureShortcut(_ app: XCUIApplication, message: String) throws {
        let shortcuts = XCUIApplication(bundleIdentifier: "com.apple.shortcuts")
        shortcuts.launch()
        let create = shortcuts.navigationBars.buttons["Create Shortcut"]
        if !create.waitForExistence(timeout: 2) {
            if shortcuts.buttons["Cancel"].exists { shortcuts.buttons["Cancel"].tap() }
            if shortcuts.navigationBars.buttons["BackButton"].exists { shortcuts.navigationBars.buttons["BackButton"].tap() }
        }
        XCTAssertTrue(create.waitForExistence(timeout: 10), shortcuts.navigationBars.debugDescription)
        create.tap()
        let search = shortcuts.searchFields.firstMatch
        XCTAssertTrue(search.waitForExistence(timeout: 10), shortcuts.debugDescription)
        let actionName = ProcessInfo.processInfo.environment["NANOCODEX_CONTEXT_ACTION_NAME"] ?? "Capture Context"
        search.tap(); search.typeText(actionName)
        let action = shortcuts.cells[actionName].firstMatch
        XCTAssertTrue(action.waitForExistence(timeout: 10), shortcuts.debugDescription)
        action.tap()
        let captureRow = shortcuts.otherElements.matching(NSPredicate(format: "label == %@", "Capture , Text,  from , Shared")).firstMatch
        XCTAssertTrue(captureRow.waitForExistence(timeout: 5), shortcuts.debugDescription)
        // Shortcuts exposes this parameter summary as one accessibility element.
        captureRow.coordinate(withNormalizedOffset: CGVector(dx: 0.42, dy: 0.25)).tap()
        XCTAssertTrue(shortcuts.keyboards.firstMatch.waitForExistence(timeout: 5), shortcuts.debugDescription)
        // The system parameter editor shows its keyboard without exposing a
        // focused text field. Tap real keys instead of relying on typeText.
        let keyboard = shortcuts.keyboards.firstMatch
        if keyboard.buttons["shift"].isSelected { keyboard.buttons["shift"].tap() }
        for character in message {
            keyboard.keys.matching(NSPredicate(format: "label ==[c] %@", String(character))).firstMatch.tap()
        }
        if shortcuts.buttons["Done"].exists { shortcuts.buttons["Done"].tap() }
        else { shortcuts.otherElements["editor"].coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.5)).tap() }
        attach(shortcuts, name: "context-shortcuts-action")
        app.terminate()
        shortcuts.buttons["play"].tap()
        attach(shortcuts, name: "context-shortcuts-executed")
        app.launch()
        openContext(app)
        let captured = app.staticTexts.matching(NSPredicate(format: "label ==[c] %@", message)).firstMatch
        for _ in 0..<5 {
            if captured.exists { break }
            app.swipeUp()
        }
        XCTAssertTrue(captured.waitForExistence(timeout: 10), app.debugDescription)
        attach(app, name: "context-shortcuts-captured-message")
    }
    private func capture(_ app: XCUIApplication, source: String, text: String) {
        app.buttons["Add context"].tap()
        let field = app.textFields["capture-source"]
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.tap()
        field.press(forDuration: 1)
        if app.menuItems["Select All"].waitForExistence(timeout: 2) { app.menuItems["Select All"].tap() }
        else { field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: (field.value as? String)?.count ?? 0)) }
        field.typeText(source)
        let content = app.textViews["capture-text"].exists ? app.textViews["capture-text"] : app.textFields["capture-text"]
        content.tap(); content.typeText(text)
        app.buttons["capture-save"].tap()
        // The saved captures follow setup and routing controls in the list.
        // Scroll the actual sheet before looking for a lazily materialized row.
        for _ in 0..<6 {
            if app.staticTexts[text].exists { break }
            app.swipeUp()
        }
        XCTAssertTrue(app.staticTexts[text].waitForExistence(timeout: 5))
    }
    func testCaptureSearchSelectAndDurableRetry() {
        let app = launch()
        openContext(app)
        toggleCapture(app)
        capture(app, source: "Instagram", text: "Dinner with Alex on Friday")
        capture(app, source: "Messages", text: "Train leaves at six")
        attach(app, name: "context-inbox")
        let search = app.searchFields.firstMatch
        for _ in 0..<6 {
            if search.exists && search.isHittable { break }
            app.swipeDown()
        }
        XCTAssertTrue(search.waitForExistence(timeout: 5), app.debugDescription)
        search.tap(); search.typeText("Alex")
        XCTAssertTrue(app.staticTexts["Dinner with Alex on Friday"].exists)
        XCTAssertFalse(app.staticTexts["Train leaves at six"].exists)
        app.staticTexts["Dinner with Alex on Friday"].tap()
        app.buttons["context-use"].tap()
        XCTAssertTrue(app.buttons["composer-context"].waitForExistence(timeout: 5))
        let composer = app.textFields["composer"].exists ? app.textFields["composer"] : app.textViews["composer"]
        composer.tap(); composer.typeText("Help me plan Friday")
        app.buttons["send"].tap()
        XCTAssertTrue(app.buttons["retry-pending"].waitForExistence(timeout: 5))
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["retry-pending"].waitForExistence(timeout: 10))
        app.buttons["retry-pending"].tap()
        // A restored demo intentionally fails once again, then succeeds.
        if app.buttons["retry-pending"].waitForExistence(timeout: 3) { app.buttons["retry-pending"].tap() }
        let conversation = app.scrollViews["conversation"]
        XCTAssertTrue(conversation.staticTexts["Help me plan Friday"].waitForExistence(timeout: 8))
        conversation.buttons["Captured context (1)"].tap()
        XCTAssertTrue(conversation.staticTexts["Dinner with Alex on Friday"].waitForExistence(timeout: 5))
        XCTAssertFalse(conversation.staticTexts["Train leaves at six"].exists)
        XCTAssertEqual(conversation.staticTexts.matching(identifier: "Help me plan Friday").count, 1)
        attach(app, name: "context-agent-message")
    }
    func testCaptureDisabledAndDemoAccountIsolation() {
        let app = launch()
        openContext(app)
        XCTAssertFalse(app.buttons["Add context"].isEnabled)
        toggleCapture(app)
        capture(app, source: "Messages", text: "Only for this account")
        toggleCapture(app)
        XCTAssertFalse(app.buttons["Add context"].isEnabled)
        app.terminate()
        app.launchEnvironment["NANOCODEX_DEMO_PROFILE"] = UUID().uuidString
        app.launch(); openContext(app)
        XCTAssertFalse(app.staticTexts["Only for this account"].exists)
        XCTAssertFalse(app.buttons["Add context"].isEnabled)
        attach(app, name: "context-isolated-account")
    }
    func testSafariShareReachesContextInbox() throws {
        guard #available(iOS 26.0, *) else { throw XCTSkip("This system share-sheet journey targets iOS 26.") }
        let app = launch()
        openContext(app)
        toggleCapture(app)
        try shareSafariPage(app)
    }
    private func shareSafariPage(_ app: XCUIApplication) throws {
        let link = "https://support.apple.com/guide/shortcuts/communication-triggers-apdd711f9dff/ios"
        let safari = XCUIApplication(bundleIdentifier: "com.apple.mobilesafari")
        safari.launch()
        let address = safari.textFields.firstMatch
        XCTAssertTrue(address.waitForExistence(timeout: 20), safari.debugDescription)
        address.tap()
        // iPad Safari replaces the tab title field with a focused editor.
        let editor = safari.textFields.matching(NSPredicate(format: "identifier BEGINSWITH %@", "SearchFieldItemView")).firstMatch
        if editor.waitForExistence(timeout: 2) { editor.typeText(link + XCUIKeyboardKey.return.rawValue) }
        else { address.typeText(link + XCUIKeyboardKey.return.rawValue) }
        let share = safari.buttons["Share"]
        if !share.waitForExistence(timeout: 3) {
            let more = safari.buttons["More"]
            XCTAssertTrue(more.waitForExistence(timeout: 20), safari.debugDescription)
            more.tap()
        }
        XCTAssertTrue(share.waitForExistence(timeout: 10), safari.debugDescription)
        let shareReady = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: share)
        XCTAssertEqual(XCTWaiter.wait(for: [shareReady], timeout: 30), .completed, safari.debugDescription)
        share.tap()
        attach(safari, name: "context-system-share-sheet")
        let sheet = safari.otherElements["ActivityListView"]
        XCTAssertTrue(sheet.waitForExistence(timeout: 10))
        let destinations = sheet.scrollViews.containing(.cell, identifier: "shareCell").firstMatch
        XCTAssertTrue(destinations.waitForExistence(timeout: 5))
        let shareName = ProcessInfo.processInfo.environment["NANOCODEX_CONTEXT_SHARE_NAME"] ?? "Nanocodex"
        let destination = sheet.descendants(matching: .any).matching(NSPredicate(format: "label == %@", shareName)).firstMatch
        let moreApps = destinations.cells.matching(NSPredicate(format: "label BEGINSWITH %@", "More")).firstMatch
        for _ in 0..<15 {
            if destination.exists && destination.isHittable { break }
            if moreApps.exists && moreApps.isHittable {
                moreApps.tap()
                break
            }
            destinations.swipeLeft()
        }
        let expandedDestination = safari.descendants(matching: .any).matching(NSPredicate(format: "label == %@", shareName)).firstMatch
        XCTAssertTrue(expandedDestination.waitForExistence(timeout: 5))
        let appList = safari.tables["tableView"]
        if appList.exists {
            for _ in 0..<10 {
                if expandedDestination.isHittable { break }
                appList.swipeUp()
            }
        }
        expandedDestination.tap()
        let save = safari.buttons["capture-save"]
        XCTAssertTrue(save.waitForExistence(timeout: 15), safari.debugDescription)
        XCTAssertTrue(save.isEnabled, safari.debugDescription)
        XCTAssertTrue(safari.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Message Contains")).firstMatch.waitForExistence(timeout: 10), safari.debugDescription)
        attach(safari, name: "context-safari-share")
        save.tap()
        app.activate()
        let captured = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "context-item-")).firstMatch
        for _ in 0..<6 { if captured.exists { break }; app.swipeUp() }
        XCTAssertTrue(captured.waitForExistence(timeout: 10), app.debugDescription)
        captured.tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Message Contains")).firstMatch.waitForExistence(timeout: 5), app.debugDescription)
        XCTAssertTrue(app.buttons["Open original"].exists || app.links["Open original"].exists)
        attach(app, name: "context-shared-page-text")
    }
    func testLiveSharedPageReachesAgent() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_CONTEXT_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in phone and NANOCODEX_CONTEXT_LIVE=1.")
        }
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.buttons["tab-overview"].waitForExistence(timeout: 30))
        app.buttons["new-conversation"].tap()
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.staticTexts["agent-title"].label == "New agent" && app.textFields["composer"].isEnabled
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 20), .completed)
        let title = "Context check " + String(UUID().uuidString.prefix(8)) + ". Reply READY"
        print("Live context UI conversation title: " + title)
        let composer = app.textFields["composer"]
        composer.tap(); composer.typeText(title); app.buttons["send"].tap()
        XCTAssertTrue(app.scrollViews["conversation"].waitForExistence(timeout: 10))
        let completed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: self.assistantText(app, matching: NSPredicate(format: "label == %@", "READY")))
        XCTAssertEqual(XCTWaiter.wait(for: [completed], timeout: 60), .completed)
        openContext(app)
        let toggle = app.switches["context-enabled"]
        if toggle.value as? String != "1" { toggleCapture(app) }
        try shareSafariPage(app)
        app.buttons["context-use"].tap()
        XCTAssertTrue(app.buttons["composer-context"].waitForExistence(timeout: 10))
        composer.tap()
        composer.typeText("From the captured page, what two criteria can a Message automation match? Reply with the two option names only. Do not use tools.")
        app.buttons["send"].tap()
        let answer = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            let text = app.scrollViews["conversation"].otherElements.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message-assistant-")).staticTexts.allElementsBoundByIndex.map(\.label).joined(separator: "\n").lowercased()
            return text.contains("sender") && text.contains("message contains")
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [answer], timeout: 90), .completed)
        attach(app, name: "context-live-page-answer")
        let disclosure = app.scrollViews["conversation"].buttons["Captured context (1)"]
        XCTAssertTrue(disclosure.waitForExistence(timeout: 10))
        disclosure.tap()
        XCTAssertTrue(app.scrollViews["conversation"].staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Message Contains")).firstMatch.waitForExistence(timeout: 5))
        attach(app, name: "context-live-message-provenance")
    }
    func testMessagingSetupExplainsCaptureAndPhoneQueries() {
        let app = launch()
        openContext(app)
        for source in ["Messages", "WhatsApp", "Instagram", "Signal"] {
            let setup = app.buttons["context-setup-" + source]
            for _ in 0..<3 {
                if setup.isHittable { break }
                app.swipeUp()
            }
            setup.tap()
            let instructions = source == "Messages" ? "Run Immediately" : "cannot read"
            XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", instructions)).firstMatch.waitForExistence(timeout: 5))
            XCTAssertTrue(app.staticTexts["Phone Hand"].exists)
            attach(app, name: "context-setup-" + source)
            app.navigationBars.buttons["BackButton"].tap()
        }
    }
    private func attach(_ app: XCUIApplication, name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name; attachment.lifetime = .keepAlways; add(attachment)
    }
}
