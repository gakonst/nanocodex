import XCTest
import UIKit

final class InboxUITests: XCTestCase {
    func testNativeBrowserResponsesWaitForActiveAndBackgroundCancels() {
        for outcome in ["success", "failure", "background"] {
            let app = launchNativeBrowserForm(arguments: ["--browser-native-form-inactive-response"]
                + (outcome == "failure" ? ["--browser-native-form-fill-fails"] : []))
            enterNativeBrowserFields(app)
            revealNativeFill(app).tap()
            let delivered = app.staticTexts["native-fixture-inactive-responses"]
            let inactive = expectation(for: NSPredicate(format: "label == 'Responses delivered inactive: 1'"), evaluatedWith: delivered)
            wait(for: [inactive], timeout: 5)
            XCTAssertEqual(app.staticTexts["native-fixture-actions"].label, "Fills: 1 · Site submits: 0 · Handoffs: 0")
            capture(app, "native-response-inactive-" + outcome)
            if outcome == "background" { app.buttons["Fixture background"].tap() }
            app.buttons["Fixture active"].tap()
            if outcome == "success" {
                XCTAssertTrue(app.buttons["Open native browser form"].waitForExistence(timeout: 5))
                XCTAssertEqual(app.staticTexts["native-fixture-sequence"].label, "fill_fields → finish")
                XCTAssertEqual(app.staticTexts["native-fixture-actions"].label, "Fills: 1 · Site submits: 0 · Handoffs: 1")
            } else {
                let message = outcome == "background" ? "Private view paused. Refresh to continue."
                    : "Couldn’t confirm the action. Refresh before continuing."
                XCTAssertTrue(app.staticTexts[message].waitForExistence(timeout: 5))
                XCTAssertFalse(app.secureTextFields["Password"].exists)
                let actions = app.staticTexts["native-fixture-actions"]
                let retry = expectation(for: NSPredicate(format: "label != 'Fills: 1 · Site submits: 0 · Handoffs: 0'"), evaluatedWith: actions)
                retry.isInverted = true
                wait(for: [retry], timeout: 2)
                XCTAssertTrue(app.buttons["Refresh"].isEnabled, "Discarding a response must release busy state")
                app.buttons["Refresh"].tap()
                XCTAssertTrue(app.secureTextFields["Password"].waitForExistence(timeout: 5))
                XCTAssertFalse(app.buttons["browser-native-fill"].isEnabled, "Recovery must not reuse private drafts")
            }
            capture(app, "native-response-resumed-" + outcome)
            app.terminate()
        }
    }

    func testNativeBrowserInactiveDraftsAndLoginCheckResume() {
        let app = launchNativeBrowserForm(arguments: ["--browser-native-form-inactive-response"])
        enterNativeBrowserFields(app)
        app.buttons["Fixture inactive"].tap()
        app.buttons["Fixture active"].tap()
        XCTAssertEqual(app.textFields["Email"].value as? String, "synthetic@example.com")
        revealNativeFill(app).tap()
        let delivered = app.staticTexts["native-fixture-inactive-responses"]
        let inactive = expectation(for: NSPredicate(format: "label == 'Responses delivered inactive: 1'"), evaluatedWith: delivered)
        wait(for: [inactive], timeout: 5)
        app.buttons["Fixture active"].tap()
        XCTAssertTrue(app.buttons["Open native browser form"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["native-fixture-filled"].exists, "Both drafts must survive transient inactivity")
        app.terminate()

        app.launchArguments = ["--browser-native-form-ui-fixture", "--browser-native-form-inactive-response", "--browser-native-form-login"]
        app.launch()
        let open = app.buttons["Open native browser form"]
        XCTAssertTrue(open.waitForExistence(timeout: 10)); open.tap()
        let checked = expectation(for: NSPredicate(format: "label == 'Responses delivered inactive: 1'"),
                                  evaluatedWith: app.staticTexts["native-fixture-inactive-responses"])
        wait(for: [checked], timeout: 5)
        XCTAssertEqual(app.staticTexts["native-fixture-observations"].label, "Observations: 0")
        app.buttons["Fixture active"].tap()
        XCTAssertTrue(app.textFields["Email"].waitForExistence(timeout: 5), "Resuming the consent check must start observation")
        enterNativeBrowserFields(app)
        revealNativeFill(app).tap()
        let filled = expectation(for: NSPredicate(format: "label == 'Responses delivered inactive: 2'"),
                                 evaluatedWith: app.staticTexts["native-fixture-inactive-responses"])
        wait(for: [filled], timeout: 5)
        app.buttons["Fixture active"].tap()
        XCTAssertTrue(open.waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts["native-fixture-sequence"].label, "fill_fields → finish")
        capture(app, "native-login-inactive-response-handed-back")
    }

    func testNativeBrowserFormFallsBackToLegacyBackendOnce() {
        let app = XCUIApplication()
        app.launchArguments = ["--browser-native-form-ui-fixture", "--browser-native-form-legacy"]
        app.launch()
        let open = app.buttons["Open native browser form"]
        XCTAssertTrue(open.waitForExistence(timeout: 10))
        open.tap()
        let showWebsite = app.buttons["browser-show-website"]
        XCTAssertTrue(showWebsite.waitForExistence(timeout: 5))
        XCTAssertFalse(app.descendants(matching: .any)["browser-private-viewport"].firstMatch.exists)
        showWebsite.tap()
        let viewport = app.descendants(matching: .any)["browser-private-viewport"].firstMatch
        XCTAssertTrue(viewport.waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts.matching(identifier: "native-fixture-observations").count, 1)
        let observationCount = app.staticTexts["native-fixture-observations"]
        let observed = expectation(for: NSPredicate(format: "label BEGINSWITH 'Observations: ' AND label != 'Observations: 0'"), evaluatedWith: observationCount)
        wait(for: [observed], timeout: 5)
        XCTAssertEqual(app.staticTexts["native-fixture-probes"].label, "Capability probes: 3")
        XCTAssertFalse(app.textFields["Email"].exists)
        let observations = app.staticTexts["native-fixture-observations"]
        let before = observations.label
        app.buttons["Refresh"].tap()
        let refreshed = expectation(for: NSPredicate(format: "label != %@", before), evaluatedWith: observations)
        wait(for: [refreshed], timeout: 5)
        XCTAssertEqual(app.staticTexts["native-fixture-probes"].label, "Capability probes: 3", "Legacy mode must survive refresh and polling")
        XCTAssertEqual(app.staticTexts["native-fixture-actions"].label, "Fills: 0 · Site submits: 0 · Handoffs: 0")
        XCTAssertFalse(app.staticTexts["Couldn’t confirm the action. Refresh before continuing."].exists)
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = "legacy-browser-capability-fallback"; attachment.lifetime = .keepAlways
        add(attachment)
        app.buttons["Hand back"].tap()
        XCTAssertTrue(open.waitForExistence(timeout: 5))
    }

    func testNativeBrowserCodeUsesSameFillAndHandoffJourney() {
        let app = XCUIApplication()
        app.launchArguments = ["--browser-native-form-ui-fixture", "--browser-native-form-otp"]
        app.launch()
        let open = app.buttons["Open native browser form"]
        XCTAssertTrue(open.waitForExistence(timeout: 10))
        open.tap()
        let code = app.textFields["Verification code"]
        XCTAssertTrue(code.waitForExistence(timeout: 5))
        code.tap(); code.typeText("123456")
        XCTAssertFalse(app.descendants(matching: .any)["browser-private-viewport"].firstMatch.exists)
        revealNativeFill(app).tap()
        XCTAssertTrue(open.waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts["native-fixture-sequence"].label, "fill_fields → finish")
        XCTAssertEqual(app.staticTexts["native-fixture-actions"].label, "Fills: 1 · Site submits: 0 · Handoffs: 1")
    }

    func testNativeBrowserFormKeepsNativeFieldsWhenOnlyHintsAreUnsupported() {
        for (argument, probes) in [("--browser-native-form-no-hints", 3), ("--browser-native-form-no-controls", 2)] {
            let app = launchNativeBrowserForm(arguments: [argument])
            XCTAssertEqual(app.staticTexts["native-fixture-probes"].label, "Capability probes: \(probes)")
            XCTAssertFalse(app.descendants(matching: .any)["browser-private-viewport"].firstMatch.exists)
            enterNativeBrowserFields(app)
            revealNativeFill(app).tap()
            XCTAssertTrue(app.buttons["Open native browser form"].waitForExistence(timeout: 5))
            XCTAssertEqual(app.staticTexts["native-fixture-sequence"].label, "fill_fields → finish")
            app.terminate()
        }
    }

    func testAgentSelectedNativeChoicesNotesAndCheckboxFillTogether() {
        let app = XCUIApplication()
        app.launchArguments = ["--browser-native-form-ui-fixture", "--browser-native-form-mixed"]
        app.launch()
        let open = app.buttons["Open native browser form"]
        XCTAssertTrue(open.waitForExistence(timeout: 10)); open.tap()
        XCTAssertTrue(app.staticTexts["Complete the profile fields on this page."].waitForExistence(timeout: 5))
        app.buttons["Sheet Grabber"].swipeUp()
        let choice = app.buttons["browser-native-choice:Country"]
        for _ in 0..<5 {
            if choice.isHittable { break }
            app.collectionViews.firstMatch.swipeUp()
        }
        choice.tap()
        app.buttons["Greece"].tap()
        let toggle = app.switches["browser-native-check:Send updates"]
        for _ in 0..<5 {
            if toggle.isHittable { break }
            app.collectionViews.firstMatch.swipeUp()
        }
        XCTAssertEqual(toggle.value as? String, "0")
        // A SwiftUI Toggle exposes the entire labelled row as a switch. Tap
        // the trailing native control, then verify the actual changed value.
        toggle.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.5)).tap()
        XCTAssertEqual(toggle.value as? String, "1")
        let notes = app.textViews["Notes"]
        for _ in 0..<5 {
            if notes.isHittable { break }
            app.collectionViews.firstMatch.swipeDown()
        }
        notes.tap(); notes.typeText("Line one\nLine two")
        XCTAssertEqual(notes.value as? String, "Line one\nLine two")
        XCTAssertFalse(app.descendants(matching: .any)["browser-private-viewport"].firstMatch.exists)
        revealNativeFill(app).tap()
        XCTAssertTrue(open.waitForExistence(timeout: 5), app.debugDescription)
        XCTAssertTrue(app.staticTexts["native-fixture-filled"].exists)
        XCTAssertEqual(app.staticTexts["native-fixture-actions"].label, "Fills: 1 · Site submits: 0 · Handoffs: 1")
        XCTAssertEqual(app.staticTexts["native-fixture-sequence"].label, "fill_fields → finish")
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = "agent-selected-native-profile-handed-back"; attachment.lifetime = .keepAlways
        add(attachment)
    }

    func testNativeCheckboxOnlyConfirmsDisplayedStateWithoutToggling() {
        for rerenders in [false, true] {
            let app = XCUIApplication()
            app.launchArguments = ["--browser-native-form-ui-fixture", "--browser-native-form-checkbox"]
                + (rerenders ? ["--browser-native-form-after-fill-stale"] : [])
            app.launch()
            let open = app.buttons["Open native browser form"]
            XCTAssertTrue(open.waitForExistence(timeout: 10)); open.tap()
            XCTAssertTrue(app.switches["browser-native-check:Keep preference"].waitForExistence(timeout: 5))
            XCTAssertEqual(app.switches["browser-native-check:Keep preference"].value as? String, "1")
            revealNativeFill(app).tap()
            XCTAssertTrue(open.waitForExistence(timeout: 5))
            XCTAssertTrue(app.staticTexts["native-fixture-filled"].exists)
            XCTAssertEqual(app.staticTexts["native-fixture-sequence"].label, "fill_fields → finish")
            XCTAssertEqual(app.staticTexts["native-fixture-outcome"].label, "Input outcome: finished")
            app.terminate()
        }
    }

    func testStaleNativeSelectionHandsBackWithoutFillAndRetriesOnlyHandoff() {
        for failsOnce in [false, true] {
            let app = XCUIApplication()
            app.launchArguments = ["--browser-native-form-ui-fixture", "--browser-native-form-stale"]
                + (failsOnce ? ["--browser-native-form-finish-fails"] : [])
            app.launch()
            let open = app.buttons["Open native browser form"]
            XCTAssertTrue(open.waitForExistence(timeout: 10)); open.tap()
            if failsOnce {
                let retry = app.buttons["browser-stale-handback"]
                XCTAssertTrue(retry.waitForExistence(timeout: 5))
                let enabled = expectation(for: NSPredicate(format: "enabled == true"), evaluatedWith: retry)
                wait(for: [enabled], timeout: 5)
                XCTAssertEqual(app.staticTexts["native-fixture-outcome"].label, "Input outcome: none")
                XCTAssertEqual(app.staticTexts["native-fixture-actions"].label, "Fills: 0 · Site submits: 0 · Handoffs: 1")
                let sequence = app.staticTexts["native-fixture-sequence"]
                let automaticRetry = expectation(for: NSPredicate(format: "label != %@", sequence.label), evaluatedWith: sequence)
                automaticRetry.isInverted = true
                wait(for: [automaticRetry], timeout: 2)
                retry.tap()
            }
            XCTAssertTrue(open.waitForExistence(timeout: 5))
            XCTAssertEqual(app.staticTexts["native-fixture-outcome"].label, "Input outcome: page_changed")
            XCTAssertEqual(app.staticTexts["native-fixture-actions"].label, "Fills: 0 · Site submits: 0 · Handoffs: \(failsOnce ? 2 : 1)")
            XCTAssertEqual(app.staticTexts["native-fixture-sequence"].label, failsOnce ? "finish → finish" : "finish")
            XCTAssertFalse(app.descendants(matching: .any)["browser-private-viewport"].firstMatch.exists)
            let attachment = XCTAttachment(screenshot: app.screenshot())
            attachment.name = failsOnce ? "stale-native-selection-retry" : "stale-native-selection-auto-handoff"
            attachment.lifetime = .keepAlways; add(attachment)
            app.terminate()
        }
    }

    func testNativeBrowserFormFillsOnceAndHandsBackWithoutWebsiteSubmit() {
        let app = launchNativeBrowserForm()
        let email = app.textFields["Email"]
        let password = app.secureTextFields["Password"]
        email.tap(); email.typeText("discarded@example.com")
        password.tap(); password.typeText("discarded-password")
        let observationsBeforeBackground = app.staticTexts["native-fixture-observations"].label
        XCUIDevice.shared.press(.home)
        XCTAssertTrue(app.wait(for: .runningBackground, timeout: 5))
        app.activate()
        dismissNativePasswordSavePrompt(app)
        XCTAssertTrue(app.staticTexts["Private view paused. Refresh to continue."].waitForExistence(timeout: 5))
        XCTAssertFalse(password.exists, "Backgrounding must remove the private form until explicit refresh")
        XCTAssertEqual(app.staticTexts["native-fixture-observations"].label, observationsBeforeBackground,
                       "Returning to the app must not resume browser observation automatically")
        app.buttons["Refresh"].tap()
        XCTAssertTrue(email.waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["browser-native-fill"].isEnabled, "Backgrounding must discard all drafts")
        XCTAssertTrue(["", "Email"].contains(email.value as? String ?? ""))
        XCTAssertTrue(["", "Password"].contains(password.value as? String ?? ""))
        enterNativeBrowserFields(app)
        let observations = app.staticTexts["native-fixture-observations"]
        let before = observations.label
        let polling = expectation(for: NSPredicate(format: "label != %@", before), evaluatedWith: observations)
        polling.isInverted = true
        wait(for: [polling], timeout: 2)
        revealNativeFill(app).tap()
        XCTAssertTrue(app.buttons["Open native browser form"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["native-fixture-filled"].exists)
        XCTAssertEqual(app.staticTexts["native-fixture-actions"].label, "Fills: 1 · Site submits: 0 · Handoffs: 1")
        XCTAssertEqual(app.staticTexts["native-fixture-sequence"].label, "fill_fields → finish")
        XCTAssertFalse(password.exists)
        XCTAssertFalse(app.descendants(matching: .any)["browser-private-viewport"].firstMatch.exists)
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = "native-fields-filled-and-handed-back"; attachment.lifetime = .keepAlways
        add(attachment)
    }

    func testNativeBrowserFormHandoffFailureDoesNotRepeatFill() {
        let app = launchNativeBrowserForm(arguments: ["--browser-native-form-finish-fails"])
        enterNativeBrowserFields(app)
        revealNativeFill(app).tap()
        let retry = app.buttons["browser-native-handback"]
        XCTAssertTrue(retry.waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Fields were filled, but handoff wasn’t confirmed. Hand back again to continue without refilling."].waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts["native-fixture-actions"].label, "Fills: 1 · Site submits: 0 · Handoffs: 1")
        XCTAssertFalse(app.secureTextFields["Password"].exists)
        XCTAssertFalse(app.descendants(matching: .any)["browser-private-viewport"].firstMatch.exists)
        let sequence = app.staticTexts["native-fixture-sequence"]
        let automaticRetry = expectation(for: NSPredicate(format: "label != %@", sequence.label), evaluatedWith: sequence)
        automaticRetry.isInverted = true
        wait(for: [automaticRetry], timeout: 2)
        retry.tap()
        XCTAssertTrue(app.buttons["Open native browser form"].waitForExistence(timeout: 5))
        XCTAssertEqual(sequence.label, "fill_fields → finish → finish")
        XCTAssertEqual(app.staticTexts["native-fixture-actions"].label, "Fills: 1 · Site submits: 0 · Handoffs: 2")
    }

    func testNativeBrowserFormWebsiteFallbackIsExplicitAndClearsDrafts() {
        let app = launchNativeBrowserForm()
        enterNativeBrowserFields(app)
        let showWebsite = app.buttons["browser-show-website"]
        for _ in 0..<4 {
            if showWebsite.isHittable { break }
            app.collectionViews.firstMatch.swipeUp()
        }
        showWebsite.tap()
        XCTAssertTrue(app.descendants(matching: .any)["browser-private-viewport"].firstMatch.waitForExistence(timeout: 5))
        dismissNativePasswordSavePrompt(app)
        let fields = app.buttons["Fields"]
        let ready = expectation(for: NSPredicate(format: "enabled == true AND hittable == true"), evaluatedWith: fields)
        wait(for: [ready], timeout: 5)
        fields.tap()
        let password = app.secureTextFields["Password"]
        XCTAssertTrue(password.waitForExistence(timeout: 5))
        XCTAssertTrue(["", "Email"].contains(app.textFields["Email"].value as? String ?? ""))
        XCTAssertTrue(["", "Password"].contains(password.value as? String ?? ""))
        XCTAssertFalse(app.descendants(matching: .any)["browser-private-viewport"].firstMatch.exists)
        XCTAssertFalse(app.buttons["browser-native-fill"].isEnabled)
        XCTAssertEqual(app.staticTexts["native-fixture-actions"].label, "Fills: 0 · Site submits: 0 · Handoffs: 0")
    }

    func testNativeBrowserFormFailureClearsDraftsWithoutRetry() {
        let app = launchNativeBrowserForm(arguments: ["--browser-native-form-fill-fails"])
        enterNativeBrowserFields(app)
        revealNativeFill(app).tap()
        XCTAssertTrue(app.staticTexts["Couldn’t confirm the action. Refresh before continuing."].waitForExistence(timeout: 5))
        XCTAssertFalse(app.secureTextFields["Password"].exists)
        let actions = app.staticTexts["native-fixture-actions"]
        XCTAssertEqual(actions.label, "Fills: 1 · Site submits: 0 · Handoffs: 0")
        let retry = expectation(for: NSPredicate(format: "label != %@", actions.label), evaluatedWith: actions)
        retry.isInverted = true
        wait(for: [retry], timeout: 2)
        app.buttons["Refresh"].tap()
        XCTAssertTrue(app.secureTextFields["Password"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["browser-native-fill"].isEnabled)
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = "native-fill-failure-cleared-drafts"; attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func launchNativeBrowserForm(arguments: [String] = []) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["--browser-native-form-ui-fixture"] + arguments
        app.launch()
        let open = app.buttons["Open native browser form"]
        XCTAssertTrue(open.waitForExistence(timeout: 10))
        open.tap()
        XCTAssertTrue(app.textFields["Email"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts.matching(identifier: "native-fixture-actions").count, 1,
                       "Only the presented sheet should expose fixture evidence")
        return app
    }
    private func dismissNativePasswordSavePrompt(_ app: XCUIApplication) {
        // Removing password fields can present iOS's save prompt. Dismiss the
        // system interruption explicitly so the next tap reaches the sheet.
        for owner in [app, XCUIApplication(bundleIdentifier: "com.apple.springboard")] {
            // iOS 26 exposes this system panel as a Sheet; older versions use
            // Alert. Match only that named prompt, never arbitrary interruptions.
            for prompt in [owner.sheets["Save Password?"], owner.alerts["Save Password?"]] {
                if prompt.waitForExistence(timeout: 2) {
                    prompt.buttons["Not Now"].tap()
                    let dismissed = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: prompt)
                    wait(for: [dismissed], timeout: 5)
                    return
                }
            }
        }
    }
    private func enterNativeBrowserFields(_ app: XCUIApplication) {
        let email = app.textFields["Email"]
        email.tap(); email.typeText("synthetic@example.com")
        let password = app.secureTextFields["Password"]
        password.tap(); password.typeText("synthetic-password")
        XCTAssertNotEqual(password.value as? String, "synthetic-password")
    }
    private func revealNativeFill(_ app: XCUIApplication) -> XCUIElement {
        let fill = app.buttons["browser-native-fill"]
        for _ in 0..<4 {
            if fill.isHittable { break }
            app.collectionViews.firstMatch.swipeUp()
        }
        XCTAssertTrue(fill.isEnabled)
        return fill
    }

    func testPrivateLoginUsesNativeSecureReviewPane() {
        let app = XCUIApplication()
        app.launchArguments = ["--browser-login-ui-fixture"]
        app.launch()
        let open = app.buttons["browser-login-open"]
        XCTAssertTrue(open.waitForExistence(timeout: 10))
        open.tap()
        XCTAssertTrue(app.staticTexts["https://auth.example.com"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["Done"].exists)
        XCTAssertTrue(app.buttons["browser-login-approve"].exists)
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = "native-private-login-review"; attachment.lifetime = .keepAlways
        add(attachment)
        app.buttons["browser-login-cancel"].tap()
        XCTAssertTrue(app.staticTexts["Private sign-in cancelled"].waitForExistence(timeout: 5))
        open.tap()
        app.buttons["browser-login-approve"].tap()
        XCTAssertTrue(app.staticTexts["Private browser approved"].waitForExistence(timeout: 5))
    }

    func testNativeCommandReviewAndDeniedAuthentication() throws {
        let app = XCUIApplication()
        app.launchArguments = ["--native-secure-input-ui-fixture"]
        app.launch()
        let open = app.buttons["secure-input-open"]
        XCTAssertTrue(open.waitForExistence(timeout: 5))
        XCTAssertFalse(app.secureTextFields["secure-input-password"].exists)
        let conversation = XCTAttachment(screenshot: app.screenshot())
        conversation.name = "native-password-conversation"; conversation.lifetime = .keepAlways
        add(conversation)
        open.tap()
        XCTAssertTrue(app.staticTexts["Machine: fixture-machine"].waitForExistence(timeout: 5))
        let sheet = app.descendants(matching: .any)["secure-input-sheet"].firstMatch
        XCTAssertTrue(sheet.exists)
        let presentation = XCTAttachment(screenshot: app.screenshot())
        presentation.name = "native-password-conversation-sheet"; presentation.lifetime = .keepAlways
        add(presentation)
        // Expand the native sheet before reviewing all command details.
        sheet.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.02))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.08)))
        let executable = app.descendants(matching: .any)["native-secure-command-executable"].firstMatch
        XCTAssertTrue(executable.waitForExistence(timeout: 5))
        XCTAssertTrue(executable.label.contains("\"/usr/bin/id\""), executable.label)
        let cwd = app.descendants(matching: .any)["native-secure-command-cwd"].firstMatch
        XCTAssertTrue(cwd.exists)
        XCTAssertTrue(cwd.label.contains("\"/\""), cwd.label)
        XCTAssertTrue(app.staticTexts["Local user ID: 501"].exists)
        let arguments = app.descendants(matching: .any)["native-secure-command-arguments"].firstMatch
        if !arguments.isHittable { app.scrollViews["secure-input-review"].swipeUp() }
        XCTAssertTrue(arguments.exists)
        XCTAssertTrue(arguments.label.contains("\\u202e"))
        XCTAssertFalse(arguments.label.contains("\u{202e}"))
        let run = app.buttons["secure-input-submit"]
        XCTAssertEqual(run.label, "Authenticate & run as root")
        XCTAssertFalse(run.isEnabled)
        let confirmation = app.switches["native-secure-command-confirm"]
        if !confirmation.isHittable { app.scrollViews["secure-input-review"].swipeUp() }
        XCTAssertTrue(confirmation.waitForExistence(timeout: 5))
        confirmation.tap()
        XCTAssertFalse(run.isEnabled, "Explicit review alone must not enable a root command")
        let field = app.secureTextFields["secure-input-password"]
        if !field.isHittable { app.scrollViews["secure-input-review"].swipeUp() }
        field.tap(); field.typeText("synthetic-native-secret")
        XCTAssertTrue(run.isEnabled)
        run.tap()
        XCTAssertTrue(app.staticTexts["Authentication denied"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Submission attempts: 0"].exists)
        XCTAssertFalse(app.staticTexts["synthetic-native-secret"].exists)
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = "native-command-auth-denied"; attachment.lifetime = .keepAlways
        add(attachment)
        app.buttons["Cancel"].tap()
        XCTAssertTrue(open.waitForExistence(timeout: 5))
        XCTAssertTrue(open.isHittable)
        XCTAssertFalse(field.exists)
        let dismissed = XCTAttachment(screenshot: app.screenshot())
        dismissed.name = "native-password-back-to-conversation"; dismissed.lifetime = .keepAlways
        add(dismissed)
    }

    func testPrivatePasswordFieldAndSafeReceipt() throws {
        let app = XCUIApplication()
        app.launchArguments = ["--secure-input-ui-fixture"]
        app.launch()
        app.buttons["secure-input-open"].tap()
        let field = app.secureTextFields["secure-input-field:password"]
        XCTAssertTrue(field.waitForExistence(timeout: 5), "Password input must use native secure text entry")
        field.tap(); field.typeText("synthetic-password")
        XCTAssertNotEqual(field.value as? String, "synthetic-password")
        XCTAssertEqual(app.buttons["secure-input-submit"].label, "Send password to website")
        app.buttons["secure-input-submit"].tap()
        XCTAssertTrue(app.staticTexts["Sensitive fields filled in browser."].waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["synthetic-password"].exists)
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = "secure-input-safe-receipt"; attachment.lifetime = .keepAlways
        add(attachment)
    }

    func testPrivateCardFormFillsOnlyBoundFields() throws {
        let app = XCUIApplication()
        app.launchArguments = ["--card-secure-input-ui-fixture"]
        app.launch()
        let open = app.buttons["secure-input-open"]
        XCTAssertTrue(open.waitForExistence(timeout: 5))
        open.tap()
        let sheet = app.descendants(matching: .any)["secure-input-sheet"].firstMatch
        XCTAssertTrue(sheet.waitForExistence(timeout: 5))
        sheet.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.02))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.08)))
        let submit = app.buttons["secure-input-submit"]
        XCTAssertEqual(submit.label, "Fill fields only")
        XCTAssertFalse(submit.isEnabled)
        let samples = [("card", "4242424242424242"), ("expiry", "12/30"), ("cvc", "123")]
        for (id, sample) in samples {
            let field = app.secureTextFields["secure-input-field:" + id]
            XCTAssertTrue(field.exists)
            if !field.isHittable { app.scrollViews["secure-input-review"].swipeUp() }
            field.tap(); field.typeText(sample)
            XCTAssertNotEqual(field.value as? String, sample)
            XCTAssertFalse(app.staticTexts[sample].exists)
        }
        XCTAssertTrue(submit.isEnabled)
        submit.tap()
        XCTAssertTrue(app.staticTexts["Sensitive fields filled in browser."].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Fixture: 3 bound fields filled; form submissions: 0"].exists)
        XCTAssertFalse(submit.isEnabled)
        XCTAssertFalse(app.keyboards.firstMatch.exists)
        for (id, sample) in samples {
            let field = app.secureTextFields["secure-input-field:" + id]
            let remaining = field.value as? String ?? ""
            XCTAssertTrue(remaining.isEmpty || remaining == field.placeholderValue, "Private field must be cleared")
            XCTAssertFalse(app.staticTexts[sample].exists)
        }
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = "private-card-fields-filled-only"; attachment.lifetime = .keepAlways
        add(attachment)
        app.buttons["Cancel"].tap()
        XCTAssertTrue(open.waitForExistence(timeout: 5))
        XCTAssertFalse(app.secureTextFields["secure-input-field:card"].exists)
    }

    // CRM failure scenarios: failed fetch must be retryable; filters must not retain
    // old rows; profile facts/notes must render; relationships must open their target.
    func testCRMBrowseAndRelationshipNavigation() {
        let app = XCUIApplication()
        app.launchEnvironment["NANOCODEX_STARTUP_FIXTURE"] = "1"
        app.launchEnvironment["NANOCODEX_STARTUP_PROFILE"] = UUID().uuidString
        app.launchEnvironment["NANOCODEX_CRM_RETRY_FIXTURE"] = "1"
        app.launch()
        XCTAssertTrue(app.buttons["main-tab-crm"].waitForExistence(timeout: 25))
        app.buttons["main-tab-crm"].tap()
        XCTAssertTrue(app.buttons["Retry"].waitForExistence(timeout: 10))
        app.buttons["Retry"].tap()
        XCTAssertTrue(app.buttons["crm-record-alex"].waitForExistence(timeout: 10))
        capture(app, "crm-directory")
        app.buttons["crm-filter-company"].tap()
        XCTAssertTrue(app.buttons["crm-record-studio"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["crm-record-alex"].exists)
        app.buttons["crm-filter-person"].tap()
        XCTAssertTrue(app.buttons["crm-record-alex"].waitForExistence(timeout: 5))
        let search = app.textFields["crm-search"]
        search.tap(); search.typeText("missing")
        XCTAssertTrue(app.staticTexts["No matches"].waitForExistence(timeout: 5))
        search.tap(); search.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: 7))
        XCTAssertTrue(app.buttons["crm-record-alex"].waitForExistence(timeout: 5))
        app.buttons["crm-record-alex"].tap()
        XCTAssertTrue(app.staticTexts["Example University"].waitForExistence(timeout: 5))
        let invite = app.descendants(matching: .any)["crm-timeline-calendar_meeting-invite"].firstMatch
        XCTAssertTrue(invite.waitForExistence(timeout: 5))
        XCTAssertTrue(invite.label.contains("Scheduled / invited"))
        XCTAssertTrue(invite.label.contains("Attendance unconfirmed"))
        XCTAssertFalse(invite.label.contains("Met"))
        XCTAssertTrue(app.descendants(matching: .any)["crm-timeline-interaction-proposal"].firstMatch.exists)
        capture(app, "crm-profile")
        for _ in 0..<4 where !app.staticTexts["Met at the design workshop."].isHittable { app.swipeUp() }
        XCTAssertTrue(app.staticTexts["Met at the design workshop."].exists)
        for _ in 0..<4 where !app.buttons["crm-related-sam"].isHittable { app.swipeDown() }
        app.buttons["crm-related-sam"].tap()
        XCTAssertTrue(app.staticTexts["Sam Rivera"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.descendants(matching: .any)["crm-timeline-empty"].firstMatch.waitForExistence(timeout: 5))
        capture(app, "crm-related-profile")
    }

    // The app selector belongs to the shell, not a directory or pushed profile.
    // Switching apps from a nested profile must leave that navigation stack.
    func testAppSelectorSurvivesCRMProfileNavigation() {
        let app = XCUIApplication()
        app.launchEnvironment["NANOCODEX_STARTUP_FIXTURE"] = "1"
        app.launchEnvironment["NANOCODEX_STARTUP_PROFILE"] = UUID().uuidString
        app.launch()
        defer { XCUIDevice.shared.orientation = .portrait }

        func assertSelector(_ name: String) {
            capture(app, name)
            for id in ["main-tab-todo", "main-tab-chat", "main-tab-crm", "main-tab-meetings", "main-tab-apps"] {
                let button = app.buttons[id]
                XCTAssertTrue(button.waitForExistence(timeout: 5), id)
                XCTAssertEqual(app.buttons.matching(identifier: id).count, 1, "One persistent selector: " + id)
                XCTAssertTrue(button.isHittable, id)
                XCTAssertTrue(app.frame.contains(button.frame), id)
            }
        }

        XCTAssertTrue(app.buttons["main-tab-crm"].waitForExistence(timeout: 25))
        app.buttons["main-tab-crm"].tap()
        XCTAssertTrue(app.buttons["crm-record-alex"].waitForExistence(timeout: 10))
        assertSelector("selector-crm-directory")
        let globalInput = app.textViews["new-thread-composer"]
        XCTAssertTrue(globalInput.waitForExistence(timeout: 5))
        globalInput.tap(); globalInput.typeText("Keep this new thread draft")
        app.buttons["main-tab-crm"].tap()
        app.buttons["crm-record-alex"].tap()
        XCTAssertTrue(app.staticTexts["Example University"].waitForExistence(timeout: 5))
        assertSelector("selector-crm-person")
        XCTAssertEqual(app.textViews["new-thread-composer"].value as? String, "Keep this new thread draft")
        XCTAssertTrue(app.textViews["new-thread-composer"].isHittable)
        for _ in 0..<4 where !app.buttons["crm-related-sam"].isHittable { app.swipeUp() }
        app.buttons["crm-related-sam"].tap()
        XCTAssertTrue(app.staticTexts["Sam Rivera"].waitForExistence(timeout: 5))
        assertSelector("selector-crm-related-person")
        XCTAssertEqual(app.textViews["new-thread-composer"].value as? String, "Keep this new thread draft")
        app.swipeUp()
        assertSelector("selector-crm-related-scrolled")
        XCUIDevice.shared.orientation = .landscapeLeft
        let landscape = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in app.frame.width > app.frame.height }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [landscape], timeout: 5), .completed)
        assertSelector("selector-crm-related-landscape")
        XCUIDevice.shared.orientation = .portrait
        let portrait = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in app.frame.width < app.frame.height }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [portrait], timeout: 5), .completed)

        app.buttons["main-tab-chat"].tap()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.staticTexts["Example University"].exists)
        let input = composer(app)
        XCTAssertTrue(input.waitForExistence(timeout: 10))
        input.tap(); input.typeText("Keep this unsent navigation draft")
        assertSelector("selector-chat-keyboard")
        app.buttons["main-tab-crm"].tap()
        XCTAssertTrue(app.buttons["crm-record-alex"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["crm-related-sam"].exists, "Tab switches must leave the pushed profile")
        app.buttons["main-tab-chat"].tap()
        XCTAssertTrue(composer(app).waitForExistence(timeout: 10))
        XCTAssertEqual(composer(app).value as? String, "Keep this unsent navigation draft")
        app.buttons["main-tab-apps"].tap()
        app.buttons["Your apps"].tap()
        XCTAssertTrue(app.buttons["create-generated-app"].waitForExistence(timeout: 10))
        assertSelector("selector-your-apps")
        XCTAssertEqual(app.textViews["new-thread-composer"].value as? String, "Keep this new thread draft")
        app.buttons["main-tab-meetings"].tap()
        XCTAssertTrue(app.textFields["meetings-search"].waitForExistence(timeout: 10))
        assertSelector("selector-meetings")
        XCTAssertEqual(app.textViews["new-thread-composer"].value as? String, "Keep this new thread draft")
        app.buttons["main-tab-todo"].tap()
        assertUnifiedInbox(app)
        assertSelector("selector-todo")
        XCTAssertEqual(app.textViews["new-thread-composer"].value as? String, "Keep this new thread draft")
        app.terminate(); app.launch()
        XCTAssertTrue(app.textViews["new-thread-composer"].waitForExistence(timeout: 25))
        XCTAssertEqual(app.textViews["new-thread-composer"].value as? String, "Keep this new thread draft")
        app.buttons["main-tab-chat"].tap()
        XCTAssertTrue(composer(app).waitForExistence(timeout: 10))
        XCTAssertEqual(composer(app).value as? String, "Keep this unsent navigation draft")
        capture(app, "selector-independent-drafts-after-relaunch")
    }

    func testCRMHistoryPaginationAndCalendarStatuses() {
        let app = XCUIApplication()
        app.launchEnvironment["NANOCODEX_STARTUP_FIXTURE"] = "1"
        app.launchEnvironment["NANOCODEX_STARTUP_PROFILE"] = UUID().uuidString
        app.launch()
        XCTAssertTrue(app.buttons["main-tab-crm"].waitForExistence(timeout: 25))
        app.buttons["main-tab-crm"].tap()
        XCTAssertTrue(app.buttons["crm-record-alex"].waitForExistence(timeout: 10))
        app.buttons["crm-record-alex"].tap()
        let more = app.buttons["crm-timeline-more"]
        XCTAssertTrue(more.waitForExistence(timeout: 10))
        for _ in 0..<5 where !more.isHittable { app.swipeUp() }
        more.tap()
        let canceled = app.descendants(matching: .any)["crm-timeline-calendar_meeting-cancelled-invite"].firstMatch
        let declined = app.descendants(matching: .any)["crm-timeline-calendar_meeting-declined-invite"].firstMatch
        XCTAssertTrue(canceled.waitForExistence(timeout: 10))
        XCTAssertTrue(canceled.label.contains("Canceled"))
        XCTAssertTrue(declined.label.contains("Declined"))
        XCTAssertTrue(app.descendants(matching: .any)["crm-timeline-email-email-note"].firstMatch.label.contains("Import date"))
        XCTAssertFalse(more.exists)
    }

    private func selectedConversationTab(_ app: XCUIApplication) -> XCUIElement {
        app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@ AND selected == true", "conversation-title:")).firstMatch
    }
    override func setUp() { super.setUp(); continueAfterFailure = false }

    // A wide markdown table must not widen the transcript or hide the final
    // paragraph beneath the composer, before or after the keyboard appears.
    func testWideTableKeepsTailAboveComposer() {
        let app = launch(["NANOCODEX_DEMO_WIDE_TABLE": "1"])
        let input = composer(app)
        let tail = app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "End of table review")).firstMatch
        XCTAssertTrue(tail.waitForExistence(timeout: 10))
        let table = app.scrollViews["markdown-table"].firstMatch
        XCTAssertTrue(table.waitForExistence(timeout: 5))
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        for _ in 0..<5 {
            if table.staticTexts["Library"].isHittable { break }
            conversation.swipeDown()
        }
        XCTAssertTrue(table.staticTexts["Library"].isHittable)
        table.swipeLeft()
        let license = table.staticTexts["License"]
        XCTAssertTrue(license.isHittable, "The final column must be reachable inside the table")
        capture(app, "wide-table-final-columns")
        table.swipeRight()
        let latest = app.buttons["latest-messages"]
        if latest.isHittable { latest.tap() }
        for typing in [false, true] {
            if typing { input.tap(); input.typeText("Keep this draft") }
            let visibleTail = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
                tail.exists && tail.frame.maxY <= input.frame.minY && tail.frame.minX >= app.frame.minX && tail.frame.maxX <= app.frame.maxX
            }, object: nil)
            XCTAssertEqual(XCTWaiter.wait(for: [visibleTail], timeout: 5), .completed)
            capture(app, typing ? "wide-table-keyboard" : "wide-table-idle")
        }
    }

    // A real native typing/dismissal journey: the idle selector must not drift
    // away from either composer, and keyboard avoidance must remain intact.
    func testSelectionBarStaysBelowComposerWhileTyping() throws {
        let app = XCUIApplication()
        app.launchEnvironment["NANOCODEX_STARTUP_FIXTURE"] = "1"
        app.launchEnvironment["NANOCODEX_STARTUP_PROFILE"] = UUID().uuidString
        app.launch()
        XCTAssertTrue(app.buttons["main-tab-chat"].waitForExistence(timeout: 25))
        var layouts: [[String: Any]] = []
        defer {
            let data = try? JSONSerialization.data(withJSONObject: layouts, options: [.prettyPrinted, .sortedKeys])
            if let data {
                let attachment = XCTAttachment(data: data, uniformTypeIdentifier: "public.json")
                attachment.name = "composer-selector-layout"; attachment.lifetime = .keepAlways; add(attachment)
            }
            app.terminate()
        }

        for surface in ["chat", "crm"] {
            let tab = app.buttons["main-tab-" + surface]
            tab.tap()
            let input = surface == "chat" ? composer(app) : app.textViews["new-thread-composer"]
            XCTAssertTrue(input.waitForExistence(timeout: 10))
            let composerBoundary = surface == "chat"
                ? app.descendants(matching: .any)["composer-input"].firstMatch : input
            let selector = app.descendants(matching: .any)["main-selection-bar"].firstMatch

            func waitForVisibleKeyboard() {
                let keyboard = app.keyboards.firstMatch
                let visible = NSPredicate { _, _ in
                    keyboard.exists && app.frame.intersects(keyboard.frame)
                        && keyboard.frame.minY < app.frame.maxY
                }
                let expectation = XCTNSPredicateExpectation(predicate: visible, object: keyboard)
                XCTAssertEqual(XCTWaiter.wait(for: [expectation], timeout: 5), .completed,
                               "Wait for the keyboard to enter the screen, not an offscreen transitioning AX node")
            }

            func captureLayout(_ state: String) -> CGFloat {
                // The visible-keyboard gate above waits for real geometry, not
                // merely an accessibility node surviving the transition.
                capture(app, "composer-selector-" + surface + "-" + state)
                XCTAssertTrue(composerBoundary.exists); XCTAssertTrue(composerBoundary.isHittable)
                XCTAssertTrue(selector.exists); XCTAssertTrue(app.frame.contains(selector.frame))
                let gap = selector.frame.minY - composerBoundary.frame.maxY
                XCTAssertGreaterThanOrEqual(gap, 0, "Composer and selector must not overlap")
                // Chat's model controls fill the selector. On CRM, native
                // accessibility reports only the left-aligned tabs, not the
                // full-width glass background. Keep Chat's centering contract
                // and separately verify that both layouts stay horizontally
                // stable through keyboard transitions below.
                if surface == "chat" {
                    XCTAssertEqual(selector.frame.midX, app.frame.midX, accuracy: 1)
                }
                XCTAssertLessThanOrEqual(selector.frame.width, 380 + 0.01)
                for id in ["main-tab-todo", "main-tab-chat", "main-tab-crm", "main-tab-meetings", "main-tab-apps"] {
                    let control = app.buttons[id]
                    XCTAssertEqual(app.buttons.matching(identifier: id).count, 1, id)
                    XCTAssertTrue(control.isHittable, id)
                    XCTAssertTrue(selector.frame.insetBy(dx: -1, dy: -1).contains(control.frame), id)
                    XCTAssertGreaterThanOrEqual(control.frame.width, 44 - 0.01, id)
                    XCTAssertGreaterThanOrEqual(control.frame.height, 44 - 0.01, id)
                }
                let keyboard = app.keyboards.firstMatch
                if keyboard.exists { XCTAssertLessThanOrEqual(selector.frame.maxY, keyboard.frame.minY + 1) }
                layouts.append(["surface": surface, "state": state, "gap": Double(gap),
                                "composer": NSCoder.string(for: composerBoundary.frame),
                                "selector": NSCoder.string(for: selector.frame),
                                "keyboardVisible": keyboard.exists,
                                "keyboard": keyboard.exists ? NSCoder.string(for: keyboard.frame) : "absent"])
                return gap
            }

            XCTAssertFalse(app.keyboards.firstMatch.exists)
            let idleGap = captureLayout("idle")
            let idleCenter = selector.frame.midX
            let draft = "Keep this spacing draft"
            input.tap(); input.typeText(draft)
            waitForVisibleKeyboard()
            let typingGap = captureLayout("keyboard")
            XCTAssertEqual(idleGap, typingGap, accuracy: 1, "Closing the keyboard must retain the typing gap")
            XCTAssertEqual(selector.frame.midX, idleCenter, accuracy: 1)
            let multilineDraft = draft + "\nSecond line\nThird line"
            input.typeText("\nSecond line\nThird line")
            XCTAssertEqual(captureLayout("multiline-keyboard"), typingGap, accuracy: 1)
            tab.tap()
            gone(app.keyboards.firstMatch)
            XCTAssertEqual(input.value as? String, multilineDraft, "Dismissing the keyboard keeps the draft")
            XCTAssertEqual(captureLayout("dismissed"), typingGap, accuracy: 1, "No idle-only space after dismissal")
            XCTAssertEqual(selector.frame.midX, idleCenter, accuracy: 1)
            input.tap()
            XCTAssertEqual(input.value as? String, multilineDraft)
            // XCTest's simulator hardware keyboard may leave the software
            // keyboard offscreen after focus alone. Resume actual typing and
            // require onscreen geometry, rather than accepting that stale node.
            let resumedText = "xyz"
            input.typeText(resumedText)
            waitForVisibleKeyboard()
            XCTAssertEqual((input.value as? String)?.replacingOccurrences(of: resumedText, with: ""), multilineDraft)
            XCTAssertEqual(captureLayout("reopened"), typingGap, accuracy: 1)
            XCTAssertEqual(selector.frame.midX, idleCenter, accuracy: 1)
            tab.tap(); gone(app.keyboards.firstMatch)
        }
    }

    func testHeaderControlsRespondOutsideTheirIcons() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        let drawer = app.descendants(matching: .any)["conversation-list"].firstMatch
        // Tap transparent label space, away from the centered SF Symbol.
        func tapCorner(_ identifier: String) {
            let button = app.buttons[identifier]
            XCTAssertTrue(button.waitForExistence(timeout: 5), identifier)
            XCTAssertGreaterThanOrEqual(button.frame.width, 44 - 0.01, identifier)
            XCTAssertGreaterThanOrEqual(button.frame.height, 44 - 0.01, identifier)
            button.coordinate(withNormalizedOffset: .zero)
                .withOffset(CGVector(dx: 5, dy: 5)).tap()
        }
        tapCorner("conversation-drawer-open")
        XCTAssertTrue(drawer.waitForExistence(timeout: 5))
        tapCorner("conversation-drawer-close")
        gone(drawer)
        tapCorner("running-agents")
        XCTAssertTrue(drawer.waitForExistence(timeout: 5))
        tapCorner("conversation-drawer-close")
        gone(drawer)
        tapCorner("new-conversation")
        XCTAssertEqual(self.selectedConversationTab(app).label, "New agent")
        tapCorner("conversation-drawer-open")
        XCTAssertTrue(drawer.waitForExistence(timeout: 5))
        tapCorner("drawer-new-conversation")
        gone(drawer)
        XCTAssertEqual(self.selectedConversationTab(app).label, "New agent")
        tapCorner("app-menu")
        XCTAssertTrue(app.buttons["inbox-scheduled-jobs"].waitForExistence(timeout: 5))
        capture(app, "header-controls-corner-taps")
    }

    // Failures: compressed touch targets, truncated controls, landscape safe-area
    // clipping, and a drawer that loses its controls after a size change.
    func testAdaptiveChromeFitsRotationAndLargeText() {
        defer { XCUIDevice.shared.orientation = .portrait }
        for largeText in [false, true] {
            XCUIDevice.shared.orientation = .portrait
            let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString], arguments:
                largeText ? ["-UIPreferredContentSizeCategoryName", "UICTContentSizeCategoryAccessibilityM"] : [])
            selectTab(app, id: largeText ? "data" : "durability",
                      title: largeText ? "Tighten the fuel forecast" : "Make long sessions bulletproof")
            for orientation in [UIDeviceOrientation.portrait, .landscapeLeft, .landscapeRight] {
                XCUIDevice.shared.orientation = orientation
                let rotated = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
                    orientation == .portrait ? app.frame.width < app.frame.height : app.frame.width > app.frame.height
                }, object: nil)
                XCTAssertEqual(XCTWaiter.wait(for: [rotated], timeout: 5), .completed)
                let sizeName = orientation == .portrait ? "portrait" : (orientation == .landscapeLeft ? "landscape-left" : "landscape-right")
                let dock = app.descendants(matching: .any)["main-selection-bar"].firstMatch
                let controls = ["main-tab-todo", "main-tab-chat", "model-picker"].map { app.buttons[$0] }
                XCTAssertTrue(dock.waitForExistence(timeout: 5))
                for control in controls {
                    XCTAssertTrue(control.isHittable, control.identifier)
                    XCTAssertGreaterThanOrEqual(control.frame.width, 44 - 0.01, control.identifier)
                    XCTAssertGreaterThanOrEqual(control.frame.height, 44 - 0.01, control.identifier)
                    XCTAssertTrue(dock.frame.insetBy(dx: -1, dy: -1).contains(control.frame), control.identifier)
                }
                XCTAssertTrue(app.frame.contains(dock.frame))
                for (index, control) in controls.enumerated() {
                    for other in controls.dropFirst(index + 1) {
                        XCTAssertFalse(control.frame.intersects(other.frame), "Controls must not overlap")
                    }
                }
                capture(app, "adaptive-chrome-\(largeText ? "large" : "standard")-\(sizeName)")
                if orientation == .landscapeLeft {
                    composer(app).tap()
                    composer(app).typeText("Keep my landscape draft")
                    XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5))
                    XCTAssertLessThanOrEqual(dock.frame.maxY, app.keyboards.firstMatch.frame.minY)
                    capture(app, "adaptive-chrome-\(largeText ? "large" : "standard")-landscape-keyboard")
                }
                if orientation == .landscapeLeft {
                    // Start at the usable leading edge, beyond the camera safe
                    // inset. Rotation must not strand the drawer's edge gesture.
                    let header = app.buttons["conversation-drawer-open"].frame
                    let edge = app.coordinate(withNormalizedOffset: .zero)
                        .withOffset(CGVector(dx: header.minX + 2, dy: header.midY))
                    edge.press(forDuration: 0.01, thenDragTo: edge.withOffset(CGVector(dx: app.frame.width * 0.65, dy: 0)))
                    XCTAssertTrue(app.descendants(matching: .any)["conversation-list"].firstMatch.waitForExistence(timeout: 5))
                } else { app.buttons["conversation-drawer-open"].tap() }
                for id in ["conversation-drawer-close", "drawer-new-conversation"] {
                    XCTAssertTrue(app.buttons[id].isHittable)
                    XCTAssertTrue(app.frame.contains(app.buttons[id].frame))
                }
                capture(app, "adaptive-drawer-\(largeText ? "large" : "standard")-\(sizeName)")
                app.buttons["conversation-drawer-close"].tap()
                XCTAssertTrue(app.buttons["model-picker"].isHittable)
            }
            XCUIDevice.shared.orientation = .portrait
            let portrait = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in app.frame.width < app.frame.height }, object: nil)
            XCTAssertEqual(XCTWaiter.wait(for: [portrait], timeout: 5), .completed)
            app.terminate()
        }
    }

    private func assertUnifiedInbox(_ app: XCUIApplication) {
        XCTAssertTrue(app.buttons["main-tab-todo"].waitForExistence(timeout: 15))
        for legacy in ["For you", "Mail", "Later"] {
            XCTAssertFalse(app.buttons["todo-split:" + legacy].exists, "Inbox must not have legacy split tabs")
        }
    }

    private func revealInboxElement(_ element: XCUIElement, in app: XCUIApplication) {
        XCTAssertTrue(element.waitForExistence(timeout: 5), element.identifier)
        for _ in 0..<8 where !element.isHittable {
            if element.frame.maxY < app.frame.midY && element.frame != .zero { app.swipeDown() }
            else { app.swipeUp() }
        }
        XCTAssertTrue(element.isHittable, element.identifier)
    }

    private func selectInboxScope(_ app: XCUIApplication, _ scope: String) {
        let menu = app.buttons["todo-filter-menu"]
        XCTAssertTrue(menu.waitForExistence(timeout: 5))
        for _ in 0..<8 where !menu.isHittable { app.swipeDown() }
        XCTAssertTrue(menu.isHittable); menu.tap()
        let option = app.buttons["todo-filter:" + scope]
        XCTAssertTrue(option.waitForExistence(timeout: 5)); option.tap()
    }

    func testDecisionWarmNavigationAndPreparationStates() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture", "--decision-preparation-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        let decision = app.buttons["decision-card:fixture-email"]
        assertUnifiedInbox(app)
        XCTAssertTrue(decision.waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["todo-row:capture:fixture-working"].exists)
        XCTAssertTrue(app.buttons["todo-row:capture:fixture-blocked"].exists)
        var timings: [Double] = []
        for index in 0..<8 {
            let start = ProcessInfo.processInfo.systemUptime
            decision.tap()
            XCTAssertTrue(app.staticTexts["decision-complete-draft"].waitForExistence(timeout: 5))
            if index > 0 { timings.append((ProcessInfo.processInfo.systemUptime - start) * 1000) }
            XCTAssertTrue(app.buttons["decision-approve-send"].isEnabled)
            app.buttons["decision-detail-close"].tap()
            XCTAssertTrue(decision.waitForExistence(timeout: 5))
        }
        timings.sort()
        print("DECISION_SIMULATOR_XCTEST_NAVIGATION n=7 p50_ms=\(timings[3]) p95_ms=\(timings[6]) includes_XCTest_tap_wait=true not_phone=true")
        let blocked = app.buttons["todo-row:capture:fixture-blocked"]
        revealInboxElement(blocked, in: app)
        capture(app, "unified-inbox-blocked-and-preparing")
        blocked.tap()
        XCTAssertTrue(app.buttons["capture-retry"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["decision-approve-send"].exists)
    }

    func testTodoUnifiedQueueThreadDraftAndExplicitSend() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        XCTAssertTrue(app.buttons["main-tab-todo"].waitForExistence(timeout: 10))
        let meeting = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "meeting-briefing:")).firstMatch
        XCTAssertTrue(meeting.waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["decision-card:fixture-email"].exists)
        XCTAssertFalse(app.buttons["todo-row:mail:fixture-mail:fixture-thread"].exists, "Linked decisions replace only their duplicate raw thread in Inbox")
        assertUnifiedInbox(app)
        let raw = app.buttons["todo-row:mail:fixture-mail:fixture-budget"]
        XCTAssertTrue(raw.exists, "Unlinked raw mail belongs beside prepared decisions in the default Inbox")
        capture(app, "unified-inbox-calendar-decision-and-raw-mail")
        meeting.tap()
        XCTAssertTrue(app.staticTexts["Prepared briefing"].waitForExistence(timeout: 5))
        app.buttons["Done"].tap()
        let decision = app.buttons["decision-card:fixture-email"]
        revealInboxElement(decision, in: app); decision.tap()
        let conversation = app.buttons["decision-open-conversation"]
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        revealInboxElement(conversation, in: app); conversation.tap()
        XCTAssertTrue(app.buttons["mail-expand-all"].waitForExistence(timeout: 5))
        app.buttons["mail-expand-all"].tap()
        XCTAssertTrue(app.staticTexts["mail-body:fixture-message-1"].exists)
        app.buttons["mail-reply_all"].tap()
        let body = app.textViews["mail-draft-body"]
        XCTAssertTrue(body.waitForExistence(timeout: 5)); body.tap()
        body.typeText("Thursday at 10 works. I will review the launch plan before then.")
        app.buttons["mail-draft-done"].tap()
        let continued = app.buttons["mail-continue-draft"]
        if !continued.isHittable { app.swipeUp() }
        XCTAssertTrue(continued.waitForExistence(timeout: 5)); continued.tap()
        XCTAssertTrue((body.value as? String ?? "").contains("Thursday at 10 works"))
        XCTAssertFalse(app.staticTexts["Fixture send complete · no email sent"].exists)
        app.buttons["mail-send"].tap()
        XCTAssertTrue(app.staticTexts["Fixture send complete · no email sent"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["mail-send"].isEnabled)
        capture(app, "todo-explicit-fixture-send-receipt")
    }

    func testTodoFilterMenuChangesScopeWithoutSplitTabs() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture", "--decision-preparation-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        assertUnifiedInbox(app)
        let decision = app.buttons["decision-card:fixture-email"]
        XCTAssertTrue(decision.waitForExistence(timeout: 10))
        let menu = app.buttons["todo-filter-menu"]
        XCTAssertTrue(menu.waitForExistence(timeout: 5)); menu.tap()
        for scope in ["Inbox", "Mail", "Snoozed", "Drafts", "Sent", "All mail"] {
            XCTAssertTrue(app.buttons["todo-filter:" + scope].exists, "Missing optional scope: " + scope)
        }
        capture(app, "unified-inbox-optional-scope-menu")
        app.buttons["todo-filter:Mail"].tap()
        XCTAssertTrue(app.buttons["todo-row:mail:fixture-mail:fixture-thread"].waitForExistence(timeout: 5))
        XCTAssertFalse(decision.exists, "Mail scope is raw mail, not another decision tab")
        XCTAssertFalse(app.buttons["todo-row:capture:fixture-blocked"].exists)
        assertUnifiedInbox(app)
        selectInboxScope(app, "Inbox")
        XCTAssertTrue(decision.waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["todo-row:capture:fixture-blocked"].exists)
        XCTAssertTrue(app.buttons["todo-row:mail:fixture-mail:fixture-budget"].exists)
        XCTAssertFalse(app.buttons["todo-row:mail:fixture-mail:fixture-thread"].exists)
        capture(app, "unified-inbox-default-restored-after-mail-scope")
    }

    func testTodoAIQuickActionsKeepContextEditableAndDoNotSend() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture", "--decision-preparation-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        assertUnifiedInbox(app)
        let decisionAI = app.buttons["inbox-ai:decision:fixture-email"]
        // A request is reviewable before agent work. None of these commands is a send approval.
        for action in ["Brief me", "Prepare next steps", "Draft reply", "Ask something else"] {
            revealInboxElement(decisionAI, in: app); decisionAI.tap()
            XCTAssertTrue(app.buttons[action].waitForExistence(timeout: 5)); app.buttons[action].tap()
            let context = app.descendants(matching: .any)["inbox-ai-context"].firstMatch
            XCTAssertTrue(context.waitForExistence(timeout: 5))
            XCTAssertTrue(context.label.contains("Maya"), context.label)
            let instructions = app.descendants(matching: .any)["inbox-ai-instructions"].firstMatch
            XCTAssertTrue(instructions.waitForExistence(timeout: 5))
            instructions.tap(); instructions.typeText(" Mention the launch review; prepare only, do not send.")
            XCTAssertTrue((instructions.value as? String ?? "").contains("prepare only, do not send"))
            XCTAssertTrue(app.buttons["inbox-ai-prepare"].isEnabled)
            XCTAssertFalse(app.buttons["mail-send"].exists)
            XCTAssertFalse(app.buttons["decision-approve-send"].exists)
            capture(app, "unified-inbox-ai-decision-" + action.lowercased().replacingOccurrences(of: " ", with: "-"))
            app.buttons["Cancel"].tap()
            XCTAssertTrue(instructions.waitForNonExistence(timeout: 5))
            XCTAssertFalse(app.staticTexts["Fixture send complete · no email sent"].exists)
        }
        let blockedAI = app.buttons["inbox-ai:capture:fixture-blocked"]
        revealInboxElement(blockedAI, in: app); blockedAI.tap()
        XCTAssertFalse(app.buttons["Draft reply"].exists, "A capture is not a mail reply target")
        app.buttons["Prepare next steps"].tap()
        let context = app.descendants(matching: .any)["inbox-ai-context"].firstMatch
        XCTAssertTrue(context.waitForExistence(timeout: 5))
        XCTAssertTrue(context.label.contains("Book a trip"), context.label)
        let instructions = app.descendants(matching: .any)["inbox-ai-instructions"].firstMatch
        instructions.tap(); instructions.typeText(" Destination is Oslo, budget is 800. Prepare options only.")
        capture(app, "unified-inbox-ai-blocked-capture-context")
        app.buttons["inbox-ai-prepare"].tap()
        XCTAssertTrue(instructions.waitForNonExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["Fixture send complete · no email sent"].exists)
        XCTAssertTrue(app.buttons["todo-row:capture:fixture-blocked"].exists)
        // Raw mail uses its own source, not the linked decision's context.
        let rawAI = app.buttons["inbox-ai:mail:fixture-mail:fixture-budget"]
        revealInboxElement(rawAI, in: app); rawAI.tap(); app.buttons["Draft reply"].tap()
        XCTAssertTrue(context.waitForExistence(timeout: 5))
        XCTAssertTrue(context.label.contains("September notes"), context.label)
        XCTAssertFalse(context.label.contains("How should we reply to Maya?"))
        XCTAssertTrue(instructions.waitForExistence(timeout: 5))
        instructions.tap(); instructions.typeText(" Thank Jordan for the notes. Save a draft only.")
        XCTAssertTrue(app.buttons["inbox-ai-prepare"].isEnabled)
        XCTAssertFalse(app.buttons["mail-send"].exists)
        capture(app, "unified-inbox-ai-raw-mail-context")
        app.buttons["Cancel"].tap()
        XCTAssertFalse(app.staticTexts["Fixture send complete · no email sent"].exists)
    }

    func testTodoVerifiedCRMProfileNavigationAndUnverifiedBlockedContext() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture", "--decision-preparation-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        assertUnifiedInbox(app)
        let decision = app.buttons["decision-card:fixture-email"]
        revealInboxElement(decision, in: app); decision.tap()
        let person = app.buttons["inbox-crm:f015eb65-ea12-4bc9-9a80-aa937ac6ffea"]
        revealInboxElement(person, in: app)
        XCTAssertTrue(person.label.contains("Maya Chen"), person.label)
        XCTAssertTrue(person.label.contains("Product lead"), person.label)
        XCTAssertTrue(person.label.contains("Example Studio"), person.label)
        XCTAssertTrue(app.staticTexts["Launch collaborator"].exists)
        capture(app, "unified-inbox-verified-person-in-decision-context")
        person.tap()
        XCTAssertTrue(app.navigationBars["Profile"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Maya Chen"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Product lead"].exists)
        XCTAssertTrue(app.staticTexts["Launch collaborator"].exists)
        XCTAssertFalse(app.staticTexts["Example University"].exists, "The link must not open an unrelated cached profile")
        capture(app, "unified-inbox-linked-maya-crm-profile")
        let back = app.navigationBars["Profile"].buttons.element(boundBy: 0)
        XCTAssertTrue(back.isHittable); back.tap()
        XCTAssertTrue(person.waitForExistence(timeout: 5))
        XCTAssertFalse(app.navigationBars["Profile"].exists)
        XCTAssertFalse(app.staticTexts["Fixture send complete · no email sent"].exists)
        app.buttons["decision-detail-close"].tap()
        let blocked = app.buttons["todo-row:capture:fixture-blocked"]
        revealInboxElement(blocked, in: app); blocked.tap()
        XCTAssertTrue(app.staticTexts["Destination and budget are needed"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["No verified CRM profile is linked yet."].exists)
        XCTAssertFalse(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "inbox-crm:")).firstMatch.exists,
                       "Missing source identity must not manufacture a person/profile link")
        XCTAssertTrue(app.buttons["capture-retry"].exists)
        XCTAssertFalse(app.buttons["decision-approve-send"].exists)
        XCTAssertFalse(app.buttons["mail-send"].exists)
        capture(app, "unified-inbox-blocked-without-guessed-crm-person")
    }

    func testTodoRawMailWarmReopenKeepsReadableThread() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        assertUnifiedInbox(app); selectInboxScope(app, "Mail")
        let thread = app.buttons["todo-row:mail:fixture-mail:fixture-budget"]
        var samples: [Double] = []
        for index in 0..<3 {
            revealInboxElement(thread, in: app)
            let start = ProcessInfo.processInfo.systemUptime
            thread.tap()
            let body = app.staticTexts["mail-body:fixture-budget-message"]
            XCTAssertTrue(body.waitForExistence(timeout: 5))
            XCTAssertEqual(app.staticTexts["mail-thread-subject"].label, "September notes")
            XCTAssertEqual(body.label, "The updated September notes are ready for your review.")
            XCTAssertFalse(app.staticTexts["mail-body:fixture-message-2"].exists,
                           "Opening September notes must not reuse the linked launch conversation")
            samples.append((ProcessInfo.processInfo.systemUptime - start) * 1000)
            XCTAssertFalse(app.staticTexts["Fixture send complete · no email sent"].exists)
            capture(app, "unified-inbox-raw-thread-open-" + String(index))
            app.buttons["mail-thread-done"].tap()
            XCTAssertTrue(thread.waitForExistence(timeout: 5))
        }
        print("RAW_MAIL_SIM_UI fixture_n=3 tap_to_body_ms=\(samples) XCTest_automation_included=true not_production=true")
    }

    func testTodoCaptureCompleteUndoAndMailArchiveUndo() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture", "--decision-preparation-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        XCTAssertTrue(app.buttons["todo-compose"].waitForExistence(timeout: 10))
        assertUnifiedInbox(app)
        let filter = app.buttons["todo-filter-menu"]
        revealInboxElement(filter, in: app); filter.tap()
        let prepare = app.buttons["todo-prepare-thought"]
        XCTAssertTrue(prepare.waitForExistence(timeout: 5)); prepare.tap()
        let input = app.descendants(matching: .any)["todo-capture"].firstMatch
        input.tap(); input.typeText("Review the launch agenda")
        app.buttons["todo-capture-save"].tap()
        app.buttons["todo-capture-done"].tap()
        XCTAssertTrue(app.staticTexts["Review the launch agenda"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Pending"].exists, "Capture delegates preparation rather than claiming completion")
        let ready = app.buttons["todo-row:capture:fixture-ready-capture"]
        revealInboxElement(ready, in: app); ready.swipeRight()
        app.buttons["todo-complete:fixture-ready-capture"].tap()
        XCTAssertTrue(ready.waitForNonExistence(timeout: 5), "Completing the capture removes it from the unified Inbox")
        XCTAssertTrue(app.buttons["Undo"].waitForExistence(timeout: 5)); app.buttons["Undo"].tap()
        // Unified Inbox keeps the capture's row identity across preparation states.
        // Undo requeues the same capture; it must not restore the ready proposal.
        XCTAssertTrue(ready.waitForExistence(timeout: 5))
        XCTAssertEqual(app.buttons.matching(identifier: "todo-row:capture:fixture-ready-capture").count, 1)
        revealInboxElement(ready, in: app)
        XCTAssertTrue(ready.staticTexts["Plan the launch review"].exists)
        XCTAssertTrue(ready.staticTexts["Pending"].exists, "Undo requeues preparation; it must not pretend the old proposal is still ready")
        XCTAssertFalse(ready.staticTexts["Ready"].exists)
        XCTAssertFalse(ready.staticTexts["Hold a focused review"].exists, "Undo must discard the old ready recommendation")
        capture(app, "todo-undo-requeues-same-capture")
        selectInboxScope(app, "Mail")
        let thread = app.buttons["todo-row:mail:fixture-mail:fixture-thread"]
        XCTAssertTrue(thread.waitForExistence(timeout: 5)); revealInboxElement(thread, in: app); thread.swipeLeft()
        app.buttons["todo-archive:fixture-thread"].tap()
        XCTAssertFalse(thread.exists)
        app.buttons["Undo"].tap(); XCTAssertTrue(thread.waitForExistence(timeout: 5))
        capture(app, "todo-capture-prepares-and-archive-undo")
    }

    func testRawInboxWithoutPreparedDecisionsDoesNotClaimCaughtUpOrHealthyWatch() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-mail-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        let tab = app.buttons["main-tab-todo"]
        XCTAssertTrue(tab.waitForExistence(timeout: 15)); tab.tap()
        XCTAssertTrue(app.buttons["todo-compose"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["todo-compose"].isEnabled, "Connected fixture mail accounts do not establish watch coverage")
        let disclosure = app.descendants(matching: .any)["inbox-coverage"].firstMatch
        XCTAssertTrue(disclosure.waitForExistence(timeout: 5)); disclosure.tap()
        let coverage = app.staticTexts["todo-source-coverage"]
        XCTAssertTrue(coverage.waitForExistence(timeout: 5))
        XCTAssertTrue(coverage.label.contains("Inbox-scoped"))
        XCTAssertTrue(coverage.label.contains("archived mail is excluded"))
        XCTAssertTrue(coverage.label.contains("Watch coverage is unknown"))
        assertUnifiedInbox(app)
        XCTAssertTrue(app.buttons["todo-row:mail:fixture-mail:fixture-thread"].exists)
        XCTAssertFalse(app.buttons["decision-card:fixture-email"].exists)
        XCTAssertFalse(app.staticTexts["Caught up"].exists)
        capture(app, "decision-empty-watch-coverage-unknown")
    }

    func testDecisionCachedPreviewIsReadOnlyWithoutCurrentAuthority() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture", "--decision-cached-readonly-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        let card = app.buttons["decision-card:fixture-email"]
        XCTAssertTrue(card.waitForExistence(timeout: 15)); card.tap()
        let preview = app.staticTexts["decision-complete-draft"]
        XCTAssertTrue(preview.waitForExistence(timeout: 5))
        XCTAssertEqual(preview.label, "Thursday at 10 works. I will review the launch plan before then.")
        XCTAssertTrue(app.staticTexts["decision-mail-error"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["decision-approve-send"].isEnabled)
        XCTAssertFalse(app.staticTexts["Fixture send complete · no email sent"].exists)
        capture(app, "decision-cached-preview-no-authority")
    }

    func testDecisionExactDraftChangeInstructionsAndWarmReopen() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        let card = app.buttons["decision-card:fixture-email"]
        XCTAssertTrue(card.waitForExistence(timeout: 15))
        var samples: [Double] = []
        for _ in 0..<3 {
            let start = ProcessInfo.processInfo.systemUptime
            card.tap()
            let preview = app.staticTexts["decision-complete-draft"]
            XCTAssertTrue(preview.waitForExistence(timeout: 5))
            samples.append((ProcessInfo.processInfo.systemUptime - start) * 1000)
            XCTAssertEqual(preview.label, "Thursday at 10 works. I will review the launch plan before then.")
            XCTAssertFalse(app.staticTexts["Fixture send complete · no email sent"].exists)
            app.buttons["decision-detail-close"].tap()
        }
        print("DECISION_SIM_UI fixture_n=3 tap_to_preview_ms=\(samples) XCTest_automation_included=true not_production=true")
        card.tap()
        let instructions = app.textFields["decision-instructions"]
        for _ in 0..<4 where !instructions.isHittable { app.swipeUp() }
        XCTAssertTrue(instructions.exists)
        XCTAssertFalse(app.buttons["decision-change"].isEnabled)
        instructions.tap(); instructions.typeText("Make it warmer and mention the launch review")
        XCTAssertTrue(app.buttons["decision-change"].isEnabled)
        XCTAssertFalse(app.staticTexts["Fixture send complete · no email sent"].exists)
        capture(app, "decision-change-instructions-unsent")
        // Demo Change acknowledges locally; backend generation fencing is
        // verified separately by HTTP/XCTest, not claimed by this UI fixture.
        app.buttons["decision-change"].tap()
        XCTAssertTrue(card.waitForExistence(timeout: 5))
    }

    func testTodoSuggestedDraftFromLinkedDecisionStaysEditable() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        let decision = app.buttons["decision-card:fixture-email"]
        XCTAssertTrue(decision.waitForExistence(timeout: 10)); decision.tap()
        XCTAssertTrue(app.staticTexts["decision-complete-draft"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts["decision-complete-draft"].label, "Thursday at 10 works. I will review the launch plan before then.")
        XCTAssertTrue(app.staticTexts["From: alex@example.com"].exists)
        XCTAssertTrue(app.staticTexts["To: maya@example.com"].exists)
        XCTAssertTrue(app.staticTexts["Re: A quick look at the launch plan"].exists)
        XCTAssertTrue(app.staticTexts["Account draft · version 1"].exists)
        XCTAssertFalse(app.staticTexts["Fixture send complete · no email sent"].exists)
        let approve = app.buttons["decision-approve-send"]
        XCTAssertTrue(approve.isEnabled)
        capture(app, "decision-ready-for-exact-approval")
        revealInboxElement(approve, in: app); approve.tap()
        XCTAssertTrue(app.staticTexts["Fixture send complete · no email sent"].waitForExistence(timeout: 5))
        XCTAssertFalse(approve.isEnabled)
    }

    func testTodoSnoozedMailReturnsThroughScopeMenu() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        assertUnifiedInbox(app); selectInboxScope(app, "Mail")
        let thread = app.buttons["todo-row:mail:fixture-mail:fixture-budget"]
        XCTAssertTrue(thread.waitForExistence(timeout: 10)); revealInboxElement(thread, in: app); thread.swipeLeft()
        app.buttons["Snooze"].tap(); app.buttons["Tomorrow at 9 AM"].tap()
        XCTAssertTrue(thread.waitForNonExistence(timeout: 5))
        selectInboxScope(app, "Snoozed")
        XCTAssertTrue(thread.waitForExistence(timeout: 5)); revealInboxElement(thread, in: app); thread.swipeRight()
        app.buttons["Bring back"].tap()
        XCTAssertTrue(thread.waitForNonExistence(timeout: 5))
        selectInboxScope(app, "Mail")
        XCTAssertTrue(thread.waitForExistence(timeout: 5))
        capture(app, "todo-snooze-return")
    }

    func testTodoLinkedMailSnoozeUsesOneReminder() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture", "--todo-linked-mail-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        let decision = app.buttons["decision-card:fixture-email"]
        XCTAssertTrue(decision.waitForExistence(timeout: 10))
        selectInboxScope(app, "Mail")
        let thread = app.buttons["todo-row:mail:fixture-mail:fixture-thread"]
        XCTAssertTrue(thread.waitForExistence(timeout: 5)); revealInboxElement(thread, in: app); thread.swipeLeft()
        app.buttons["Snooze"].tap(); app.buttons["Tomorrow at 9 AM"].tap()
        XCTAssertTrue(thread.waitForNonExistence(timeout: 5))
        selectInboxScope(app, "Inbox")
        XCTAssertFalse(decision.exists)
        XCTAssertFalse(thread.exists)
        capture(app, "unified-inbox-linked-thread-snoozed")
        app.terminate(); app.launch()
        assertUnifiedInbox(app)
        XCTAssertFalse(decision.exists, "A linked snooze must survive app relaunch")
        selectInboxScope(app, "Snoozed")
        XCTAssertTrue(decision.waitForExistence(timeout: 5))
        XCTAssertFalse(thread.exists)
        revealInboxElement(decision, in: app); decision.swipeRight(); app.buttons["Bring back"].tap()
        XCTAssertTrue(decision.waitForNonExistence(timeout: 5))
        selectInboxScope(app, "Inbox")
        XCTAssertTrue(decision.waitForExistence(timeout: 5))
        XCTAssertFalse(thread.exists)
        capture(app, "unified-inbox-linked-snooze-restored-once")
    }

    func testTodoDistinctDecisionsInOneThreadRemainActionable() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture", "--todo-multi-message-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        let latest = app.buttons["decision-card:fixture-email"]
        let earlier = app.buttons["decision-card:fixture-earlier-email"]
        assertUnifiedInbox(app)
        XCTAssertTrue(latest.waitForExistence(timeout: 10)); XCTAssertTrue(earlier.exists)
        XCTAssertFalse(app.buttons["todo-row:mail:fixture-mail:fixture-thread"].exists)
        revealInboxElement(earlier, in: app); earlier.tap()
        XCTAssertTrue(app.staticTexts["decision-complete-draft"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts["decision-complete-draft"].label, "I will review the launch plan before Thursday.")
        app.buttons["decision-detail-close"].tap()
        latest.tap()
        XCTAssertTrue(app.staticTexts["decision-complete-draft"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["decision-complete-draft"].label.contains("Thursday at 10 works"))
        app.buttons["decision-detail-close"].tap()
        XCTAssertTrue(earlier.exists); XCTAssertTrue(latest.exists)
        capture(app, "unified-inbox-two-decisions-one-thread")
    }

    func testTodoComposeShowsSenderAndCanStartAgainAfterSend() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        let compose = app.buttons["todo-compose"]
        XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
        XCTAssertTrue(app.staticTexts["alex@example.com"].waitForExistence(timeout: 5))
        let to = app.textFields["mail-draft-to"]
        XCTAssertTrue(to.waitForExistence(timeout: 5)); to.tap(); to.typeText("review@example.com")
        let subject = app.textFields["mail-draft-subject"]
        subject.tap(); subject.typeText("Launch review")
        let body = app.textViews["mail-draft-body"]
        body.tap(); body.typeText("Here are my notes for the launch.")
        app.buttons["mail-send"].tap()
        XCTAssertTrue(app.staticTexts["Fixture send complete · no email sent"].waitForExistence(timeout: 5))
        app.buttons["mail-draft-done"].tap()
        compose.tap()
        XCTAssertTrue(app.textViews["mail-draft-body"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.textViews["mail-draft-body"].value as? String, "")
        XCTAssertFalse(app.buttons["mail-send"].isEnabled)
        capture(app, "todo-new-compose-after-send")
    }

    func testTodoUnknownSendStaysLockedAfterRelaunch() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "--todo-ui-fixture", "--todo-mail-fixture", "--todo-mail-unknown-fixture"]
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString]
        app.launch()
        assertUnifiedInbox(app); selectInboxScope(app, "Mail")
        let thread = app.buttons["todo-row:mail:fixture-mail:fixture-thread"]
        XCTAssertTrue(thread.waitForExistence(timeout: 10)); thread.tap()
        app.buttons["mail-reply"].tap()
        let body = app.textViews["mail-draft-body"]
        XCTAssertTrue(body.waitForExistence(timeout: 5)); body.tap(); body.typeText("Thanks, I will review it.")
        app.buttons["mail-send"].tap()
        XCTAssertTrue(app.staticTexts["Send outcome unknown · retry blocked"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["mail-send"].isEnabled)
        app.terminate(); app.launch()
        assertUnifiedInbox(app); selectInboxScope(app, "Mail")
        XCTAssertTrue(thread.waitForExistence(timeout: 10)); thread.tap()
        let continued = app.buttons["mail-continue-draft"]
        if !continued.isHittable { app.swipeUp() }
        XCTAssertTrue(continued.waitForExistence(timeout: 5)); continued.tap()
        XCTAssertTrue(app.staticTexts["Send outcome unknown · retry blocked"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["mail-send"].isEnabled)
        capture(app, "todo-unknown-send-preserved")
    }

    func testRunningAgentsToolbarFiltersAndShowsLastPrompt() {
        let originalAppearance = XCUIDevice.shared.appearance
        addTeardownBlock { XCUIDevice.shared.appearance = originalAppearance }
        for appearance in ["light", "dark"] {
            XCUIDevice.shared.appearance = appearance == "dark" ? .dark : .light
            let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString,
                              "NANOCODEX_DEMO_SIDEBAR": "1",
                              "NANOCODEX_DEMO_APPEARANCE": appearance])
            let toolbar = app.buttons["running-agents"]
            XCTAssertTrue(toolbar.waitForExistence(timeout: 5))
            XCTAssertEqual(toolbar.value as? String, "2")
            capture(app, "agent-overview-toolbar-" + appearance)
            toolbar.tap()
            let inbox = app.buttons["conversation-row:inbox"]
            XCTAssertTrue(inbox.waitForExistence(timeout: 5))
            XCTAssertTrue((inbox.value as? String ?? "").contains("I'm checking inbox state"))
            XCTAssertTrue((inbox.value as? String ?? "").contains("You:"))
            XCTAssertTrue(app.buttons["conversation-row:data"].exists)
            XCTAssertFalse(app.buttons["conversation-row:hands"].exists)
            capture(app, "agent-overview-running-" + appearance)
            app.segmentedControls.buttons["All"].tap()
            XCTAssertTrue(app.buttons["conversation-row:hands"].waitForExistence(timeout: 5))
            capture(app, "agent-overview-all-" + appearance)
            app.terminate()
        }
    }

    func testThreadScreenDockContainsRowsAcrossAppearanceAndSelection() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_SCREEN_FIXTURE"] == "1" else {
            throw XCTSkip("Run fixtures/remote-screen.mjs on loopback port 18965")
        }
        let originalAppearance = XCUIDevice.shared.appearance
        addTeardownBlock { XCUIDevice.shared.appearance = originalAppearance }
        for appearance in ["light", "dark"] {
            XCUIDevice.shared.appearance = appearance == "dark" ? .dark : .light
            let app = launch(["NANOCODEX_DEMO_SCREENS": "1", "NANOCODEX_DEMO_APPEARANCE": appearance],
                             arguments: ["-UIPreferredContentSizeCategoryName", "UICTContentSizeCategoryXXXL"])
            selectInbox(app)
            let draft = "Keep my draft while choosing a desktop"
            composer(app).tap(); composer(app).typeText(draft)
            navigationAction(app, "conversation-remote-screens").tap()
            let panel = app.descendants(matching: .any)["thread-screen-panel"].firstMatch
            XCTAssertTrue(panel.waitForExistence(timeout: 5))
            let desktop = app.buttons["thread-screen:cf:fixture:desktop"]
            XCTAssertTrue(desktop.waitForExistence(timeout: 10))

            func requireContainedRow(_ id: String) {
                let row = app.buttons["thread-screen:cf:fixture:" + id]
                let list = app.scrollViews["thread-screen-devices"]
                XCTAssertTrue(list.exists)
                for _ in 0..<4 {
                    if row.exists && row.isHittable && panel.frame.contains(row.frame) { break }
                    list.swipeUp()
                }
                XCTAssertTrue(row.isHittable, "The entire screen row must be reachable")
                XCTAssertFalse(row.frame.isEmpty)
                XCTAssertTrue(panel.frame.contains(row.frame), "Rows must stay inside the dock")
                XCTAssertGreaterThanOrEqual(row.frame.height, 44)
                XCTAssertTrue(row.label.contains("Synthetic research workspace with a long desktop name that must stay inside its screen row"),
                              "The complete device name must remain available to VoiceOver when the visible line truncates")
                XCTAssertTrue(app.frame.contains(panel.frame), "The dock must fit on screen at large text sizes")
                XCTAssertTrue(composer(app).isHittable, "The dock must leave the draft usable")
                capture(app, "screen-dock-contained-" + appearance + "-" + id)
            }

            requireContainedRow("desktop")
            desktop.tap()
            XCTAssertTrue(app.staticTexts["Watching"].waitForExistence(timeout: 15))
            let canvas = app.descendants(matching: .any)["thread-screen-canvas"].firstMatch
            XCTAssertTrue(canvas.exists)
            XCTAssertTrue(panel.frame.contains(canvas.frame))
            XCTAssertTrue(app.staticTexts["View only"].exists)
            XCTAssertEqual(composer(app).value as? String, draft)
            capture(app, "screen-dock-selected-" + appearance)

            app.buttons["thread-screen-options"].tap()
            app.buttons["Change desktop"].tap()
            requireContainedRow("gamepad")
            app.buttons["thread-screen:cf:fixture:gamepad"].tap()
            XCTAssertTrue(app.staticTexts["Watching"].waitForExistence(timeout: 15))
            app.buttons["thread-screen-close"].tap()
            gone(panel)
            XCTAssertEqual(composer(app).value as? String, draft)
            navigationAction(app, "conversation-remote-screens").tap()
            XCTAssertTrue(panel.waitForExistence(timeout: 5))
            XCTAssertTrue(app.staticTexts["Watching"].waitForExistence(timeout: 15), "Reopening must recover the selected screen")
            XCTAssertEqual(composer(app).value as? String, draft)
            capture(app, "screen-dock-reopened-" + appearance)
            app.buttons["thread-screen-close"].tap()
            app.terminate()
        }
    }

    func testThreadScreenDockPreservesDraftAndThreadNavigation() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString,
                          "NANOCODEX_DEMO_SCREENS": "1"])
        switchConversation(app, id: "inbox")
        composer(app).tap(); composer(app).typeText("Keep talking while watching")
        navigationAction(app, "conversation-remote-screens").tap()
        let panel = app.descendants(matching: .any)["thread-screen-panel"].firstMatch
        XCTAssertTrue(panel.waitForExistence(timeout: 5))
        if ProcessInfo.processInfo.environment["NANOCODEX_SCREEN_FIXTURE"] == "1" {
            let desktop = app.buttons["thread-screen:cf:fixture:desktop"]
            XCTAssertTrue(desktop.waitForExistence(timeout: 10))
            desktop.tap()
            XCTAssertTrue(app.staticTexts["Watching"].waitForExistence(timeout: 15))
            XCTAssertTrue(app.descendants(matching: .any)["thread-screen-canvas"].firstMatch.exists)
            XCTAssertTrue(app.staticTexts["View only"].exists)
        }
        XCTAssertTrue(composer(app).isHittable)
        XCTAssertEqual(composer(app).value as? String, "Keep talking while watching")
        capture(app, "thread-screen-docked")
        app.buttons["thread-screen-expand"].tap()
        XCTAssertTrue(composer(app).isHittable)
        capture(app, "thread-screen-expanded")
        app.buttons["thread-screen-expand"].tap()
        app.buttons["conversation-drawer-open"].tap()
        app.buttons["conversation-row:durability"].tap()
        XCTAssertFalse(panel.exists, "Each thread owns its screen panel")
        app.buttons["conversation-drawer-open"].tap()
        app.buttons["conversation-row:inbox"].tap()
        XCTAssertTrue(panel.waitForExistence(timeout: 5))
        app.buttons["thread-screen-close"].tap()
        XCTAssertEqual(composer(app).value as? String, "Keep talking while watching")
    }

    func testLatestComputerScreenOpensLiveDock() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_SCREEN_FIXTURE"] == "1" else {
            throw XCTSkip("Run fixtures/remote-screen.mjs on loopback port 18965")
        }
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString,
                          "NANOCODEX_DEMO_SCREENS": "1",
                          "NANOCODEX_DEMO_GENERATED_OUTPUTS": "1",
                          "NANOCODEX_DEMO_LIVE_SCREEN_ENTRY": "1"])
        switchConversation(app, id: "inbox")
        let latest = app.buttons["latest-computer-screen"]
        XCTAssertTrue(latest.waitForExistence(timeout: 5))
        latest.tap()
        let panel = app.descendants(matching: .any)["thread-screen-panel"].firstMatch
        XCTAssertTrue(panel.waitForExistence(timeout: 5))
        XCTAssertFalse(latest.exists)
        let desktop = app.buttons["thread-screen:cf:fixture:desktop"]
        XCTAssertTrue(desktop.waitForExistence(timeout: 5))
        desktop.tap()
        // frames-v1 reports Watching only after decoding its first JPEG frame.
        XCTAssertTrue(app.staticTexts["Watching"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.descendants(matching: .any)["thread-screen-canvas"].firstMatch.exists)
        XCTAssertTrue(app.staticTexts["View only"].exists)
        let draft = "Keep my live screen draft"
        composer(app).tap(); composer(app).typeText(draft)
        capture(app, "latest-computer-screen-live")
        app.buttons["thread-screen-close"].tap()
        XCTAssertFalse(panel.exists)
        XCTAssertTrue(latest.waitForExistence(timeout: 5))
        XCTAssertEqual(composer(app).value as? String, draft)
    }

    private func openDrawerFromEdge(_ app: XCUIApplication) {
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.015, dy: 0.45))
            .press(forDuration: 0.01, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.75, dy: 0.45)))
        XCTAssertTrue(app.descendants(matching: .any)["conversation-list"].firstMatch.waitForExistence(timeout: 5))
    }

    func testScreenEdgeOpensDrawerAndPreservesDraft() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        selectInbox(app)
        composer(app).tap(); composer(app).typeText("Keep my edge swipe draft")
        openDrawerFromEdge(app)
        XCTAssertFalse(app.keyboards.firstMatch.exists)
        capture(app, "edge-swipe-open")
        // Compare the unobstructed left edge above and inside the home area.
        // The drawer surface must reach the screen bottom without a white strip.
        let screenshot = app.screenshot().image
        func edgePixel(_ y: CGFloat) -> [UInt8] {
            guard let pixel = screenshot.cgImage?.cropping(to: CGRect(
                x: 4 * screenshot.scale, y: y * screenshot.scale, width: 1, height: 1
            )) else { XCTFail("Expected drawer screenshot pixels"); return [] }
            var bytes = [UInt8](repeating: 0, count: 4)
            bytes.withUnsafeMutableBytes { buffer in
                let context = CGContext(data: buffer.baseAddress, width: 1, height: 1,
                    bitsPerComponent: 8, bytesPerRow: 4, space: CGColorSpaceCreateDeviceRGB(),
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                context.draw(pixel, in: CGRect(x: 0, y: 0, width: 1, height: 1))
            }
            return bytes
        }
        let sidebar = edgePixel(screenshot.size.height / 2)
        let homeArea = edgePixel(screenshot.size.height - 10)
        for (above, below) in zip(sidebar, homeArea) {
            XCTAssertEqual(Double(above), Double(below), accuracy: 3,
                           "The drawer background must continue through the home area")
        }
        app.buttons["conversation-drawer-close"].tap()
        gone(app.descendants(matching: .any)["conversation-list"].firstMatch)
        XCTAssertTrue(app.buttons["conversation-title:inbox"].isSelected)
        XCTAssertEqual(composer(app).value as? String, "Keep my edge swipe draft")
        let dock = app.descendants(matching: .any)["main-selection-bar"].firstMatch
        XCTAssertTrue(dock.exists)
        let dockFrame = dock.frame
        let inputFrame = composer(app).frame
        // A short pull that is released slowly must return to the conversation.
        let edge = app.coordinate(withNormalizedOffset: CGVector(dx: 0.015, dy: 0.45))
        edge.press(forDuration: 0.01, thenDragTo: edge.withOffset(CGVector(dx: 35, dy: 0)),
                   withVelocity: .slow, thenHoldForDuration: 0.5)
        gone(app.descendants(matching: .any)["conversation-list"].firstMatch)
        XCTAssertTrue(app.buttons["conversation-title:inbox"].isSelected)
        XCTAssertEqual(composer(app).value as? String, "Keep my edge swipe draft")
        XCTAssertEqual(dock.frame.minY, dockFrame.minY, accuracy: 2)
        XCTAssertEqual(composer(app).frame.minY, inputFrame.minY, accuracy: 2)
        capture(app, "edge-swipe-cancelled-dock")
        composer(app).tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5))
        XCTAssertLessThanOrEqual(dock.frame.maxY, app.keyboards.firstMatch.frame.minY)
        openDrawerFromEdge(app)
        app.buttons["conversation-row:data"].tap()
        XCTAssertTrue(app.buttons["conversation-title:data"].waitForExistence(timeout: 5))
        navigationAction(app, "Back").tap()
        XCTAssertEqual(composer(app).value as? String, "Keep my edge swipe draft")
    }

    func testAttachmentLibrarySheetPreservesDraft() {
        let originalAppearance = XCUIDevice.shared.appearance
        addTeardownBlock { XCUIDevice.shared.appearance = originalAppearance }
        for appearance in ["light", "dark"] {
            XCUIDevice.shared.appearance = appearance == "dark" ? .dark : .light
            let app = launch(["NANOCODEX_DEMO_APPEARANCE": appearance,
                              "NANOCODEX_DEMO_COMPOSER_PHOTOS": "1"])
            selectInbox(app)
            let draft = "Keep this draft when the attachment library is dismissed"
            composer(app).tap(); composer(app).typeText(draft)
            let removals = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "remove-attachment-"))
            XCTAssertTrue(removals.firstMatch.waitForExistence(timeout: 5))
            let attachmentIDs = removals.allElementsBoundByIndex.map(\.identifier)
            let sheet = app.descendants(matching: .any)["attachment-library-sheet"].firstMatch
            for cycle in 1...2 {
                app.buttons["add-attachments"].tap()
                XCTAssertTrue(sheet.waitForExistence(timeout: 5))
                XCTAssertTrue(app.staticTexts["Library"].exists)
                XCTAssertTrue(app.scrollViews["recent-photos"].exists)
                for identifier in ["choose-camera", "choose-photos", "choose-files", "choose-videos"] {
                    let action = app.buttons[identifier]
                    XCTAssertTrue(action.isHittable, "Library action must be reachable without scrolling: " + identifier)
                    XCTAssertTrue(app.frame.contains(action.frame), "Library action must fit on screen: " + identifier)
                }
                capture(app, "attachment-library-" + appearance + "-" + String(cycle))
                dismissAttachmentLibrary(app)
                XCTAssertEqual(composer(app).value as? String, draft)
                XCTAssertEqual(removals.allElementsBoundByIndex.map(\.identifier), attachmentIDs,
                               "Opening and dismissing the library must preserve existing photo attachments")
            }
            capture(app, "attachment-library-preserved-draft-" + appearance)
            app.terminate()
        }
    }

    private func dismissAttachmentLibrary(_ app: XCUIApplication) {
        let sheet = app.descendants(matching: .any)["attachment-library-sheet"].firstMatch
        XCTAssertTrue(sheet.waitForExistence(timeout: 5))
        let grabber = app.buttons["Sheet Grabber"]
        XCTAssertTrue(grabber.waitForExistence(timeout: 5))
        grabber.swipeDown()
        gone(sheet)
        XCTAssertTrue(app.buttons["add-attachments"].isHittable)
    }

    func testConversationDrawerAndSheetsPreserveIndependentDrafts() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        selectInbox(app)
        composer(app).tap(); composer(app).typeText("Keep this draft through navigation")
        app.buttons["conversation-drawer-open"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["conversation-list"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["send"].isHittable, "The covered conversation must not receive taps")
        capture(app, "muse-session-drawer")
        let search = app.textFields["conversation-search"]
        search.tap(); search.typeText("durability")
        let row = app.buttons["conversation-row:durability"]
        XCTAssertTrue(row.waitForExistence(timeout: 5)); row.tap()
        XCTAssertEqual(selectedConversationTab(app).label, "Make long sessions bulletproof")
        XCTAssertNotEqual(composer(app).value as? String, "Keep this draft through navigation")
        navigationAction(app, "Back").tap()
        XCTAssertEqual(composer(app).value as? String, "Keep this draft through navigation")
        app.buttons["add-attachments"].tap()
        XCTAssertTrue(app.buttons["choose-photos"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["choose-context"].exists)
        capture(app, "muse-attachment-sheet")
        dismissAttachmentLibrary(app)
        XCTAssertEqual(composer(app).value as? String, "Keep this draft through navigation")
        navigationAction(app, "Account settings").tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5))
        app.buttons["Done"].tap()
        XCTAssertEqual(composer(app).value as? String, "Keep this draft through navigation")
        capture(app, "muse-conversation-dock")
    }

    #if DEBUG && targetEnvironment(simulator)
    private func startupFixture(reject: Bool = false, historyWindow: Bool = false, warmTabs: Bool = false, historyMedia: Bool = false, historyDelay: Int = 3000, liveReading: Bool = false) -> XCUIApplication {
        addUIInterruptionMonitor(withDescription: "Isolated simulator notifications") { alert in
            guard alert.buttons["Don’t Allow"].exists || alert.buttons["Don't Allow"].exists else { return false }
            let button = alert.buttons["Don’t Allow"].exists ? alert.buttons["Don’t Allow"] : alert.buttons["Don't Allow"]
            button.tap(); return true
        }
        let app = XCUIApplication()
        app.launchEnvironment = ["NANOCODEX_STARTUP_FIXTURE": "1", "NANOCODEX_STARTUP_PROFILE": UUID().uuidString.lowercased(),
                                 "NANOCODEX_STARTUP_REJECT": reject ? "1" : "0", "NANOCODEX_STARTUP_HISTORY_WINDOW": historyWindow ? "1" : "0",
                                 "NANOCODEX_STARTUP_WARM_TABS": warmTabs ? "1" : "0", "NANOCODEX_STARTUP_HISTORY_MEDIA": historyMedia ? "1" : "0",
                                 "NANOCODEX_STARTUP_HISTORY_DELAY_MS": String(historyDelay),
                                 "NANOCODEX_STARTUP_LIVE_READING": liveReading ? "1" : "0"]
        app.launch()
        return app
    }
    func testSessionDoneSwipePersistsAndReopensWithoutLosingHistory() {
        let app = XCUIApplication()
        app.launchEnvironment = ["NANOCODEX_STARTUP_FIXTURE": "1", "NANOCODEX_STARTUP_PROFILE": UUID().uuidString,
                                 "NANOCODEX_SESSION_DONE_FIXTURE": "1"]
        app.launch()
        XCTAssertTrue(app.buttons["main-tab-chat"].waitForExistence(timeout: 20))
        app.buttons["main-tab-chat"].tap()
        XCTAssertTrue(app.staticTexts["Loaded saved conversation."].waitForExistence(timeout: 20))
        app.buttons["conversation-drawer-open"].tap()
        let row = app.buttons["conversation-row:saved"]
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        row.swipeLeft()
        let mark = app.buttons["Mark Done"]
        XCTAssertTrue(mark.waitForExistence(timeout: 5))
        XCTAssertEqual(mark.label, "Mark Done")
        capture(app, "session-done-swipe")
        mark.tap()
        XCTAssertTrue(row.waitForNonExistence(timeout: 10))
        app.buttons["conversation-done-filter"].tap()
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        capture(app, "session-done-filter")
        row.tap()
        XCTAssertTrue(app.staticTexts["Loaded saved conversation."].waitForExistence(timeout: 10))
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["main-tab-chat"].waitForExistence(timeout: 20))
        app.buttons["main-tab-chat"].tap()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 20))
        app.buttons["conversation-drawer-open"].tap()
        XCTAssertTrue(app.buttons["conversation-row:other"].waitForExistence(timeout: 10))
        XCTAssertFalse(row.exists)
        app.buttons["conversation-done-filter"].tap()
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        row.swipeLeft()
        let reopen = app.buttons["Reopen"]
        XCTAssertTrue(reopen.waitForExistence(timeout: 5))
        XCTAssertEqual(reopen.label, "Reopen")
        reopen.tap()
        XCTAssertTrue(row.waitForNonExistence(timeout: 10))
        app.buttons["conversation-done-filter"].tap()
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        row.tap()
        XCTAssertTrue(app.staticTexts["Loaded saved conversation."].waitForExistence(timeout: 10))
        capture(app, "session-done-reopened-history")
    }

    func testSessionDoneRowsAndBlankDrawerSwipesStayIndependent() {
        let app = XCUIApplication()
        app.launchEnvironment = ["NANOCODEX_STARTUP_FIXTURE": "1", "NANOCODEX_STARTUP_PROFILE": UUID().uuidString,
                                 "NANOCODEX_SESSION_DONE_FIXTURE": "1"]
        app.launch()
        XCTAssertTrue(app.buttons["main-tab-chat"].waitForExistence(timeout: 20))
        app.buttons["main-tab-chat"].tap()
        XCTAssertTrue(app.staticTexts["Loaded saved conversation."].waitForExistence(timeout: 20))
        app.buttons["conversation-drawer-open"].tap()
        let saved = app.buttons["conversation-row:saved"]
        let other = app.buttons["conversation-row:other"]
        XCTAssertTrue(saved.waitForExistence(timeout: 10))
        XCTAssertTrue(other.waitForExistence(timeout: 10))
        let list = app.descendants(matching: .any)["conversation-list"].firstMatch
        XCTAssertTrue(list.waitForExistence(timeout: 5))
        let y = list.frame.maxY - 20
        XCTAssertGreaterThan(y, max(saved.frame.maxY, other.frame.maxY) + 8,
                             "Use actual blank list space, not a native row's swipe region")
        let origin = app.coordinate(withNormalizedOffset: .zero)
        let start = origin.withOffset(CGVector(dx: list.frame.minX + list.frame.width * 0.8, dy: y))
        let end = origin.withOffset(CGVector(dx: list.frame.minX + 8, dy: y))
        start.press(forDuration: 0.1, thenDragTo: end)
        let history = app.staticTexts["Loaded saved conversation."]
        let visible = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true AND hittable == true"), object: history)
        XCTAssertEqual(XCTWaiter.wait(for: [visible], timeout: 10), .completed,
                       "Blank-space dismissal must restore the retained conversation")
        app.buttons["conversation-drawer-open"].tap()
        XCTAssertTrue(saved.waitForExistence(timeout: 10))
        saved.swipeLeft()
        XCTAssertTrue(app.buttons["Mark Done"].waitForExistence(timeout: 5),
                      "A native row swipe must reveal Done instead of dismissing the drawer")
        XCTAssertTrue(app.buttons["conversation-drawer-close"].isHittable)
        capture(app, "session-done-row-and-blank-swipes")
    }

    func testSessionDoneRejectedWriteLeavesSessionVisible() {
        let app = XCUIApplication()
        app.launchEnvironment = ["NANOCODEX_STARTUP_FIXTURE": "1", "NANOCODEX_STARTUP_PROFILE": UUID().uuidString,
                                 "NANOCODEX_SESSION_DONE_FIXTURE": "1", "NANOCODEX_SESSION_DONE_FAILURE": "before"]
        app.launch()
        XCTAssertTrue(app.buttons["main-tab-chat"].waitForExistence(timeout: 20))
        app.buttons["main-tab-chat"].tap()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 20))
        app.buttons["conversation-drawer-open"].tap()
        let row = app.buttons["conversation-row:saved"]
        XCTAssertTrue(row.waitForExistence(timeout: 10)); row.swipeLeft()
        let mark = app.buttons["Mark Done"]
        XCTAssertTrue(mark.waitForExistence(timeout: 5)); mark.tap()
        let error = app.staticTexts["conversation-done-error"]
        let reconciled = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS %@", "no automatic retry"), object: error)
        XCTAssertEqual(XCTWaiter.wait(for: [reconciled], timeout: 10), .completed)
        XCTAssertTrue(row.exists)
        app.buttons["conversation-done-filter"].tap()
        XCTAssertFalse(row.exists)
        capture(app, "session-done-rejected-main-list-preserved")
    }

    func testSessionDoneUncertainWriteRefreshesWithoutRetry() {
        let app = XCUIApplication()
        app.launchEnvironment = ["NANOCODEX_STARTUP_FIXTURE": "1", "NANOCODEX_STARTUP_PROFILE": UUID().uuidString,
                                 "NANOCODEX_SESSION_DONE_FIXTURE": "1", "NANOCODEX_SESSION_DONE_FAILURE": "after"]
        app.launch()
        XCTAssertTrue(app.buttons["main-tab-chat"].waitForExistence(timeout: 20))
        app.buttons["main-tab-chat"].tap()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 20))
        app.buttons["conversation-drawer-open"].tap()
        let row = app.buttons["conversation-row:saved"]
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        row.swipeLeft()
        let mark = app.buttons["Mark Done"]
        XCTAssertTrue(mark.waitForExistence(timeout: 5)); mark.tap()
        XCTAssertTrue(app.staticTexts["conversation-done-error"].waitForExistence(timeout: 10))
        XCTAssertTrue(row.waitForNonExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["conversation-done-error"].label.contains("no automatic retry"))
        app.buttons["conversation-done-filter"].tap()
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        capture(app, "session-done-uncertain-refreshed")
    }

    // A process restart must recover persisted roster, transcript, directory,
    // and visited profile even when every network request fails immediately.
    func testOfflineColdRelaunchRestoresConversationsAndCRM() {
        let app = startupFixture(warmTabs: true)
        XCTAssertTrue(app.buttons["main-tab-chat"].waitForExistence(timeout: 10))
        app.buttons["main-tab-chat"].tap()
        XCTAssertTrue(app.staticTexts["Loaded saved conversation."].waitForExistence(timeout: 10))
        switchConversation(app, id: "other")
        XCTAssertTrue(app.staticTexts["Loaded other conversation."].waitForExistence(timeout: 10))
        app.buttons["main-tab-crm"].tap()
        XCTAssertTrue(app.buttons["crm-record-alex"].waitForExistence(timeout: 5))
        app.buttons["crm-record-alex"].tap()
        XCTAssertTrue(app.staticTexts["Example University"].waitForExistence(timeout: 5))
        capture(app, "offline-cache-populated")

        app.terminate()
        app.launchEnvironment["NANOCODEX_STARTUP_OFFLINE"] = "1"
        app.launch()
        XCTAssertTrue(app.buttons["main-tab-chat"].waitForExistence(timeout: 5))
        app.buttons["main-tab-chat"].tap()
        XCTAssertTrue(app.buttons["conversation-title:other"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["conversation-title:other"].isSelected)
        XCTAssertTrue(app.staticTexts["Loaded other conversation."].waitForExistence(timeout: 5))
        XCTAssertFalse(app.textFields["phone-number"].exists)
        switchConversation(app, id: "saved")
        XCTAssertTrue(app.staticTexts["Loaded saved conversation."].waitForExistence(timeout: 5))
        capture(app, "offline-cold-restored-conversation")
        app.buttons["main-tab-crm"].tap()
        XCTAssertTrue(app.buttons["crm-record-alex"].waitForExistence(timeout: 5))
        app.buttons["crm-record-alex"].tap()
        XCTAssertTrue(app.staticTexts["Example University"].waitForExistence(timeout: 5))
        capture(app, "offline-cold-restored-crm-profile")
    }

    func testStartupSpinnerAndLastTabRestoration() {
        let app = startupFixture()
        XCTAssertTrue(app.activityIndicators["account-restoration"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["Opening your inbox…"].exists)
        capture(app, "startup-account-spinner")
        XCTAssertTrue(app.buttons["conversation-title:saved"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.buttons["conversation-title:saved"].isSelected)
        XCTAssertFalse(app.staticTexts["Loading conversation…"].exists)
        gone(app.descendants(matching: .any)["conversation-loading"].firstMatch, timeout: 20)
        XCTAssertTrue(app.staticTexts["Loaded saved conversation."].waitForExistence(timeout: 5))
        switchConversation(app, id: "other")
        XCTAssertTrue(app.activityIndicators["conversation-loading"].waitForExistence(timeout: 5))
        capture(app, "startup-conversation-spinner")
        XCTAssertTrue(app.staticTexts["Loaded other conversation."].waitForExistence(timeout: 20))
        XCUIDevice.shared.press(.home)
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["conversation-title:other"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.buttons["conversation-title:other"].isSelected)
        XCTAssertTrue(app.staticTexts["Loaded other conversation."].waitForExistence(timeout: 20))
        XCTAssertFalse(app.staticTexts["Loaded saved conversation."].exists)
        capture(app, "startup-restored-tab")
    }
    func testCachedTabsSurviveBackgroundWithoutReloading() {
        let app = startupFixture(warmTabs: true)
        XCTAssertTrue(app.staticTexts["Loaded saved conversation."].waitForExistence(timeout: 10))
        switchConversation(app, id: "other")
        XCTAssertTrue(app.staticTexts["Loaded other conversation."].waitForExistence(timeout: 10))
        for _ in 0..<3 {
            for id in ["saved", "other"] {
                switchConversation(app, id: id)
                XCTAssertFalse(app.activityIndicators["conversation-loading"].exists)
                XCTAssertTrue(app.staticTexts["Loaded \(id) conversation."].exists)
            }
        }
        XCUIDevice.shared.press(.home)
        app.activate()
        switchConversation(app, id: "saved")
        let saved = app.buttons["conversation-title:saved"]
        XCTAssertTrue(saved.isSelected)
        XCTAssertFalse(app.activityIndicators["conversation-loading"].exists,
                       "A normal background/foreground cycle must retain bounded cached tabs")
        XCTAssertTrue(app.staticTexts["Loaded saved conversation."].exists)
        capture(app, "warm-tab-restored-after-background")
    }

    func testStartupSwitchCancelsObsoleteHistory() {
        let app = startupFixture()
        XCTAssertTrue(app.buttons["conversation-title:saved"].waitForExistence(timeout: 15))
        switchConversation(app, id: "other")
        XCTAssertTrue(app.staticTexts["Loaded other conversation."].waitForExistence(timeout: 20))
        XCTAssertFalse(app.staticTexts["Loaded saved conversation."].exists)
        XCTAssertTrue(app.buttons["conversation-title:other"].isSelected)
    }
    func testConnectorCenterShowsMultipleAccountsAndSearchesAvailableProviders() {
        let app = startupFixture()
        XCTAssertTrue(app.buttons["conversation-title:saved"].waitForExistence(timeout: 15))
        app.descendants(matching: .any).matching(identifier: "app-menu").firstMatch.tap()
        let connectors = app.buttons["inbox-connectors"]
        XCTAssertTrue(connectors.waitForExistence(timeout: 5))
        connectors.tap()

        XCTAssertTrue(app.descendants(matching: .any)["connectors-list"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["connector-connected:google"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["mcp-connected:" + String(repeating: "m", count: 43)].exists)
        XCTAssertTrue(app.buttons["connector-available:slack"].exists)
        XCTAssertTrue(app.buttons["mcp-available:" + String(repeating: "l", count: 43)].exists)
        XCTAssertTrue(app.buttons["mcp-add"].exists)
        capture(app, "mobile-connectors-multiple-accounts")

        app.buttons["connector-connected:google"].tap()
        XCTAssertTrue(app.staticTexts["georgios@paradigm.xyz"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["me@gakonst.com"].exists)
        XCTAssertTrue(app.buttons["connector-add-account"].exists)
        app.buttons["Revoke"].firstMatch.tap()
        XCTAssertTrue(app.buttons["Revoke account"].waitForExistence(timeout: 3))
        if app.buttons["Cancel"].exists { app.buttons["Cancel"].tap() }
        else { app.coordinate(withNormalizedOffset: CGVector(dx: 0.05, dy: 0.55)).tap() }
        gone(app.buttons["Revoke account"])
        capture(app, "mobile-connector-account-detail")

        app.navigationBars.buttons.firstMatch.tap()
        let search = app.searchFields["Search connectors"]
        XCTAssertTrue(search.waitForExistence(timeout: 3))
        search.tap(); search.typeText("Slack")
        XCTAssertTrue(app.buttons["connector-available:slack"].exists)
        XCTAssertFalse(app.buttons["connector-connected:google"].exists)
        search.tap(); app.buttons["Clear text"].tap(); search.typeText("Spotify")
        XCTAssertTrue(app.buttons["connector-connected:spotify"].exists)
        XCTAssertFalse(app.buttons["connector-available:slack"].exists)
        search.tap(); app.buttons["Clear text"].tap(); search.typeText("SoundCloud")
        let soundcloud = app.buttons["connector-available:soundcloud"]
        XCTAssertTrue(soundcloud.exists)
        soundcloud.tap()
        XCTAssertTrue(app.buttons["connect-soundcloud"].waitForExistence(timeout: 5))
        app.buttons["Done"].tap()
        search.tap(); app.buttons["Clear text"].tap(); search.typeText("Mercator")
        XCTAssertTrue(app.buttons["mcp-connected:" + String(repeating: "m", count: 43)].exists)
        XCTAssertFalse(app.buttons["connector-available:slack"].exists)
    }
    func testDrawerDismissalPreservesConversationAcrossLaunches() {
        let app = startupFixture()
        XCTAssertTrue(app.buttons["conversation-title:saved"].waitForExistence(timeout: 20))
        app.buttons["conversation-drawer-open"].tap()
        XCTAssertTrue(app.buttons["conversation-row:saved"].waitForExistence(timeout: 5))
        app.buttons["conversation-drawer-close"].tap()
        gone(app.descendants(matching: .any)["conversation-list"].firstMatch)
        XCTAssertTrue(app.buttons["conversation-title:saved"].isSelected)
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["conversation-title:saved"].waitForExistence(timeout: 20))
        app.buttons["conversation-drawer-open"].tap()
        XCTAssertTrue(app.buttons["conversation-row:saved"].waitForExistence(timeout: 5))
        app.buttons["conversation-row:saved"].tap()
        XCTAssertTrue(app.staticTexts["Loaded saved conversation."].waitForExistence(timeout: 20))
    }

    func testLongActiveTranscriptDrawerScrollPreservesSelectionAndSearch() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString,
                          "NANOCODEX_DEMO_LONG_THREAD": "1",
                          "NANOCODEX_DEMO_SIDEBAR": "1"])
        selectInbox(app)
        app.buttons["conversation-drawer-open"].tap()
        let list = app.descendants(matching: .any)["conversation-list"].firstMatch
        XCTAssertTrue(list.waitForExistence(timeout: 5))
        for _ in 0..<3 { list.swipeUp(); list.swipeDown() }
        XCTAssertTrue(list.exists, "Vertical browsing must keep the drawer open")
        let search = app.textFields["conversation-search"]
        search.tap(); search.typeText("inbox")
        XCTAssertTrue(app.buttons["conversation-row:inbox"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["conversation-row:hands"].exists)
        app.buttons["Clear search"].tap()
        XCTAssertTrue(app.buttons["conversation-row:hands"].waitForExistence(timeout: 5))
        app.buttons["conversation-drawer-close"].tap()
        gone(list)
        XCTAssertTrue(app.buttons["conversation-title:inbox"].isSelected)
    }

    func testLiveArrivalAppearsWhileReadingHistoryWithoutMovingReader() {
        let app = startupFixture(historyWindow: true, liveReading: true)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 15))
        let first = conversation.staticTexts["History page 1 of 3"]
        for _ in 0..<12 {
            if first.exists && first.isHittable { break }
            conversation.swipeDown(velocity: .slow)
        }
        XCTAssertTrue(first.isHittable)
        let y = first.frame.minY
        let live = conversation.staticTexts["Fixture live arrival beyond history window."]
        let moved = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            !first.isHittable || abs(first.frame.minY - y) >= 8
        }, object: nil)
        moved.isInverted = true
        XCTAssertEqual(XCTWaiter.wait(for: [moved], timeout: 7), .completed,
                       "Incoming text must preserve the older message's reading position")
        XCTAssertTrue(live.waitForExistence(timeout: 5),
                      "Incoming agent text must enter the transcript without requesting newer messages")
        XCTAssertFalse(app.buttons["load-newer"].exists)
        XCTAssertTrue(first.isHittable)
        XCTAssertEqual(first.frame.minY, y, accuracy: 8)
        let latest = app.buttons["latest-messages"]
        XCTAssertTrue(latest.isHittable)
        capture(app, "live-arrival-preserves-history-reader")
        latest.tap()
        let visible = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: live)
        XCTAssertEqual(XCTWaiter.wait(for: [visible], timeout: 5), .completed,
                       "The down arrow must reach the incoming bubble without corrective swipes")
        gone(latest)
        capture(app, "live-arrival-arrow-reaches-bubble")
    }

    func testNativeHistoryWindowCrossesEventAndByteBudgetsAndReturnsToLiveTail() {
        let app = startupFixture(historyWindow: true)
        XCTAssertTrue(app.buttons["conversation-title:saved"].waitForExistence(timeout: 15))
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 10))
        let first = conversation.staticTexts["History page 1 of 20"]
        for _ in 0..<60 {
            if first.exists && first.isHittable { break }
            conversation.swipeDown(velocity: .fast)
        }
        XCTAssertTrue(first.isHittable, "Every page remains accessible beyond 2048 events and the 16 MiB memory target")
        capture(app, "history-window-oldest-page")
        let y = first.frame.minY
        // Live delivery is scheduled by reaching the oldest HTTP page. Reading
        // that page must remain stationary instead of refilling the newer tail.
        let latest = app.buttons["latest-messages"]
        XCTAssertTrue(latest.waitForExistence(timeout: 8))
        let moved = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            !first.isHittable || abs(first.frame.minY - y) >= 8
        }, object: nil)
        moved.isInverted = true
        XCTAssertEqual(XCTWaiter.wait(for: [moved], timeout: 4), .completed,
                       "History position changed from \(y) to \(first.frame.minY) while stationary")
        let next = conversation.staticTexts["History page 19 of 20"]
        for _ in 0..<50 {
            if next.exists {
                if next.isHittable { break }
                // A fast native fling may already have crossed this heading.
                if next.frame.maxY < conversation.frame.minY { conversation.swipeDown(velocity: .slow) }
                else { conversation.swipeUp(velocity: .slow) }
            } else { conversation.swipeUp(velocity: .fast) }
        }
        capture(app, "history-window-forward-position")
        XCTAssertTrue(next.isHittable, "Scrolling forward retrieves content trimmed while reading older pages\n" + app.debugDescription)
        capture(app, "history-window-forward-page")
        latest.tap()
        let live = conversation.staticTexts["Fixture live arrival beyond history window."]
        XCTAssertTrue(live.waitForExistence(timeout: 15))
        for _ in 0..<3 { if live.isHittable { break }; conversation.swipeUp() }
        XCTAssertTrue(live.isHittable)
        XCTAssertFalse(latest.exists, "The latest jump catches up to the independent live cursor")
        capture(app, "history-window-live-tail")
    }

    func testSameTurnMediaPrependKeepsVisibleImage() {
        let app = startupFixture(historyWindow: true, historyMedia: true)
        let conversation = app.descendants(matching: .any).matching(identifier: "conversation").firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 15))
        let image = conversation.descendants(matching: .any).matching(NSPredicate(format: "identifier == %@ AND label == %@", "generated-image-loaded", "Open History image 16")).firstMatch
        XCTAssertTrue(conversation.staticTexts["Completed image review."].waitForExistence(timeout: 15))
        let loading = app.descendants(matching: .any)["loading-older"].firstMatch
        for _ in 0..<8 {
            if loading.exists { break }
            conversation.swipeDown()
        }
        XCTAssertTrue(loading.exists)
        XCTAssertTrue(image.isHittable, app.debugDescription)
        let y = image.frame.minY
        capture(app, "same-turn-image-before-history")
        gone(loading, timeout: 8)
        XCTAssertTrue(image.isHittable)
        XCTAssertEqual(image.frame.minY, y, accuracy: 4, "Older outputs in the same turn cannot replace the image under the reader")
        let settled = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            loading.exists || !image.isHittable || abs(image.frame.minY - y) > 4
        }, object: nil)
        settled.isInverted = true
        XCTAssertEqual(XCTWaiter.wait(for: [settled], timeout: 4), .completed, "One history load must not cascade into more loads while stationary")
        capture(app, "same-turn-image-after-history")
    }

    func testSameTurnHistoryKeepsExpandedToolVisible() {
        let app = startupFixture(historyWindow: true, historyMedia: true, historyDelay: 12000)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 15))
        XCTAssertTrue(conversation.staticTexts["Completed image review."].waitForExistence(timeout: 15))
        let loading = app.descendants(matching: .any)["loading-older"].firstMatch
        let cards = conversation.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "tool-disclosure-"))
        for _ in 0..<8 {
            if loading.exists && cards.allElementsBoundByIndex.contains(where: { $0.isHittable }) { break }
            conversation.swipeDown()
        }
        XCTAssertTrue(loading.exists)
        guard let step = cards.allElementsBoundByIndex.first(where: { $0.isHittable }) else {
            return XCTFail("Expected an inline tool card")
        }
        step.tap()
        let id = step.identifier, screenY = step.frame.minY
        XCTAssertEqual(step.value as? String, "Expanded")
        capture(app, "same-turn-tool-before-history")
        gone(loading, timeout: 20)
        let retained = conversation.buttons[id]
        XCTAssertTrue(retained.isHittable, "The expanded tool stays under the reader while earlier media arrives")
        XCTAssertEqual(retained.value as? String, "Expanded")
        XCTAssertEqual(retained.frame.minY, screenY, accuracy: 4)
        capture(app, "same-turn-tool-after-history")
    }

    func testStartupRejectsUnauthorizedRoster() {
        let app = startupFixture(reject: true)
        XCTAssertTrue(app.textFields["phone-number"].waitForExistence(timeout: 15))
        XCTAssertFalse(app.buttons["conversation-title:saved"].exists)
        XCTAssertFalse(app.staticTexts["Loaded saved conversation."].exists)
    }
    #endif
    func testLiveVoiceGreeting() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_VOICE_GREETING_LIVE"] == "1" else { throw XCTSkip("Requires a signed-in phone and the synchronized greeting audio fixture.") }
        let app = XCUIApplication()
        app.launchEnvironment["NANOCODEX_VOICE_TIMING"] = "1"
        app.launch()
        XCTAssertTrue(app.buttons["new-conversation"].waitForExistence(timeout: 30))
        app.buttons["new-conversation"].tap()
        XCTAssertTrue(app.buttons["start-voice"].waitForExistence(timeout: 10))
        app.buttons["start-voice"].tap()
        defer {
            if app.buttons["end-voice-compact"].exists { app.buttons["end-voice-compact"].tap() }
            else if app.buttons["end-voice"].exists { app.buttons["end-voice"].tap() }
        }
        let connected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label IN %@", ["Listening", "Speaking", "Working on it", "Voice paused"]), object: app.staticTexts["voice-status"])
        XCTAssertEqual(XCTWaiter.wait(for: [connected], timeout: 40), .completed)
        XCTAssertNotEqual(app.staticTexts["voice-status"].label, "Voice paused")
        app.buttons["close-voice"].tap()
        XCTAssertTrue(app.buttons["end-voice-compact"].waitForExistence(timeout: 5))
        let assistantCount = app.otherElements.matching(identifier: "voice-transcript-assistant").count
        FileHandle.standardOutput.write(Data("PHONE_VOICE_INPUT_READY at=\(Date().timeIntervalSince1970)\n".utf8))
        XCTAssertTrue(app.otherElements["voice-transcript-user"].firstMatch.waitForExistence(timeout: 15))
        let reply = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.otherElements.matching(identifier: "voice-transcript-assistant").count > assistantCount
        }, object: app)
        let replied = XCTWaiter.wait(for: [reply], timeout: 30)
        capture(app, "voice-greeting-reply")
        XCTAssertEqual(replied, .completed)
    }
    func testRemoteScreenControlAndReconnect() throws {
        let environment = ProcessInfo.processInfo.environment
        guard let origin = environment["NANOCODEX_TEST_REMOTE_ORIGIN"],
              let machine = environment["NANOCODEX_TEST_VM_MACHINE_ID"], machine.hasPrefix("vm:") else {
            throw XCTSkip("Requires a signed-in account and an explicitly selected disposable VM with a focused test terminal")
        }
        XCTAssertEqual(URL(string: origin)?.scheme, "https")
        let app = XCUIApplication()
        app.launchEnvironment["NANOCODEX_REMOTE_DIAGNOSTICS"] = "1"
        app.launch()
        defer { print("Remote status: \(app.staticTexts["remote-status"].debugDescription)") }
        let screens = app.buttons["Remote screens"]
        XCTAssertTrue(screens.waitForExistence(timeout: 20)); screens.tap()
        let desktop = app.buttons["remote-screen:\(machine):desktop"]
        XCTAssertTrue(desktop.waitForExistence(timeout: 15)); desktop.tap()
        let control = app.buttons["Take control"]
        XCTAssertTrue(control.waitForExistence(timeout: 10))
        func waitForConnection(timeout: TimeInterval = 45) {
            let connected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: control)
            let result = XCTWaiter.wait(for: [connected], timeout: timeout)
            if result != .completed {
                print("Remote connection failure: \(app.staticTexts["remote-status"].debugDescription)")
                capture(app, "remote-ios-connection-failure")
            }
            XCTAssertEqual(result, .completed)
        }
        waitForConnection()
        control.tap()
        XCTAssertTrue(app.buttons["Release control"].waitForExistence(timeout: 5))
        let canvas = app.otherElements["remote-canvas"]
        XCTAssertTrue(canvas.waitForExistence(timeout: 5)); canvas.tap()
        let remoteReturn = app.buttons.matching(NSPredicate(format: "label == %@", "Return")).firstMatch
        remoteReturn.tap()
        let text = app.textFields["Type on remote screen"]
        let marker = "ios-native-control-" + UUID().uuidString.lowercased()
        text.tap(); text.typeText("touch /workspace/\(marker)")
        app.buttons["Send"].tap(); remoteReturn.tap()
        capture(app, "remote-ios-control")
        // Leave with control and an unsent draft. Returning must retain the
        // selected screen, release control, and clear input before reconnecting.
        text.tap(); text.typeText("must-not-be-replayed")
        XCUIDevice.shared.press(.home)
        XCTAssertTrue(app.wait(for: .runningBackground, timeout: 10))
        app.activate()
        XCTAssertTrue(control.waitForExistence(timeout: 15))
        waitForConnection()
        XCTAssertFalse(app.buttons["Release control"].exists)
        XCTAssertTrue(canvas.exists, "Returning must reopen the selected screen without tapping its catalog row")
        capture(app, "remote-ios-background-resumed")
        control.tap()
        XCTAssertTrue(app.buttons["Release control"].waitForExistence(timeout: 5))
        XCTAssertEqual(text.value as? String, "Type on remote screen", "Unsent input must be cleared after losing control")
        text.tap(); text.typeText("ls /workspace/\(marker)")
        app.buttons["Send"].tap(); remoteReturn.tap()
        capture(app, "remote-ios-input-after-resume")
        print("Remote VM evidence file: /workspace/\(marker)")
        app.buttons["Release control"].tap()
        app.buttons["Done"].tap(); app.terminate(); app.launch()
        XCTAssertTrue(screens.waitForExistence(timeout: 20)); screens.tap()
        XCTAssertTrue(desktop.waitForExistence(timeout: 15)); desktop.tap()
        XCTAssertTrue(control.waitForExistence(timeout: 10))
        waitForConnection()
        control.tap(); XCTAssertTrue(app.buttons["Release control"].waitForExistence(timeout: 5))
        capture(app, "remote-ios-reconnected")
        app.buttons["Release control"].tap(); app.buttons["Done"].tap()
        // The same viewer must be reachable without leaving an open chat.
        // Read an existing chat. Only the explicitly selected disposable VM
        // receives input; the conversation itself is never changed.
        app.buttons["conversation-drawer-open"].tap()
        let overview = app.descendants(matching: .any)["conversation-list"].firstMatch
        XCTAssertTrue(overview.waitForExistence(timeout: 10))
        let chat = try XCTUnwrap(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "conversation-row:")).allElementsBoundByIndex.first { $0.isHittable })
        chat.tap(); gone(overview)
        let title = self.selectedConversationTab(app)
        XCTAssertTrue(title.waitForExistence(timeout: 10)); title.tap()
        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))
        let chatScreens = navigationAction(app, "conversation-remote-screens")
        XCTAssertTrue(chatScreens.waitForExistence(timeout: 5)); chatScreens.tap()
        XCTAssertTrue(app.buttons["thread-screen-options"].waitForExistence(timeout: 5))
        app.buttons["thread-screen-options"].tap(); app.buttons["Screen controls"].tap()
        XCTAssertTrue(desktop.waitForExistence(timeout: 15)); desktop.tap()
        XCTAssertTrue(control.waitForExistence(timeout: 10))
        waitForConnection()
        capture(app, "remote-ios-from-conversation")
        if environment["NANOCODEX_TEST_REMOTE_RESTART"] == "1" {
            print("REMOTE_RESTART_READY")
            let disconnected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == false"), object: control)
            XCTAssertEqual(XCTWaiter.wait(for: [disconnected], timeout: 30), .completed)
            waitForConnection(timeout: 90)
            XCTAssertFalse(app.buttons["Release control"].exists)
            control.tap()
            XCTAssertTrue(app.buttons["Release control"].waitForExistence(timeout: 5))
            canvas.tap()
            text.tap(); text.typeText("ls /workspace/\(marker); touch /workspace/\(marker)-after-restart")
            app.buttons["Send"].tap(); remoteReturn.tap()
            capture(app, "remote-ios-vm-restarted")
            app.buttons["Release control"].tap()
        }
        app.buttons["Done"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))
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
        try require(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 30), "The inbox did not connect")
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
            try require(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 30), "The inbox did not connect")
            let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: app.buttons["conversation-drawer-open"])
            try require(XCTWaiter.wait(for: [ready], timeout: 10) == .completed, "The inbox did not finish loading")
            let title = self.selectedConversationTab(app)
            if !title.label.contains(marker) {
                selectAgentFromDrawer(app, title: marker, id: agentID)
            }
            title.tap()
            let conversation = app.descendants(matching: .any)["conversation"].firstMatch
            try require(conversation.waitForExistence(timeout: 5), "The conversation did not open")
            for _ in 0..<30 {
                if command(commandText).exists && command(commandText).isHittable { break }
                conversation.swipeDown()
            }
            try require(command(commandText).isHittable, "The inline command card did not appear")
            try require(command(commandText).identifier.contains(turnID), "The command belongs to the requested turn")
        }
        func command(_ text: String) -> XCUIElement {
            app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'tool-disclosure-' AND label CONTAINS %@", text)).firstMatch
        }
        func revealOutput(_ predicate: NSPredicate, command: XCUIElement) throws {
            let detailID = command.identifier.replacingOccurrences(of: "tool-disclosure-", with: "tool-detail-")
            let detail = app.descendants(matching: .any).matching(identifier: detailID).firstMatch
            let conversation = app.descendants(matching: .any)["conversation"].firstMatch
            try require(detail.waitForExistence(timeout: 5), "Command details did not expand")
            let output = detail.staticTexts.matching(predicate).firstMatch
            try require(output.waitForExistence(timeout: 5), "The command result did not contain the expected output")
            func visibleTail() -> Bool {
                let viewport = conversation.frame.intersection(app.frame)
                return !viewport.isNull && output.frame.maxY > viewport.minY && output.frame.maxY <= viewport.maxY + 4
            }
            for _ in 0..<40 {
                if visibleTail() { break }
                conversation.swipeUp()
            }
            try require(visibleTail(), "The command output tail was not visible in the main conversation scroll")
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
        selectAgentFromDrawer(app, title: "Make long sessions bulletproof")
        let title = self.selectedConversationTab(app).label
        composer(app).tap(); composer(app).typeText("Keep my draft while I check jobs")

        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))

        navigationAction(app, "inbox-scheduled-jobs").coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        let active = app.buttons["scheduled-job-durability/daily-check"]
        let paused = app.buttons["scheduled-job-data/daily-check"]
        XCTAssertTrue(active.waitForExistence(timeout: 10))
        XCTAssertEqual(app.sheets.count, 0, "Scheduled jobs opens directly from the inbox as a navigation page")
        XCTAssertTrue(paused.exists, "Identical job IDs from different agents remain separate")
        XCTAssertTrue(app.staticTexts["Tap a job to edit, pause, or cancel it. Create new jobs by asking an agent in chat."].exists)
        XCTAssertFalse(app.buttons["New schedule"].exists)
        capture(app, "scheduled-jobs-account-list")
        active.tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label ENDSWITH %@", "Europe/Athens")).firstMatch.waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label ENDSWITH %@", "0 9 * * *")).firstMatch.exists)
        XCTAssertTrue(app.buttons["scheduled-job-latest-run"].exists)
        XCTAssertTrue(app.buttons["scheduled-job-edit"].exists)
        XCTAssertFalse(app.buttons["scheduled-job-edit"].isEnabled, "Demo jobs must not mutate an account")
        capture(app, "scheduled-job-detail")
        app.buttons["scheduled-job-source-chat"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertEqual(composer(app).value as? String, "Keep my draft while I check jobs")

        XCTAssertEqual(self.selectedConversationTab(app).label, title)
        navigationAction(app, "inbox-scheduled-jobs").coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        paused.tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label ENDSWITH %@", "Continue source chat")).firstMatch.waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Last skipped"].exists)
        capture(app, "scheduled-job-paused")
        app.navigationBars.buttons["Scheduled jobs"].tap()
        active.tap(); app.buttons["scheduled-job-latest-run"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))

        XCTAssertEqual(self.selectedConversationTab(app).label, "Build the agent inbox")
    }

    func testLiveScheduledJobsLoadAfterRelaunch() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_SCHEDULES_UI_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in simulator or device for real schedule reads.")
        }
        let app = XCUIApplication()
        for pass in 1...2 {
            app.launch()
            XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 20))
            navigationAction(app, "inbox-scheduled-jobs").coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
            XCTAssertTrue(app.navigationBars["Scheduled jobs"].waitForExistence(timeout: 10))
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
                let detail = app.descendants(matching: .any)["scheduled-job-detail"].firstMatch
                XCTAssertTrue(detail.waitForExistence(timeout: 5))
                let source = app.buttons["scheduled-job-source-chat"]
                for _ in 0..<5 {
                    if source.exists && source.isHittable { break }
                    detail.swipeUp()
                }
                XCTAssertTrue(source.exists && source.isHittable)
            }
            capture(app, "scheduled-jobs-live-\(pass)")
            app.terminate()
        }
    }
    func testAppStoreOpensNativeAppsAndReturnsToChat() {
        let app = launch(arguments: ["--generated-apps-ui-fixture"])
        let conversation = selectedConversationTab(app).identifier
        let store = app.buttons["main-tab-apps"]
        XCTAssertTrue(store.waitForExistence(timeout: 25))
        XCTAssertFalse(app.scrollViews["saved-apps-bar"].exists)
        XCTAssertFalse(app.buttons["main-app-water"].exists)
        capture(app, "app-store-compact-dock")

        func openApp(_ id: String) {
            store.tap()
            let item = app.buttons["app-store-app-" + id]
            XCTAssertTrue(item.waitForExistence(timeout: 5))
            item.tap()
        }
        openApp("water")
        XCTAssertTrue(app.staticTexts["Glasses: 0"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.webViews.firstMatch.exists)
        app.buttons["Add glass"].tap()
        XCTAssertTrue(app.staticTexts["Glasses: 1"].waitForExistence(timeout: 5))
        capture(app, "app-store-water-native")

        openApp("reading")
        XCTAssertTrue(app.staticTexts["Pages: 0"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["Glasses: 1"].exists)
        app.buttons["Read page"].tap()
        XCTAssertTrue(app.staticTexts["Pages: 1"].waitForExistence(timeout: 5))

        openApp("travel")
        XCTAssertTrue(app.staticTexts["Packed items: 0"].waitForExistence(timeout: 5))
        capture(app, "app-store-travel")

        app.buttons["main-tab-chat"].tap()
        XCTAssertTrue(selectedConversationTab(app).waitForExistence(timeout: 5))
        XCTAssertEqual(selectedConversationTab(app).identifier, conversation)
        XCTAssertFalse(app.staticTexts["Packed items: 0"].exists)
        capture(app, "app-store-return-to-chat")

        openApp("water")
        XCTAssertTrue(app.staticTexts["Glasses: 1"].waitForExistence(timeout: 5), "Switching apps must reopen their saved native state.")
        capture(app, "app-store-water-reopened")
        store.tap()
        app.buttons["Your apps"].tap()
        XCTAssertTrue(app.buttons["create-generated-app"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Synthetic water tracker"].exists)
    }

    func testOneModelButtonOpensAllConversationSettings() {
        let app = launch()
        let picker = app.buttons["model-picker"]
        XCTAssertTrue(picker.isHittable)
        XCTAssertFalse(app.buttons["effort-dial"].exists)
        XCTAssertFalse(app.buttons["auto-route"].exists)
        picker.tap()
        XCTAssertTrue(app.buttons["Astra"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["Sol"].exists)
        XCTAssertTrue(app.buttons["Automatic routing"].exists)
        let thinking = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "Thinking:")).firstMatch
        XCTAssertTrue(thinking.exists)
        capture(app, "single-model-menu")
    }

    private func launch(_ environment: [String: String] = [:], arguments: [String] = []) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["--demo"] + arguments
        app.launchEnvironment = ["NANOCODEX_DEMO_PROFILE": UUID().uuidString, "NANOCODEX_DEMO_COMPLETE_AFTER_MS": "120000", "NANOCODEX_DEMO_DELAY_MS": "600"].merging(environment) { _, new in new }
        app.launch()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 10))
        if environment["NANOCODEX_DEMO_EMPTY_AGENTS"] != "1" {
            XCTAssertTrue(self.selectedConversationTab(app).waitForExistence(timeout: 10))
        }
        return app
    }
    func testLiveAccountCreatesConversationAndCompletesTask() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else {
            throw XCTSkip("Opt-in journey requires a signed-in physical device.")
        }
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 20), "The phone must already be signed in.")
        capture(app, "live-01-connected-account")
        let create = navigationAction(app, "New agent")
        create.tap()
        // The empty conversation is usable before server creation completes.
        XCTAssertTrue(self.composer(app).isEnabled)
        XCTAssertEqual(self.selectedConversationTab(app).label, "New agent")
        XCTAssertTrue(app.otherElements["conversation-empty"].waitForExistence(timeout: 5))
        XCTAssertTrue(composer(app).waitForExistence(timeout: 20))
        XCTAssertTrue(self.selectedConversationTab(app).waitForExistence(timeout: 10))
        capture(app, "live-01b-created-conversation")
        let input = "Phone check " + String(UUID().uuidString.prefix(8)) + ". Use a terminal command to calculate 17 * 23. Do not change files. Reply with exactly: Phone check: 391"
        queue(app, input)
        XCTAssertTrue(latestUserText(app).waitForExistence(timeout: 10))
        XCTAssertEqual(latestUserText(app).label, input)
        capture(app, "live-02-task-sent")
        let response = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: self.assistantText(app, matching: NSPredicate(format: "label CONTAINS %@", "Phone check: 391")))
        XCTAssertEqual(XCTWaiter.wait(for: [response], timeout: 90), .completed)
        capture(app, "live-03-task-completed")

        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))
        let followUp = "Reply with exactly: Follow-up check: 392"
        queue(app, followUp)
        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.staticTexts["Follow-up check: 392"].waitForExistence(timeout: 60))
        capture(app, "live-04-conversation-follow-up")

        let title = self.selectedConversationTab(app).label
        app.terminate(); app.launch()
        selectAgentFromDrawer(app, title: title)

        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.staticTexts["Follow-up check: 392"].waitForExistence(timeout: 15))
        capture(app, "live-05-durable-history-after-relaunch")

        checkLiveVoice(app)
    }
    func testLiveHandConnectsAutomaticallyAndRunsFiles() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else { throw XCTSkip("Requires a signed-in device or simulator.") }
        let app = XCUIApplication(); app.launch()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 20))
        navigationAction(app, "Account settings").tap()
        XCTAssertTrue(app.staticTexts["Hand connected"].waitForExistence(timeout: 30), "This device must become a Hand without enabling it.")
        capture(app, "automatic-hand-connected")
        app.buttons["Done"].tap()
        let create = navigationAction(app, "New agent"); create.tap()
        let created = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.composer(app).isEnabled && self.selectedConversationTab(app).exists && self.selectedConversationTab(app).label == "New agent"
                && app.otherElements["conversation-empty"].exists
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [created], timeout: 20), .completed)
        let file = "automatic-hand-" + UUID().uuidString.lowercased() + ".txt"
        queue(app, "Use this iPhone's already-connected Hand named iPhone. Discover its native write_file and read_file tools. Write exactly IPHONE_HAND_WORKS to \(file), then read it back. Use the iPhone Hand, not /brain or a Mac. Reply with the contents. Do not ask about setup.")
        let response = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: self.assistantText(app, matching: NSPredicate(format: "label CONTAINS %@", "IPHONE_HAND_WORKS")))
        XCTAssertEqual(XCTWaiter.wait(for: [response], timeout: 120), .completed)
        capture(app, "automatic-hand-real-file-roundtrip")
        let title = self.selectedConversationTab(app).label
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 20))
        navigationAction(app, "Account settings").tap()
        XCTAssertTrue(app.staticTexts["Hand connected"].waitForExistence(timeout: 30))
        app.buttons["Done"].tap()
        selectAgentFromDrawer(app, title: title)
        queue(app, "Read \(file) from the same iPhone Hand again. Reply with its contents and COLD_LAUNCH_HAND_OK.")
        let restored = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: self.assistantText(app, matching: NSPredicate(format: "label CONTAINS %@", "COLD_LAUNCH_HAND_OK")))
        XCTAssertEqual(XCTWaiter.wait(for: [restored], timeout: 120), .completed)
        capture(app, "automatic-hand-restored-file-after-cold-launch")
    }
    func testLiveHandDisableSurvivesRelaunchAndBackground() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else { throw XCTSkip("Requires a signed-in device or simulator.") }
        let app = XCUIApplication(); app.launch()
        func settings() {
            XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 30))
            navigationAction(app, "Account settings").tap()
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
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 30))
        navigationAction(app, "Account settings").tap()
        guard app.switches["device-hand-enabled"].value as? String == "1" else { throw XCTSkip("This device's Hand was explicitly disabled.") }
        XCTAssertTrue(app.staticTexts["Hand connected"].waitForExistence(timeout: 30))
        app.buttons["Done"].tap()
        let create = navigationAction(app, "New agent"); create.tap()
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.composer(app).isEnabled && self.selectedConversationTab(app).label == "New agent"
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 20), .completed)
        let marker = "HAND_BG_" + UUID().uuidString.replacingOccurrences(of: "-", with: "")
        let file = marker.lowercased() + ".txt"
        print("Background Hand proof file: " + file)
        queue(app, "Background Hand check \(marker). Use accountInfo and tool_search to find this phone's connected native iPhone write_file and read_file tools. Use that iPhone Hand, not a Mac or /brain. Perform 8 sequential rounds: wait 6 seconds using bash sleep, write ROUND_n to \(file) on the iPhone, then read it back. Do not parallelize or skip rounds. After round 8 write exactly \(marker) to the same iPhone file, read it, and reply with only its contents.")
        navigationAction(app, "Account settings").tap()
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
        app.buttons["Done"].tap()
        let completed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: self.assistantText(app, matching: NSPredicate(format: "label CONTAINS %@", marker)))
        XCTAssertEqual(XCTWaiter.wait(for: [completed], timeout: 90), .completed)
        capture(app, "hand-task-background-file-result")
    }
    func testLiveRunAgentShortcutOffersAccountAgents() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_HAND_SHORTCUT_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in phone and Shortcuts.")
        }
        let app = XCUIApplication(); app.launch()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 30))
        let title = self.selectedConversationTab(app).label
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
    func testLiveVideoAttachmentReopensHistory() throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["NANOCODEX_VIDEO_UI_LIVE"] == "1",
              let title = environment["NANOCODEX_VIDEO_AGENT_TITLE"], !title.isEmpty else {
            throw XCTSkip("Requires the existing real video-attachment validation conversation.")
        }
        let app = XCUIApplication(); app.launch()
        selectAgentFromDrawer(app, title: title)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        let play = conversation.buttons["play-original-video"].firstMatch
        for _ in 0..<6 {
            if play.exists && play.isHittable { break }
            conversation.swipeDown()
        }
        XCTAssertTrue(play.waitForExistence(timeout: 10))
        capture(app, "video-history-before-playback")
        play.tap()
        XCTAssertTrue(app.descendants(matching: .any)["video-player"].waitForExistence(timeout: 30))
        capture(app, "video-history-original-playback")
        app.navigationBars["VideoAudioCheck.mp4"].buttons["Done"].tap()
        conversation.swipeUp()
        capture(app, "video-history-latest-response")
    }

    func testLiveVideoAttachmentDraftSendAndHistory() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_VIDEO_UI_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in phone with VideoAudioCheck.mp4 copied into the app Documents folder.")
        }
        guard let title = ProcessInfo.processInfo.environment["NANOCODEX_VIDEO_AGENT_TITLE"], !title.isEmpty else {
            throw XCTSkip("Requires a dedicated, already-created video test conversation with a READY reply.")
        }
        let app = XCUIApplication(); app.launch()
        selectAgentFromDrawer(app, title: title)
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: self.assistantText(app, matching: NSPredicate(format: "label CONTAINS %@", "READY")))
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
                let folder = app.cells.containing(NSPredicate(format: "label == %@", "Nanocodex")).firstMatch
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
        app.terminate(); app.launch(); selectAgentFromDrawer(app, title: title)
        XCTAssertTrue(app.buttons[id].waitForExistence(timeout: 10), "Keep the original clip with the saved draft.")
        XCTAssertEqual(self.selectedConversationTab(app).label, title)
        capture(app, "video-03-restored-draft")
        queue(app, "Use tools to compute the SHA-256 of the attached original video at its /brain path. Reply ORIGINAL_FILE_OK and the computed digest. Do not infer bytes from the filename or metadata.")
        let answer = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            let text = app.descendants(matching: .any)["conversation"].firstMatch.staticTexts.allElementsBoundByIndex.map(\.label).joined(separator: "\n").lowercased()
            return self.selectedConversationTab(app).label == title && text.contains("original_file_ok") && text.contains("5e9ac6c51375c596547b61d338fd72623e03f2b861a4c45b62f08bc1ef253ca1")
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [answer], timeout: 120), .completed)
        XCTAssertEqual(app.keyboards.count, 0)
        capture(app, "video-04-original-file-digest")
        app.terminate(); app.launch(); selectAgentFromDrawer(app, title: title)

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
            selectAgentFromDrawer(app, title: title)
        } else {
            let create = navigationAction(app, "New agent")
            XCTAssertTrue(create.waitForExistence(timeout: 20), "The phone must already be signed in.")
            create.tap()
            let created = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
                self.composer(app).isEnabled && self.selectedConversationTab(app).exists
                    && self.selectedConversationTab(app).label == "New agent"
                    && app.descendants(matching: .any)["conversation"].firstMatch.exists
                    && app.otherElements["conversation-empty"].exists
            }, object: app)
            XCTAssertEqual(XCTWaiter.wait(for: [created], timeout: 20), .completed)
            queue(app, title)
        }
        let seeded = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.selectedConversationTab(app).label == title
                && self.assistantText(app, matching: NSPredicate(format: "label ==[c] %@", "READY")).exists
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [seeded], timeout: 90), .completed)
        print("Live attachment UI conversation title: \(title)")

        let cancelledDraft = "Keep my draft while choosing an attachment"
        composer(app).tap(); composer(app).typeText(cancelledDraft)
        for identifier in ["choose-photos", "choose-files", "choose-videos"] {
            app.buttons["add-attachments"].tap()
            XCTAssertTrue(app.buttons[identifier].waitForExistence(timeout: 5))
            app.buttons[identifier].tap()
            let cancel = app.buttons["Cancel"].firstMatch
            XCTAssertTrue(cancel.waitForExistence(timeout: 10), "Open the native picker: " + identifier)
            capture(app, "live-attachment-01-picker-" + identifier)
            cancel.tap()
            gone(cancel)
            XCTAssertEqual(composer(app).value as? String, cancelledDraft)
            XCTAssertFalse(app.scrollViews["composer-attachments"].exists)
            XCTAssertFalse(app.staticTexts["attachment-error"].exists, "Cancelling must not report an import failure")
        }
        composer(app).tap()
        composer(app).typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: cancelledDraft.count))

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
                    let folder = app.cells.containing(.staticText, identifier: "Nanocodex").firstMatch
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
        selectAgentFromDrawer(app, title: title)
        waitForAttachment()
        XCTAssertEqual(removals.firstMatch.identifier, attachmentID, "Restore the original image reference after relaunch.")
        XCTAssertEqual(composer(app).value as? String, prompt)
        capture(app, "live-attachment-04-restored-draft")

        app.buttons["send"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["message-image"].waitForExistence(timeout: 10), "Show the submitted image immediately.")
        capture(app, "live-attachment-05-image-sent")
        let answer = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            let text = self.assistantContent(app).lowercased()
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
        selectAgentFromDrawer(app, title: title)

        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
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
        let create = navigationAction(app, "New agent")
        XCTAssertTrue(create.waitForExistence(timeout: 20)); create.tap()
        let created = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.composer(app).isEnabled && self.selectedConversationTab(app).label == "New agent"
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [created], timeout: 20), .completed)
        let title = "Stop check " + String(UUID().uuidString.prefix(8)) + ". Reply READY"
        queue(app, title)
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.selectedConversationTab(app).label == title && self.assistantText(app, matching: NSPredicate(format: "label ==[c] %@", "READY")).exists
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
            app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@ AND value == %@", "conversation-title:", "Stopped")).firstMatch.exists && action.label == "Send message" && !action.isEnabled
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
        let create = navigationAction(app, "New agent")
        XCTAssertTrue(create.waitForExistence(timeout: 20))
        create.tap()
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.composer(app).isEnabled && self.selectedConversationTab(app).label == "New agent"
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 20), .completed)
        let title = "Camera check " + String(UUID().uuidString.prefix(8)) + ". Reply READY"
        queue(app, title)
        let named = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.selectedConversationTab(app).label == title && self.assistantText(app, matching: NSPredicate(format: "label ==[c] %@", "READY")).exists
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [named], timeout: 90), .completed)
        print("Live camera UI conversation title: \(title)")
        app.buttons["add-attachments"].tap()
        XCTAssertTrue(app.buttons["choose-camera"].waitForExistence(timeout: 5))
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
        selectAgentFromDrawer(app, title: title)

        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
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
        app.launch()
        XCTAssertTrue(selectedConversationTab(app).waitForExistence(timeout: 30))
        let originalID = selectedConversationTab(app).identifier
        let configuredID = ProcessInfo.processInfo.environment["NANOCODEX_LIVE_AGENT_IDS"]?.split(separator: ",").first.map(String.init)
        if let configuredID {
            selectAgentFromDrawer(app, title: "", id: configuredID)
        } else {
            app.buttons["conversation-drawer-open"].tap()
            let originalRow = originalID.replacingOccurrences(of: "conversation-title:", with: "conversation-row:")
            let alternate = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@ AND identifier != %@", "conversation-row:", originalRow)).allElementsBoundByIndex.first
            if let alternate { alternate.tap() }
            else { app.buttons["conversation-drawer-close"].tap() }
        }
        let expectedTabID = selectedConversationTab(app).identifier
        // Backgrounding drains the ordered preferences writer before termination.
        XCUIDevice.shared.press(.home)
        for index in 1...3 {
            app.terminate()
            app.launch()
            XCTAssertFalse(app.descendants(matching: .any)["phone-onboarding"].exists, "A saved account must not show the sign-in form while restoring")
            XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 20), "Cold launch must restore the saved account")
            XCTAssertTrue(selectedConversationTab(app).waitForExistence(timeout: 10))
            XCTAssertEqual(selectedConversationTab(app).identifier, expectedTabID, "Reopen the last selected browser tab")
            gone(app.descendants(matching: .any)["conversation-loading"].firstMatch, timeout: 30)
            XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.exists)
            XCTAssertFalse(app.staticTexts["Loading conversation…"].exists)
            XCTAssertFalse(app.staticTexts["Opening your inbox…"].exists)
            XCTAssertFalse(app.textFields["phone-number"].exists)
            XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "identifier == %@ AND label == %@", "connection", "Demo")).firstMatch.exists)
            capture(app, "live-cold-launch-\(index)")
        }
        if originalID != expectedTabID, configuredID == nil {
            selectAgentFromDrawer(app, title: "", id: String(originalID.dropFirst("conversation-title:".count)))
        }
    }
    func testLiveVoiceConnectsMinimizesAndStops() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_VOICE_UI_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in device for native voice service evidence.")
        }
        let app = XCUIApplication()
        app.launchEnvironment["NANOCODEX_VOICE_TIMING"] = "1"
        app.launch()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 20))
        defer {
            if app.buttons["end-voice"].exists { app.buttons["end-voice"].tap() }
            else if app.buttons["end-voice-compact"].exists { app.buttons["end-voice-compact"].tap() }
        }
        for pass in 1...2 {
            XCTAssertTrue(app.buttons["start-voice"].waitForExistence(timeout: 10))
            let began = ProcessInfo.processInfo.systemUptime
            app.buttons["start-voice"].tap()
            if pass == 1 {
                let permission = XCUIApplication(bundleIdentifier: "com.apple.springboard").alerts.firstMatch
                if permission.waitForExistence(timeout: 2), permission.label.localizedCaseInsensitiveContains("microphone") {
                    if permission.buttons["Allow"].exists { permission.buttons["Allow"].tap() }
                    else if permission.buttons["OK"].exists { permission.buttons["OK"].tap() }
                }
            }
            guard app.buttons["mute-voice"].waitForExistence(timeout: 10) else {
                capture(app, "voice-live-admission-failed-\(pass)")
                let message = app.staticTexts["voice-error"].exists ? app.staticTexts["voice-error"].label : "Voice controls did not appear"
                if app.buttons["end-voice"].exists { app.buttons["end-voice"].tap() }
                XCTFail(message)
                return
            }
            // Exercise microphone activation on the first call. The second
            // retains the distinct muted-during-connection readiness path.
            if pass == 2, app.buttons["mute-voice"].label == "Mute microphone" { app.buttons["mute-voice"].tap() }
            let readyStatuses = pass == 1 ? ["Listening", "Speaking", "Working on it"] : ["Microphone muted"]
            let connected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label IN %@", readyStatuses + ["Voice paused"]), object: app.staticTexts["voice-status"])
            let result = XCTWaiter.wait(for: [connected], timeout: 55)
            guard result == .completed, readyStatuses.contains(app.staticTexts["voice-status"].label) else {
                capture(app, "voice-live-start-failed-\(pass)")
                let message = app.staticTexts["voice-error"].exists ? app.staticTexts["voice-error"].label : "Voice did not become active; status: \(app.staticTexts["voice-status"].label)"
                if app.buttons["end-voice"].exists { app.buttons["end-voice"].tap() }
                XCTFail(message)
                return
            }
            print("VOICE_UI_READY pass=\(pass) started_muted=\(pass == 2) elapsed_ms=\(Int((ProcessInfo.processInfo.systemUptime - began) * 1_000))")
            XCTAssertTrue(app.descendants(matching: .any).matching(identifier: "voice-orb").firstMatch.waitForExistence(timeout: 3))
            if pass == 1 {
                XCTAssertEqual(app.buttons["mute-voice"].label, "Mute microphone")
                capture(app, "voice-live-connected-unmuted")
                app.buttons["mute-voice"].tap()
                XCTAssertEqual(app.buttons["mute-voice"].label, "Unmute microphone")
            }
            // Also require audio on the call that was muted during startup:
            // ambient microphone input must not be needed to trigger speech.
            app.buttons["voice-settings"].tap()
            let testAudio = app.buttons["test-voice-audio"]
            XCTAssertTrue(testAudio.waitForExistence(timeout: 5))
            if !testAudio.isHittable { app.swipeUp() }
            testAudio.tap()
            let audio = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", "Audio received"), object: app.staticTexts["voice-audio-result"])
            let audioResult = XCTWaiter.wait(for: [audio], timeout: 20)
            capture(app, "voice-live-spoken-audio-\(pass)")
            app.buttons["Cancel"].tap()
            if audioResult != .completed {
                app.buttons["end-voice"].tap()
                XCTFail("Connected voice must deliver audible media for an explicit test phrase on call \(pass)")
                return
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
    private func navigationAction(_ app: XCUIApplication, _ label: String) -> XCUIElement {
        if label == "New agent" { return app.buttons["new-conversation"] }
        app.descendants(matching: .any).matching(identifier: "app-menu").firstMatch.tap()
        let action = app.buttons[label]
        XCTAssertTrue(action.waitForExistence(timeout: 5))
        return action
    }
    private func selectAgentFromDrawer(_ app: XCUIApplication, title: String, id: String? = nil) {
        let overview = app.descendants(matching: .any)["conversation-list"].firstMatch
        if !overview.exists { app.buttons["conversation-drawer-open"].tap() }
        XCTAssertTrue(overview.waitForExistence(timeout: 10))
        let search = app.textFields["conversation-search"]
        XCTAssertTrue(search.waitForExistence(timeout: 5))
        search.tap()
        if !(search.value as? String ?? "").isEmpty, app.buttons["Clear search"].exists { app.buttons["Clear search"].tap(); search.tap() }
        search.typeText(id ?? title)
        let card = id.map { app.buttons["conversation-row:" + $0] }
            ?? app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@ AND label == %@", "conversation-row:", title)).firstMatch
        XCTAssertTrue(card.waitForExistence(timeout: 10))
        XCTAssertTrue(card.isHittable)
        card.tap(); gone(overview)
        let selected = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            if let id { return app.buttons["conversation-title:" + id].isSelected }
            return self.selectedConversationTab(app).label == title
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [selected], timeout: 10), .completed)
    }
    private func assistantText(_ app: XCUIApplication, matching predicate: NSPredicate) -> XCUIElement {
        app.descendants(matching: .any)["conversation"].firstMatch.otherElements.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message-assistant-"))
            .staticTexts.matching(predicate).firstMatch
    }
    private func assistantContent(_ app: XCUIApplication) -> String {
        app.descendants(matching: .any)["conversation"].firstMatch.otherElements.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message-assistant-"))
            .staticTexts.allElementsBoundByIndex.map(\.label).joined(separator: "\n")
    }
    private func latestUserText(_ app: XCUIApplication) -> XCUIElement {
        let messages = app.descendants(matching: .any)["conversation"].firstMatch.otherElements.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message-user-"))
        return messages.element(boundBy: max(0, messages.count - 1)).staticTexts.firstMatch
    }
    private func switchConversation(_ app: XCUIApplication, id: String) {
        let selected = app.buttons["conversation-title:" + id]
        if selected.exists && selected.isSelected { return }
        app.buttons["conversation-drawer-open"].tap()
        let search = app.textFields["conversation-search"]
        XCTAssertTrue(search.waitForExistence(timeout: 5))
        search.tap(); search.typeText(id)
        let row = app.buttons["conversation-row:" + id]
        for _ in 0..<20 {
            if row.exists { break }
            let older = app.buttons["drawer-load-older"]
            guard older.exists else { break }
            older.tap()
        }
        XCTAssertTrue(row.waitForExistence(timeout: 5))
        row.tap()
        XCTAssertTrue(selected.waitForExistence(timeout: 5))
    }
    private func selectTab(_ app: XCUIApplication, id: String, title: String) {
        switchConversation(app, id: id)
        let selected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", title), object: self.selectedConversationTab(app))
        XCTAssertEqual(XCTWaiter.wait(for: [selected], timeout: 5), .completed)
    }
    private func selectInbox(_ app: XCUIApplication) {
        selectTab(app, id: "inbox", title: "Build the agent inbox")
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

        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        // A queued message has one representation in the queue, not a sent bubble.
        let queued = app.scrollViews["pending-messages"].staticTexts.matching(NSPredicate(format: "label == %@", text))
        if queued.count == 1 {
            XCTAssertFalse(conversation.staticTexts[text].exists)
            return
        }
        let message = conversation.staticTexts[text]
        if !message.isHittable { conversation.swipeUp() }
        XCTAssertTrue(message.waitForExistence(timeout: 5))
        XCTAssertEqual(conversation.staticTexts.matching(NSPredicate(format: "label == %@", text)).count, 1)
    }

    func testSendSteersActiveTurnWithoutSeparateAction() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString,
                          "NANOCODEX_DEMO_COMPLETE_AFTER_MS": "60000"])
        selectInbox(app)
        let text = "Use the synthetic workstation snapshot"
        queue(app, text)
        XCTAssertFalse(app.buttons["steer-now"].exists)
        thread(app, contains: text)
        XCTAssertTrue(app.buttons["withdraw-steering"].waitForExistence(timeout: 8))
        XCTAssertEqual(app.buttons["send"].label, "Stop turn")
        XCTAssertEqual(app.buttons["conversation-title:inbox"].value as? String, "Running")
        XCTAssertFalse(app.staticTexts["Working on: " + text].exists,
                       "Sending to an active turn must not launch another turn")
        app.terminate(); app.launch(); selectInbox(app)
        thread(app, contains: text)
        XCTAssertTrue(app.buttons["withdraw-steering"].waitForExistence(timeout: 5))
        app.buttons["withdraw-steering"].tap()
        XCTAssertTrue(app.staticTexts["Steering withdrawn: " + text].waitForExistence(timeout: 5))
        XCTAssertEqual(app.buttons["send"].label, "Stop turn")
        capture(app, "default-send-steers-current-turn")
    }

    func testSteeringResponseLossSurvivesRelaunchWithoutResubmission() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString, "NANOCODEX_DEMO_FAIL_ONCE": "steer"])
        selectInbox(app); queue(app, "Retain uncertain steering")
        XCTAssertFalse(app.buttons["steer-now"].exists)
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS[c] %@", "unconfirmed")).firstMatch.waitForExistence(timeout: 8))
        XCTAssertFalse(app.buttons["retry-steering"].exists)
        app.terminate(); app.launch(); selectInbox(app)
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS[c] %@", "unconfirmed")).firstMatch.waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["steer-now"].exists)
        app.buttons["withdraw-steering"].tap()
        XCTAssertTrue(app.staticTexts["Steering withdrawn: Retain uncertain steering"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.buttons["send"].label, "Stop turn")
    }
    func testConsumedSteeringCannotBePresentedAsWithdrawn() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString, "NANOCODEX_DEMO_STEER_CONSUMED": "1"])
        selectInbox(app); queue(app, "Already consumed steering")
        XCTAssertFalse(app.buttons["steer-now"].exists)
        XCTAssertTrue(app.buttons["withdraw-steering"].waitForExistence(timeout: 5))
        app.buttons["withdraw-steering"].tap()
        XCTAssertTrue(app.staticTexts["Steering could not be withdrawn; it may already be in use."].waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["Steering withdrawn: Already consumed steering"].exists)
        XCTAssertEqual(app.buttons["send"].label, "Stop turn")
    }
    func testTabsPreserveIndependentDraftsAndQueuedSteering() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        selectAgentFromDrawer(app, title: "Make long sessions bulletproof")
        composer(app).tap(); composer(app).typeText("Check the reconnect boundary")
        selectTab(app, id: "inbox", title: "Build the agent inbox")
        XCTAssertNotEqual(composer(app).value as? String, "Check the reconnect boundary")
        composer(app).tap(); composer(app).typeText("Prioritize reconnect and keep the UI minimal")
        selectTab(app, id: "durability", title: "Make long sessions bulletproof")
        XCTAssertEqual(composer(app).value as? String, "Check the reconnect boundary")
        selectTab(app, id: "inbox", title: "Build the agent inbox")
        XCTAssertEqual(composer(app).value as? String, "Prioritize reconnect and keep the UI minimal")
        capture(app, "tabs-independent-drafts")
        composer(app).tap()
        XCTAssertLessThanOrEqual(app.buttons["send"].frame.maxY, app.keyboards.firstMatch.frame.minY)
        app.buttons["send"].tap()
        XCTAssertFalse(app.buttons["steer-now"].exists)
        thread(app, contains: "Prioritize reconnect and keep the UI minimal")
        selectTab(app, id: "durability", title: "Make long sessions bulletproof")
        XCTAssertFalse(app.staticTexts["pending-message"].exists)
        XCTAssertEqual(composer(app).value as? String, "Check the reconnect boundary")
        selectTab(app, id: "inbox", title: "Build the agent inbox")
        thread(app, contains: "Prioritize reconnect and keep the UI minimal")

        gone(app.staticTexts["pending-message"])
        thread(app, contains: "Prioritize reconnect and keep the UI minimal")

        capture(app, "tabs-queued-steering")
    }

    func testDrawerShowsRunningStatusAndSelectsAgent() {
        let app = launch(["NANOCODEX_DEMO_COMPLETE_AFTER_MS": "12000", "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        selectTab(app, id: "inbox", title: "Build the agent inbox")
        queue(app, "Update the overview while I browse")
        XCTAssertFalse(app.buttons["steer-now"].exists)
        selectTab(app, id: "durability", title: "Make long sessions bulletproof")
        composer(app).tap(); composer(app).typeText("Keep this draft behind the overview")
        app.buttons["conversation-drawer-open"].tap()
        let overview = app.descendants(matching: .any)["conversation-list"]
        XCTAssertTrue(overview.waitForExistence(timeout: 5))
        let preview = app.buttons["conversation-row:inbox"]
        XCTAssertTrue(preview.waitForExistence(timeout: 5))
        XCTAssertTrue((preview.value as? String ?? "").contains("Running"))
        XCTAssertTrue((app.buttons["conversation-row:hands"].value as? String ?? "").contains("Failed"))
        capture(app, "overview-live-content-and-status")
        app.buttons["conversation-row:inbox"].tap()
        gone(overview)
        XCTAssertEqual(self.selectedConversationTab(app).label, "Build the agent inbox")
        selectTab(app, id: "durability", title: "Make long sessions bulletproof")
        XCTAssertEqual(composer(app).value as? String, "Keep this draft behind the overview")
    }

    func testFailedSubmissionRetainsMessageAndRetriesOnce() {
        let app = launch(["NANOCODEX_DEMO_FAIL_ONCE": "submit", "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        navigationAction(app, "New agent").tap()
        queue(app, "Retry only once")
        XCTAssertTrue(app.buttons["retry-pending"].waitForExistence(timeout: 5))
        app.buttons["retry-pending"].doubleTap()
        gone(app.buttons["retry-pending"])
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
        XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: NSPredicate(format: "value == %@", "Stopped"), object: app.buttons["conversation-title:inbox"])], timeout: 4), .completed)
    }
    func testFirstMessageCanBeStoppedBeforeAdmission() {
        let app = launch(["NANOCODEX_DEMO_DELAY_MS": "8000", "NANOCODEX_DEMO_CANCEL_DELAY_MS": "100"])
        navigationAction(app, "New agent").tap()
        XCTAssertEqual(self.selectedConversationTab(app).label, "New agent")
        queue(app, "Cancel the first submission")
        XCTAssertEqual(app.buttons["send"].label, "Stop turn")
        XCTAssertTrue(app.buttons["send"].isEnabled)
        app.buttons["send"].tap()
        XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: NSPredicate(format: "value == %@", "Stopped"), object: app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@ AND selected == true", "conversation-title:")).firstMatch)], timeout: 4), .completed)
        let resurrected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", "Stop turn"), object: app.buttons["send"])
        resurrected.isInverted = true
        XCTAssertEqual(XCTWaiter.wait(for: [resurrected], timeout: 9), .completed)
        capture(app, "first-send-stopped-before-admission")
    }
    func testStopDuringIdleSubmissionCannotReturnOnLateAcknowledgement() {
        let app = launch(["NANOCODEX_DEMO_DELAY_MS": "8000", "NANOCODEX_DEMO_CANCEL_DELAY_MS": "100"])
        navigationAction(app, "New agent").tap()
        queue(app, "Cancel before acknowledgement")
        let stop = app.buttons["send"]
        XCTAssertEqual(stop.label, "Stop turn")
        XCTAssertTrue(stop.isEnabled, "Stopping must bypass the outstanding submission")
        stop.tap()
        let stopped = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", "Send message"), object: stop)
        XCTAssertEqual(XCTWaiter.wait(for: [stopped], timeout: 4), .completed)
        let resurrected = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", "Stop turn"), object: stop)
        resurrected.isInverted = true
        XCTAssertEqual(XCTWaiter.wait(for: [resurrected], timeout: 9), .completed)
        capture(app, "stop-during-send-no-resurrection")
    }
    func testStopIntentSurvivesColdLaunch() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString, "NANOCODEX_DEMO_CANCEL_DELAY_MS": "5000"])
        selectInbox(app)
        app.buttons["send"].tap()
        XCTAssertEqual(app.buttons["send"].label, "Stopping turn")
        app.terminate(); app.launch(); selectInbox(app)
        XCTAssertEqual(app.buttons["send"].label, "Stopping turn")
        XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: NSPredicate(format: "value == %@", "Stopped"), object: app.buttons["conversation-title:inbox"])], timeout: 10), .completed)
        XCTAssertEqual(app.buttons["send"].label, "Send message")
        capture(app, "stop-restored-and-confirmed")
    }
    func testDirectMessageCanBeWithdrawnAfterDelivery() {
        let app = launch(["NANOCODEX_DEMO_STEER_DELAY_MS": "1200"])
        selectInbox(app); queue(app, "Withdraw this correction")
        XCTAssertTrue(app.buttons["withdraw-steering"].waitForExistence(timeout: 5))
        app.buttons["withdraw-steering"].tap()
        XCTAssertTrue(app.staticTexts["Steering withdrawn: Withdraw this correction"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.buttons["send"].label, "Stop turn")
    }

    func testLiveCancelQueuedMessageThenSteerItsSuccessor() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else { throw XCTSkip("Requires a signed-in physical phone.") }
        let app = XCUIApplication(); app.launch()
        let create = navigationAction(app, "New agent")
        XCTAssertTrue(create.waitForExistence(timeout: 30)); create.tap()
        let created = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.composer(app).isEnabled && self.selectedConversationTab(app).label == "New agent"
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [created], timeout: 20), .completed)
        let marker = String(UUID().uuidString.prefix(8))
        let title = "Steer check \(marker). Reply READY"
        print("Live steering UI conversation title: " + title)
        queue(app, title)
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: self.assistantText(app, matching: NSPredicate(format: "label == %@", "READY")))
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 90), .completed)
        queue(app, "Keep running terminal commands `sleep 10` until 90 seconds have elapsed. Do not finish early or change files. I will interrupt this from the app.")
        let running = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.buttons["send"].label == "Stop turn" && app.buttons["send"].isEnabled
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [running], timeout: 30), .completed)
        queue(app, "Reply only STEERED_\(marker)")
        XCTAssertFalse(app.buttons["steer-now"].exists)
        let reply = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: self.assistantText(app, matching: NSPredicate(format: "label == %@", "STEERED_" + marker)))
        XCTAssertEqual(XCTWaiter.wait(for: [reply], timeout: 90), .completed)
        gone(app.staticTexts["pending-message"])
        capture(app, "live-steer-after-queued-cancellation")
        app.terminate(); app.launch()
        selectAgentFromDrawer(app, title: title)
        thread(app, contains: "Reply only STEERED_" + marker)
        XCTAssertTrue(app.staticTexts["STEERED_" + marker].exists)
    }
    func testFailedWithdrawalCanRetryWithoutDuplicateInput() {
        let app = launch(["NANOCODEX_DEMO_FAIL_ONCE": "withdraw", "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        selectInbox(app); queue(app, "Keep the captured target")
        XCTAssertTrue(app.buttons["withdraw-steering"].waitForExistence(timeout: 5))
        app.buttons["withdraw-steering"].tap()
        XCTAssertTrue(app.staticTexts["Steering could not be confirmed. Retry keeps the same message and target."].waitForExistence(timeout: 5))
        let retry = app.buttons["withdraw-steering"]
        let enabled = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: retry)
        XCTAssertEqual(XCTWaiter.wait(for: [enabled], timeout: 5), .completed)
        retry.tap()
        XCTAssertTrue(app.staticTexts["Steering withdrawn: Keep the captured target"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.descendants(matching: .any)["conversation"].firstMatch.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Keep the captured target")).count, 1)
    }

    func testWithdrawingFirstDirectMessageKeepsSecondDelivered() {
        let app = launch(); selectInbox(app)
        queue(app, "First correction")
        XCTAssertTrue(app.buttons["withdraw-steering"].waitForExistence(timeout: 5))
        queue(app, "Second correction")
        let two = XCTNSPredicateExpectation(predicate: NSPredicate(format: "count == 2"), object: app.buttons.matching(identifier: "withdraw-steering"))
        XCTAssertEqual(XCTWaiter.wait(for: [two], timeout: 5), .completed)
        app.buttons["withdraw-steering"].firstMatch.tap()
        XCTAssertTrue(app.staticTexts["Steering withdrawn: First correction"].waitForExistence(timeout: 5))
        thread(app, contains: "Second correction")
        XCTAssertEqual(app.buttons.matching(identifier: "withdraw-steering").count, 1)
        XCTAssertEqual(app.buttons["send"].label, "Stop turn")
    }

    func testFinishedAgentStaysSelectedUntilAnotherTabIsTapped() {
        let app = launch(["NANOCODEX_DEMO_FINISH_IN_THREAD": "1"])
        selectTab(app, id: "inbox", title: "Build the agent inbox")
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        conversation.swipeUp(); conversation.swipeDown()
        let finished = XCTNSPredicateExpectation(predicate: NSPredicate(format: "value == %@", "Stopped"), object: app.buttons["conversation-title:inbox"])
        XCTAssertEqual(XCTWaiter.wait(for: [finished], timeout: 8), .completed)
        XCTAssertEqual(self.selectedConversationTab(app).label, "Build the agent inbox")
        selectTab(app, id: "data", title: "Tighten the fuel forecast")
        XCTAssertEqual(self.selectedConversationTab(app).label, "Tighten the fuel forecast")
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
        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertEqual(composer(app).value as? String, "Existing draft.")

        XCTAssertFalse(app.staticTexts["pending-message"].exists)
        selectTab(app, id: "durability", title: "Make long sessions bulletproof"); selectInbox(app)
        XCTAssertEqual(composer(app).value as? String, "Existing draft.")
        capture(app, "09-voice-draft-preserved")
    }
    func testVoiceDownArrowReturnsToInboxAndKeepsSessionActive() {
        let app = launch(["NANOCODEX_DEMO_VOICE": "1", "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        let title = self.selectedConversationTab(app).label
        app.buttons["start-voice"].tap()
        let orb = app.descendants(matching: .any).matching(identifier: "voice-orb").firstMatch
        XCTAssertTrue(orb.waitForExistence(timeout: 20))
        app.buttons["mute-voice"].tap()
        for pass in 1...2 {
            app.buttons["close-voice"].tap()
            gone(app.staticTexts["voice-panel"])
            XCTAssertTrue(self.selectedConversationTab(app).waitForExistence(timeout: 5))
            XCTAssertEqual(self.selectedConversationTab(app).label, title)
            XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.exists)
            XCTAssertTrue(app.buttons["end-voice-compact"].isHittable)
            capture(app, "voice-down-arrow-inbox-\(pass)")
            app.buttons["start-voice"].tap()
            XCTAssertTrue(orb.waitForExistence(timeout: 5))
            XCTAssertEqual(app.buttons["mute-voice"].label, "Unmute microphone")
            if pass == 1 {
                app.buttons["voice-return-chat"].tap()
                XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))
                XCTAssertTrue(self.selectedConversationTab(app).waitForExistence(timeout: 5))
                app.buttons["start-voice"].tap()
                XCTAssertTrue(orb.waitForExistence(timeout: 5))
            }
        }
        app.buttons["close-voice"].tap()
        XCTAssertTrue(app.buttons["end-voice-compact"].waitForExistence(timeout: 5))
        app.buttons["end-voice-compact"].tap()
        gone(app.buttons["end-voice-compact"])
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
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        XCTAssertEqual(composer(app).value as? String, "Keep my typed draft.")
        let userRow = conversation.otherElements["voice-transcript-user"].firstMatch
        let assistantRow = conversation.otherElements["voice-transcript-assistant"].firstMatch
        let user = userRow.staticTexts.firstMatch
        let assistant = assistantRow.staticTexts.firstMatch
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
        XCTAssertEqual(conversation.otherElements.matching(identifier: "voice-transcript-user").count, 1)
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
        let durable = conversation.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH %@", "message-assistant-demo-voice:")).firstMatch
        XCTAssertTrue(durable.waitForExistence(timeout: 10))
        XCTAssertEqual(conversation.staticTexts.matching(NSPredicate(format: "label == %@", "I can hear you clearly.")).count, 1, "Durable voice history replaces its pending final without duplication")
        XCTAssertEqual(composer(app).value as? String, "Keep my typed draft.")
        capture(app, "voice-05-ended-durable-chat")
        XCTAssertFalse(app.alerts.firstMatch.exists, "Transcript fixtures never request microphone access")
    }
    func testInlineToolsPreserveReasoningAndCommentarySequence() {
        let app = launch(["NANOCODEX_DEMO_MANY_TOOLS": "1"]); selectInbox(app)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        XCTAssertFalse(conversation.buttons["activity-disclosure"].exists)
        XCTAssertFalse(conversation.scrollViews["activity-timeline"].exists)
        let tools = conversation.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "tool-disclosure-tool-"))
        XCTAssertEqual(tools.count, 3)
        for tool in tools.allElementsBoundByIndex {
            XCTAssertEqual(tool.value as? String, "Collapsed")
        }
        let reasoning = conversation.staticTexts["Checking the reconnect boundary before changing the implementation."]
        let commentary = conversation.staticTexts["Checking the remaining steps."]
        let laterReasoning = conversation.staticTexts["The checks agree. Preparing a concise answer."]
        for _ in 0..<6 { if reasoning.isHittable { break }; conversation.swipeDown() }
        XCTAssertTrue(reasoning.isHittable)
        XCTAssertTrue(commentary.exists)
        XCTAssertTrue(laterReasoning.exists)
        let first = tools.element(boundBy: 0)
        let second = tools.element(boundBy: 1)
        let third = tools.element(boundBy: 2)
        XCTAssertLessThan(reasoning.frame.minY, first.frame.minY)
        XCTAssertLessThan(first.frame.minY, commentary.frame.minY)
        XCTAssertLessThan(commentary.frame.minY, second.frame.minY)
        XCTAssertLessThan(second.frame.minY, laterReasoning.frame.minY)
        XCTAssertLessThan(laterReasoning.frame.minY, third.frame.minY)
        XCTAssertFalse(conversation.staticTexts["Command"].exists)
        capture(app, "10-inline-sequence")
        let cardY = first.frame.minY
        first.tap()
        XCTAssertTrue(first.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH %@", "command-source-")).firstMatch.exists)
        XCTAssertFalse(conversation.staticTexts["Command"].exists, "The command stays in the card instead of being duplicated in details")
        XCTAssertEqual(first.frame.minY, cardY, accuracy: 4, "Expanding details keeps the tapped card in place")
        XCTAssertTrue(conversation.staticTexts["Exit code"].exists)
        XCTAssertEqual(second.value as? String, "Collapsed", "Each tool expands independently")
        XCTAssertEqual(third.value as? String, "Collapsed")
        capture(app, "11-inline-tool-details")
        for _ in 0..<5 { if first.isHittable { break }; conversation.swipeDown() }
        first.tap()
        gone(conversation.staticTexts["Command"])
        XCTAssertEqual(first.value as? String, "Collapsed")
        XCTAssertTrue(commentary.exists, "Collapsing a tool preserves inline commentary")
    }
    func testSendButtonBecomesStopOnlyForAnEmptyRunningDraft() {
        let app = launch(); selectInbox(app)
        let action = app.buttons["send"]
        XCTAssertEqual(action.label, "Stop turn")
        XCTAssertEqual(app.buttons.matching(identifier: "Stop turn").count, 1)
        let actionX = action.frame.midX
        composer(app).tap(); composer(app).typeText("Keep going")
        XCTAssertEqual(action.label, "Send message")
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
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 20))
        navigationAction(app, "New agent").tap()
        queue(app, "Reply exactly with this Markdown, without an enclosing code fence:\n# Markdown verified\n\n**Bold text** and `inline code`.\n\n- First item\n- Second item\n\n```swift\nlet answer = 42\n```\n\n| Name | Value |\n| --- | --- |\n| Answer | 42 |")
        gone(app.keyboards.firstMatch)
        let card = app.descendants(matching: .any)["conversation"].firstMatch
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

        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))
        queue(app, "Reply exactly with: **Follow-up verified**")
        gone(app.keyboards.firstMatch)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.staticTexts["Follow-up verified"].waitForExistence(timeout: 90))
        conversation.swipeUp()
        capture(app, "live-markdown-conversation-keyboard-dismissed")

    }

    func testMarkdownRendersInConversationAndAfterTabSwitch() {
        let app = launch(["NANOCODEX_DEMO_MARKDOWN": "1"])
        let card = app.descendants(matching: .any)["conversation"].firstMatch
        let heading = card.staticTexts["Markdown check"]
        for _ in 0..<8 { if heading.isHittable { break }; card.swipeDown() }
        XCTAssertTrue(heading.waitForExistence(timeout: 5), "Keep the start of replies longer than 1,400 characters")
        XCTAssertTrue(card.staticTexts["Read bold, italic, and inline code with a link."].exists)
        XCTAssertTrue(card.staticTexts["First item"].exists)
        XCTAssertFalse(card.staticTexts["# Markdown check"].exists)
        capture(app, "markdown-inbox")
        for _ in 0..<12 {
            if card.buttons["Copy code"].isHittable { break }
            // Short drags locate the code header without flinging past it as
            // renderer typography and the keyboard-safe viewport change.
            card.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.7))
                .press(forDuration: 0.01, thenDragTo: card.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)))
        }
        XCTAssertTrue(card.buttons["Copy code"].isHittable)
        XCTAssertTrue(card.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "let marker = \"**literal**\"")).firstMatch.exists)
        capture(app, "markdown-conversation-code")
        selectTab(app, id: "inbox", title: "Build the agent inbox")
        selectTab(app, id: "durability", title: "Make long sessions bulletproof")

        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
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

        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        let latest = conversation.staticTexts["Review note 80"]
        XCTAssertTrue(latest.waitForExistence(timeout: 5))
        XCTAssertTrue(latest.isHittable, "Lazy rich messages must open at the latest reply")
        capture(app, "long-markdown-before-keyboard")
        composer(app).tap()
        composer(app).typeText("Keep the latest reply in view")
        capture(app, "long-markdown-after-keyboard")
        // Following keeps the reply's tail above the keyboard. The heading of
        // a reply taller than this viewport may legitimately move offscreen.
        // Reading-anchor stability is exercised separately while browsing history.
        let tail = conversation.staticTexts.matching(NSPredicate(format: "label == %@", "Retained")).allElementsBoundByIndex.last
        XCTAssertNotNil(tail)
        XCTAssertTrue(tail?.isHittable == true)
        XCTAssertLessThanOrEqual(tail?.frame.maxY ?? .infinity, composer(app).frame.minY)
        capture(app, "long-markdown-latest-with-keyboard")

        XCTAssertEqual(composer(app).value as? String, "Keep the latest reply in view")
    }









    func testPrivateOutputLinksRenderNativePreviewAndSaveCards() throws {
        let clip = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "FrontiersMerchSample", withExtension: "mp4"))
        let app = launch(["NANOCODEX_DEMO_OUTPUT_LINKS": "1",
                          "NANOCODEX_DEMO_VIDEO_BASE64": try Data(contentsOf: clip).base64EncodedString()])
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 10))
        XCTAssertFalse(conversation.buttons["View Not an output"].exists,
                       "Only canonical /brain/outputs links become file cards")
        XCTAssertEqual(conversation.buttons.matching(identifier: "published-output-open").count, 2)
        XCTAssertEqual(conversation.buttons.matching(identifier: "published-output-save").count, 2)
        XCTAssertTrue(conversation.links["sandbox:/brain/outputs/frontiers-next/frontiers-merch-launch-actual-character.mp4"].exists)
        XCTAssertTrue(conversation.links["sandbox:/brain/outputs/frontiers-next/frontiers-launch-and-drops.zip"].exists)
        capture(app, "private-output-links")
        let launchVideo = conversation.buttons["View Main launch video"]
        XCTAssertTrue(launchVideo.isHittable)
        launchVideo.tap()
        let done = app.buttons["Done"]
        XCTAssertTrue(done.waitForExistence(timeout: 10), "The video should open in native Quick Look")
        capture(app, "private-output-video-preview")
        done.tap()
        let saveVideo = conversation.buttons["Save or share Main launch video"]
        XCTAssertTrue(saveVideo.isHittable)
        saveVideo.tap()
        XCTAssertTrue(app.otherElements["ShareSheet.RemoteContainerView"].waitForExistence(timeout: 10),
                      "The iOS share sheet should open for a local video file")
        capture(app, "private-output-save-share")
    }

    func testNativeMediaPreviewZoomPlaybackAndDraftRestoration() throws {
        let clip = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "VideoAudioCheck", withExtension: "mp4"))
        let app = launch(["NANOCODEX_DEMO_GENERATED_OUTPUTS": "1", "NANOCODEX_DEMO_VIDEO_BASE64": try Data(contentsOf: clip).base64EncodedString()])
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 10))
        let photo = app.descendants(matching: .any).matching(identifier: "generated-image-loaded").firstMatch
        for _ in 0..<5 { if photo.isHittable { break }; conversation.swipeDown() }
        XCTAssertTrue(photo.waitForExistence(timeout: 10)); photo.tap()
        let done = app.buttons["Done"]
        XCTAssertTrue(done.waitForExistence(timeout: 10), app.debugDescription)
        capture(app, "native-photo-preview")
        app.pinch(withScale: 2.5, velocity: 1)
        capture(app, "native-photo-zoom")
        done.tap()
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        let video = app.buttons["generated-video-play"]
        for _ in 0..<5 { if video.isHittable { break }; conversation.swipeUp() }
        XCTAssertTrue(video.waitForExistence(timeout: 5)); video.tap()
        XCTAssertTrue(done.waitForExistence(timeout: 10), app.debugDescription)
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        let playback = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.buttons["Play"].firstMatch.exists || app.buttons["Pause"].firstMatch.exists
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [playback], timeout: 5), .completed, app.debugDescription)
        capture(app, "native-video-playback")
        done.tap()
        composer(app).tap(); composer(app).typeText("Keep this draft after viewing media")
        conversation.swipeDown()
        for _ in 0..<5 { if photo.isHittable { break }; conversation.swipeDown() }
        photo.tap(); XCTAssertTrue(done.waitForExistence(timeout: 10)); done.tap()
        XCTAssertEqual(composer(app).value as? String, "Keep this draft after viewing media")
        capture(app, "native-media-restores-draft")
    }

    func testGeneratedAttachmentsStayVisibleWhileToolDetailsAreCollapsed() {
        func assertNoInternalOutput(_ scope: XCUIElement, file: StaticString = #filePath, line: UInt = #line) {
            for marker in ["INTERNAL_MEMORY_RECORD", "INTERNAL_COMMAND_OUTPUT", "INTERNAL_WAIT_OUTPUT", "Script completed", "\"memories\""] {
                XCTAssertFalse(scope.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", marker)).firstMatch.exists,
                               "Internal tool output escaped collapsed card: " + marker, file: file, line: line)
            }
        }
        let app = launch(["NANOCODEX_DEMO_GENERATED_OUTPUTS": "1"])
        let cardImage = app.descendants(matching: .any).matching(identifier: "generated-image-loaded").firstMatch
        XCTAssertTrue(cardImage.waitForExistence(timeout: 10), "The inbox card renders actual emitted PNG bytes")
        assertNoInternalOutput(app)
        capture(app, "generated-output-inbox-image")

        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        XCTAssertTrue(conversation.staticTexts["Generated chart"].waitForExistence(timeout: 10))
        XCTAssertTrue(conversation.staticTexts["The three bars are ready to review."].exists)
        XCTAssertFalse(conversation.buttons["activity-disclosure"].exists)
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
        let memory = conversation.buttons["tool-disclosure-generated-turn::tool:memory"]
        for _ in 0..<10 { if memory.isHittable { break }; conversation.swipeDown() }
        XCTAssertTrue(memory.isHittable)
        memory.tap()
        XCTAssertTrue(conversation.staticTexts["INTERNAL_MEMORY_RECORD"].waitForExistence(timeout: 5), "Tool details remain available when explicitly opened")
        capture(app, "memory-inline-details")
        for _ in 0..<6 { if memory.isHittable { break }; conversation.swipeDown() }
        memory.tap()
        assertNoInternalOutput(conversation)
    }


    func testComposerExpandsOnlyAfterFiveRenderedLinesAndKeepsDraft() {
        let app = launch()
        let input = composer(app)
        let expand = app.buttons["expand-composer"]
        XCTAssertFalse(expand.exists)
        input.tap()
        let fiveLines = "One\nTwo\nThree\nFour\nFive"
        input.typeText(fiveLines)
        if input.value as? String != fiveLines {
            input.typeKey("a", modifierFlags: .command)
            input.typeText(fiveLines)
        }
        XCTAssertEqual(input.value as? String, fiveLines)
        XCTAssertFalse(expand.exists, "Five visible lines fit without an expansion action")
        let fiveLineHeight = input.frame.height
        input.typeText("\n")
        XCTAssertTrue(expand.waitForExistence(timeout: 5), "The empty sixth line also overflows")
        input.typeText("Six")
        XCTAssertEqual(input.frame.height, fiveLineHeight, accuracy: 2, "Overflow scrolls inside the editor")
        XCTAssertLessThan(expand.frame.maxY, app.buttons["send"].frame.minY)
        XCTAssertEqual(expand.frame.midX, app.buttons["send"].frame.midX, accuracy: 2)
        expand.tap()
        let expanded = app.textViews["expanded-composer"]
        XCTAssertTrue(expanded.waitForExistence(timeout: 5))
        XCTAssertEqual(expanded.value as? String, "One\nTwo\nThree\nFour\nFive\nSix")
        expanded.typeText(" edited")
        app.buttons["collapse-composer"].tap()
        XCTAssertEqual(input.value as? String, "One\nTwo\nThree\nFour\nFive\nSix edited")
        capture(app, "composer-five-line-overflow")
    }

    func testComposerWrappedTextScrollsWithoutChangingDraft() {
        let app = launch()
        let input = composer(app)
        let draft = String(repeating: "Native editing preserves selection and scrolling. ", count: 12)
        input.tap(); input.typeText(draft)
        XCTAssertTrue(app.buttons["expand-composer"].waitForExistence(timeout: 5), "Soft wrapping also counts toward the five-line limit")
        let boundedHeight = input.frame.height
        input.swipeDown(); input.swipeUp()
        XCTAssertEqual(input.value as? String, draft)
        XCTAssertTrue(app.keyboards.firstMatch.exists, "Scrolling inside the native editor keeps editing active")
        XCTAssertEqual(input.frame.height, boundedHeight, accuracy: 2)
        app.buttons["expand-composer"].tap()
        let expanded = app.textViews["expanded-composer"]
        XCTAssertTrue(expanded.waitForExistence(timeout: 5))
        expanded.typeText("END")
        app.buttons["collapse-composer"].tap()
        XCTAssertEqual(input.value as? String, draft + "END")
    }

    func testSwipeDownDismissesKeyboardAndKeepsDraft() {
        for longThread in [false, true] {
            let app = launch(longThread ? ["NANOCODEX_DEMO_LONG_THREAD": "1"] : [:])
            let original = self.selectedConversationTab(app).label
            let draft = "Keep this draft after swiping down"
            composer(app).tap(); composer(app).typeText(draft)
            XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5))
            let conversation = app.descendants(matching: .any)["conversation"].firstMatch
            let visibleBottom = min(conversation.frame.maxY, composer(app).frame.minY - 20)
            let origin = app.coordinate(withNormalizedOffset: .zero)
            origin.withOffset(CGVector(dx: conversation.frame.midX, dy: (conversation.frame.minY + visibleBottom) / 2))
                .press(forDuration: 0.01,
                    thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.92)),
                    withVelocity: .slow, thenHoldForDuration: 0)
            gone(app.keyboards.firstMatch)
            XCTAssertTrue(conversation.exists)
            XCTAssertEqual(self.selectedConversationTab(app).label, original)
            XCTAssertEqual(composer(app).value as? String, draft)
            XCTAssertTrue(app.buttons["new-conversation"].isHittable)
            capture(app, longThread ? "swipe-down-long-conversation" : "swipe-down-short-conversation")
            app.terminate()
        }
    }

    func testSendingAndQueueingDismissKeyboardInConversation() {
        let app = launch()
        navigationAction(app, "New agent").tap()
        composer(app).tap(); composer(app).typeText("First message")
        XCTAssertTrue(app.keyboards.firstMatch.exists)
        app.buttons["send"].tap()
        XCTAssertTrue(latestUserText(app).waitForExistence(timeout: 5))
        gone(app.keyboards.firstMatch)
        XCTAssertTrue(app.buttons["conversation-drawer-open"].isHittable)
        XCTAssertEqual(latestUserText(app).label, "First message")
        XCTAssertFalse(app.scrollViews["pending-messages"].exists)
        XCTAssertEqual(app.buttons["send"].label, "Stop turn")
        capture(app, "inbox-sent-keyboard-dismissed")

        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))
        queue(app, "Follow-up message")
        gone(app.keyboards.firstMatch)
        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.staticTexts["Follow-up message"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["steer-now"].exists)
        capture(app, "conversation-direct-send-keyboard-dismissed")
    }
    func testCommandCardShowsFullMultilineCommandAndDirectory() {
        let app = launch(["NANOCODEX_DEMO_COMMAND_CARD": "1",
                          "NANOCODEX_DEMO_PROFILE": UUID().uuidString]); selectInbox(app)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        let card = conversation.buttons["tool-disclosure-demo-command-card"]
        XCTAssertTrue(card.waitForExistence(timeout: 5))
        let command = """
        printf '%s\\n' 'Inspect the complete synthetic command, including this deliberately long first line beyond the old 140 character preview boundary.'
        swift test --package-path 'apple/InboxCore' --filter CommandPresentationTests
        exit 7
        """
        XCTAssertGreaterThan(command.count, 140)
        let source = card.descendants(matching: .any)["command-source-demo-command-card"]
        XCTAssertTrue(source.exists, "The collapsed card exposes the complete command")
        XCTAssertTrue(source.isHittable, "The complete command is visible before expanding details")
        XCTAssertEqual(source.label, command, "Command input preserves every character and newline")
        let directory = card.descendants(matching: .any)["command-directory-demo-command-card"]
        XCTAssertTrue(directory.exists)
        XCTAssertEqual(directory.label, "/workspace/demo project")
        XCTAssertFalse(card.staticTexts["Run command"].exists)
        XCTAssertTrue(card.label.contains("Failed"))
        XCTAssertTrue(card.label.contains("exit 7"))
        capture(app, "command-card-full-input-and-directory")
        card.tap()
        let failure = conversation.staticTexts["Synthetic command failed with exit 7. No command was executed."]
        for _ in 0..<4 { if failure.isHittable { break }; conversation.swipeUp() }
        XCTAssertTrue(failure.isHittable, "The expanded command failure remains readable")
        capture(app, "command-card-expanded-failure")
    }

    func testOversizedCommandStaysCompactAndOpensCompleteNativeSourceViewer() {
        let app = launch(["NANOCODEX_DEMO_OVERSIZED_COMMAND": "1",
                          "NANOCODEX_DEMO_PROFILE": UUID().uuidString]); selectInbox(app)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        let card = conversation.buttons["tool-disclosure-demo-oversized-command"]
        XCTAssertTrue(card.waitForExistence(timeout: 5))
        print("OVERSIZED_COMMAND_CARD_HEIGHT=\(card.frame.height)")
        XCTAssertEqual(card.descendants(matching: .any)["command-directory-demo-oversized-command"].label, "Default directory")
        XCTAssertGreaterThan(card.frame.height, 0)
        XCTAssertLessThanOrEqual(card.frame.height, 300, "Oversized source must not create screens of blank command card")
        let followingMessage = conversation.staticTexts["The oversized command is complete. This message stays reachable."]
        XCTAssertTrue(followingMessage.waitForExistence(timeout: 5))
        XCTAssertTrue(followingMessage.isHittable, "The next assistant message stays accessible below the collapsed card")
        capture(app, "oversized-command-collapsed")

        card.tap()
        let fullSource = conversation.buttons["command-full-source-demo-oversized-command"]
        XCTAssertTrue(fullSource.waitForExistence(timeout: 5))
        XCTAssertEqual(fullSource.label, "View full command")
        for _ in 0..<3 { if fullSource.isHittable { break }; conversation.swipeUp() }
        XCTAssertTrue(fullSource.isHittable)
        fullSource.tap()
        XCTAssertTrue(app.navigationBars["Command"].waitForExistence(timeout: 5))
        let source = app.textViews["tool-source-text"]
        XCTAssertTrue(source.waitForExistence(timeout: 5), "Full source uses a native text view")
        let expectedSource = "printf '%s' '"
            + String(repeating: "QUJD", count: 47_279)
            + "' | base64 -d > /workspace/synthetic.png"
        XCTAssertGreaterThan(expectedSource.utf8.count, 189_000)
        XCTAssertEqual(source.value as? String, expectedSource, "The viewer preserves all source characters and newlines")
        source.tap()
        XCTAssertFalse(app.keyboards.firstMatch.exists, "Source is read-only")
        let copy = app.buttons["tool-source-copy"]
        XCTAssertTrue(copy.isHittable)
        XCTAssertEqual(copy.label, "Copy source")
        copy.tap()
        capture(app, "oversized-command-full-source")
        let done = app.buttons["tool-source-done"]
        XCTAssertTrue(done.isHittable)
        XCTAssertEqual(done.label, "Done")
        done.tap()
        gone(source, timeout: 3)
        XCTAssertTrue(conversation.isHittable, "Dismissing the source viewer promptly returns to the conversation")
    }

    func testUserNavigationReleasesControlsAfterHistoryWithoutUserMessages() {
        let app = launch(["NANOCODEX_DEMO_LONG_THREAD": "1", "NANOCODEX_DEMO_HISTORY_DELAY_MS": "50",
                          "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        selectInbox(app)
        let previous = app.buttons["previous-user-message"]
        XCTAssertTrue(previous.waitForExistence(timeout: 10))
        XCTAssertTrue(previous.isEnabled)
        previous.tap()
        let collapse = app.buttons["toggle-all-tools"]
        let finished = NSPredicate { _, _ in collapse.isEnabled && !previous.isEnabled }
        expectation(for: finished, evaluatedWith: nil)
        waitForExpectations(timeout: 10)
        XCTAssertFalse(app.buttons["next-user-message"].isEnabled)
    }

    func testConversationArrowNavigationResponsiveness() {
        let app = launch(["NANOCODEX_DEMO_CODE_MODE_BATCH": "1", "NANOCODEX_DEMO_THREAD_CONTROLS": "1",
                          "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        selectInbox(app)
        let previous = app.buttons["previous-user-message"]
        let next = app.buttons["next-user-message"]
        XCTAssertTrue(previous.waitForExistence(timeout: 10))
        previous.tap()
        if previous.isEnabled { previous.tap() }
        XCTAssertFalse(previous.isEnabled)
        let options = XCTMeasureOptions()
        options.iterationCount = 5
        var metrics: [XCTMetric] = [XCTClockMetric(), XCTCPUMetric(application: app)]
        if #available(iOS 26.0, *) { metrics.append(XCTHitchMetric(application: app)) }
        measure(metrics: metrics, options: options) {
            next.tap()
            XCTAssertFalse(next.isEnabled)
            previous.tap()
            XCTAssertFalse(previous.isEnabled)
        }
        capture(app, "transparent-controls-first-message")
        next.tap()
        capture(app, "transparent-controls-next-message")
    }

    func testLiveConversationArrowNavigation() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in test device with existing conversation history.")
        }
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 30))
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 20))
        let previous = app.buttons["previous-user-message"]
        let next = app.buttons["next-user-message"]
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: previous)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 20), .completed)
        let draft = composer(app).value as? String
        let options = XCTMeasureOptions()
        options.iterationCount = 3
        var metrics: [XCTMetric] = [XCTClockMetric(), XCTCPUMetric(application: app)]
        if #available(iOS 26.0, *) { metrics.append(XCTHitchMetric(application: app)) }
        // Read-only: navigate existing messages without sending or changing drafts.
        measure(metrics: metrics, options: options) {
            previous.tap()
            XCTAssertTrue(next.waitForExistence(timeout: 2))
            if next.isEnabled { next.tap() }
            XCTAssertTrue(previous.isEnabled)
        }
        XCTAssertEqual(composer(app).value as? String, draft)
    }

    func testToolsButtonExpandsAndCollapsesRepeatedly() {
        let app = launch(["NANOCODEX_DEMO_CODE_MODE_BATCH": "1",
                          "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        selectInbox(app)
        let toggle = app.buttons["toggle-all-tools"]
        let batch = app.buttons["code-mode-batch-demo-code-mode-batch"]
        let child = app.buttons["tool-disclosure-demo-code-mode-batch/code-1"]
        let source = app.buttons["code-mode-javascript-demo-code-mode-batch"]
        XCTAssertTrue(toggle.waitForExistence(timeout: 10))
        for _ in 0..<3 {
            XCTAssertEqual(toggle.label, "Collapse all tool calls")
            toggle.tap()
            XCTAssertEqual(batch.value as? String, "Collapsed")
            XCTAssertEqual(toggle.label, "Expand all tool calls")
            toggle.tap()
            XCTAssertEqual(batch.value as? String, "Expanded")
            XCTAssertEqual(child.value as? String, "Expanded")
            XCTAssertEqual(source.value as? String, "Expanded")
        }
        capture(app, "tools-toggle-expanded")
        toggle.tap()
        capture(app, "tools-toggle-collapsed")
        // Scroll with a real gesture to release the preserved reading anchor.
        app.descendants(matching: .any)["conversation"].firstMatch.swipeUp()
        // A manual disclosure change must update the global button's next action.
        batch.tap()
        XCTAssertEqual(toggle.label, "Collapse all tool calls")
        toggle.tap()
        XCTAssertEqual(batch.value as? String, "Collapsed")
        switchConversation(app, id: "durability")
        switchConversation(app, id: "inbox")
        XCTAssertEqual(toggle.label, "Expand all tool calls")
        toggle.tap()
        XCTAssertEqual(child.value as? String, "Expanded")
    }

    func testThreadControlsCollapseAndNavigateUserMessages() {
        let app = launch(["NANOCODEX_DEMO_CODE_MODE_BATCH": "1", "NANOCODEX_DEMO_THREAD_CONTROLS": "1",
                          "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        selectInbox(app)
        let collapse = app.buttons["toggle-all-tools"]
        XCTAssertTrue(collapse.waitForExistence(timeout: 10))
        collapse.tap()
        let batch = app.buttons["code-mode-batch-demo-code-mode-batch"]
        XCTAssertEqual(batch.value as? String, "Collapsed")
        capture(app, "thread-controls-collapsed")
        let previous = app.buttons["previous-user-message"]
        let next = app.buttons["next-user-message"]
        previous.tap()
        if previous.isEnabled { previous.tap() }
        XCTAssertFalse(previous.isEnabled)
        XCTAssertTrue(next.isEnabled)
        capture(app, "thread-controls-first-user-message")
        next.tap()
        XCTAssertFalse(next.isEnabled)
        XCTAssertTrue(previous.isEnabled)
        capture(app, "thread-controls-next-user-message")
        previous.tap()
        batch.tap()
        let child = app.buttons["tool-disclosure-demo-code-mode-batch/code-1"]
        XCTAssertEqual(child.value as? String, "Collapsed")
        child.tap()
        collapse.tap()
        batch.tap()
        XCTAssertEqual(child.value as? String, "Collapsed", "Collapse all also clears nested disclosure state")
        switchConversation(app, id: "durability")
        switchConversation(app, id: "inbox")
        XCTAssertEqual(batch.value as? String, "Expanded", "Thread disclosure choices survive switching threads")
    }

    func testCodeModeBatchKeepsCommandsTogether() {
        let app = launch(["NANOCODEX_DEMO_CODE_MODE_BATCH": "1", "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        selectInbox(app)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        let batch = conversation.buttons["code-mode-batch-demo-code-mode-batch"]
        XCTAssertTrue(batch.waitForExistence(timeout: 10))
        XCTAssertEqual(batch.value as? String, "Expanded")
        XCTAssertEqual(conversation.buttons.matching(identifier: "tool-disclosure-demo-code-mode-batch/code-1").count, 1)
        XCTAssertTrue(conversation.buttons["tool-disclosure-demo-code-mode-batch/code-2"].isHittable)
        capture(app, "code-mode-batch-expanded")
        batch.tap()
        XCTAssertEqual(batch.value as? String, "Collapsed")
        XCTAssertFalse(conversation.buttons["tool-disclosure-demo-code-mode-batch/code-1"].exists)
        capture(app, "code-mode-batch-collapsed")
        batch.tap()
        conversation.descendants(matching: .any)["code-mode-javascript-demo-code-mode-batch"].tap()
        XCTAssertTrue(conversation.descendants(matching: .any)["code-mode-source-demo-code-mode-batch"].exists)
        capture(app, "code-mode-batch-javascript")
    }

    func testCodeModeCardShowsFullMultilineSourceAndOutput() {
        assertCodeModeCardShowsFullMultilineSourceAndOutput(environment: "NANOCODEX_DEMO_CODE_MODE_CARD")
    }

    func testCodeModeObjectCardShowsFullMultilineSourceAndOutput() {
        assertCodeModeCardShowsFullMultilineSourceAndOutput(environment: "NANOCODEX_DEMO_CODE_MODE_OBJECT_CARD")
    }

    private func assertCodeModeCardShowsFullMultilineSourceAndOutput(environment: String) {
        let app = launch([environment: "1",
                          "NANOCODEX_DEMO_PROFILE": UUID().uuidString]); selectInbox(app)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        let card = conversation.buttons["code-mode-batch-demo-code-mode-card"]
        XCTAssertTrue(card.waitForExistence(timeout: 5))
        let expectedSource = """
        // Inspect the complete synthetic JavaScript source, preserving every newline beyond the old 140 character preview boundary.
        const result = await tools.exec_command({
          cmd: 'git status --short',
          workdir: '/workspace/demo'
        });
        text(result.output);
        """ + "\n// " + String(repeating: "Preserve full source. ", count: 20)
        XCTAssertGreaterThan(expectedSource.count, 512)
        XCTAssertEqual(card.value as? String, "Expanded")
        XCTAssertTrue(card.staticTexts["Code Mode"].exists)
        XCTAssertFalse(conversation.staticTexts["Run code"].exists)
        XCTAssertFalse(conversation.descendants(matching: .any)["code-mode-source-demo-code-mode-card"].exists)
        capture(app, "code-mode-batch-expanded")
        conversation.descendants(matching: .any)["code-mode-javascript-demo-code-mode-card"].tap()
        let source = conversation.descendants(matching: .any)["code-mode-source-demo-code-mode-card"]
        XCTAssertTrue(source.waitForExistence(timeout: 5))
        XCTAssertEqual(source.label, expectedSource, "Expanded source preserves every character and newline")
        let copy = conversation.buttons["code-mode-copy-demo-code-mode-card"]
        XCTAssertTrue(copy.isHittable, "Copy is directly accessible outside the disclosure button")
        copy.tap()
        let detail = conversation.descendants(matching: .any)["tool-detail-demo-code-mode-card"]
        XCTAssertTrue(detail.waitForExistence(timeout: 5))
        XCTAssertFalse(detail.staticTexts["Code"].exists, "Expanded details do not repeat the Code input")
        XCTAssertFalse(detail.staticTexts.matching(NSPredicate(format: "label == %@", expectedSource)).firstMatch.exists, "Expanded details do not duplicate the source")
        XCTAssertFalse(conversation.staticTexts["Run code"].exists)
        let output = conversation.staticTexts["Synthetic code result"]
        for _ in 0..<4 { if output.isHittable { break }; conversation.swipeUp() }
        XCTAssertTrue(output.isHittable, "The expanded Code Mode result remains readable")
        capture(app, "code-mode-card-expanded-result")
    }

    func testToolFailureIsReadable() {
        let app = launch(["NANOCODEX_DEMO_TOOL_ERROR": "1"]); selectInbox(app)

        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        let tool = conversation.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "tool-disclosure-tool-")).firstMatch
        XCTAssertTrue(tool.waitForExistence(timeout: 5))
        XCTAssertTrue(tool.label.contains("Failed"), "Failed calls stay discoverable on their collapsed card")
        tool.tap()
        let failure = conversation.staticTexts["The browser disconnected. Reconnect it and try again."]
        let visible = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: failure)
        XCTAssertEqual(XCTWaiter.wait(for: [visible], timeout: 5), .completed)
        capture(app, "12-activity-failure")
    }
    // Streaming Markdown and an expanding tool must share one timeline without
    // overlapping rows, losing tool state, or covering the final answer.
    func testStreamingMarkdownAndToolProgressShareTimeline() {
        let app = launch(["NANOCODEX_DEMO_RICH_STREAM": "1"]); selectInbox(app)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        let tool = conversation.buttons["tool-disclosure-demo-rich-tool"]
        XCTAssertTrue(tool.waitForExistence(timeout: 10))
        XCTAssertTrue(tool.label.contains("Running"))
        tool.tap()
        XCTAssertEqual(tool.value as? String, "Expanded")
        let done = conversation.staticTexts["Rich streaming review complete."]
        XCTAssertTrue(done.waitForExistence(timeout: 30))
        let latest = app.buttons["latest-messages"]
        if latest.isHittable { latest.tap() }
        XCTAssertTrue(done.isHittable)
        XCTAssertLessThanOrEqual(done.frame.maxY, composer(app).frame.minY)
        for _ in 0..<8 { if tool.isHittable { break }; conversation.swipeDown() }
        XCTAssertTrue(tool.isHittable)
        XCTAssertTrue(tool.label.contains("Completed"))
        XCTAssertEqual(tool.value as? String, "Expanded")
        XCTAssertTrue(conversation.staticTexts["Synthetic checks passed."].exists)
        capture(app, "rich-stream-tool-completed")
        if latest.isHittable { latest.tap() }
        XCTAssertTrue(done.isHittable)
        capture(app, "rich-stream-final-markdown")
    }

    func testSuccessiveToolCardsFollowTailWithoutFlashingJump() {
        let app = launch(["NANOCODEX_DEMO_TOOL_ARRIVALS": "1",
                          "NANOCODEX_DEMO_PROFILE": UUID().uuidString]); selectInbox(app)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        let jump = app.buttons["latest-messages"]
        let jumpAppeared = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: jump)
        jumpAppeared.isInverted = true
        let arrivals = [1, 3, 6].map { index in
            let card = conversation.buttons["tool-disclosure-demo-tool-arrival-\(index)"]
            return XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
                card.exists && card.isHittable
                    && card.frame.minY >= conversation.frame.minY
                    && card.frame.maxY <= self.composer(app).frame.minY
            }, object: nil)
        }
        // Observe the button throughout the sequence, including a settling interval
        // after the last arrival, rather than only checking its final absence.
        XCTAssertEqual(XCTWaiter.wait(for: arrivals + [jumpAppeared], timeout: 10), .completed,
                       "Successive individual tool cards must stay visible without offering a jump while following")
        let latest = conversation.buttons["tool-disclosure-demo-tool-arrival-6"]
        XCTAssertTrue(latest.isHittable, "The latest tool remains visible after all arrivals")
        XCTAssertFalse(jump.exists)
        capture(app, "successive-tool-cards-follow-tail")
    }

    func testStreamingGrowthDoesNotMoveReaderInEarlierParagraphs() {
        let app = launch(["NANOCODEX_DEMO_STREAMING_GROWTH": "1", "NANOCODEX_DEMO_STREAM_INTERVAL_MS": "1000", "NANOCODEX_RENDER_COUNTER": "1"]); selectInbox(app)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        let middle = conversation.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", "Stream paragraph 20.")).firstMatch
        XCTAssertTrue(middle.waitForExistence(timeout: 35))
        conversation.swipeDown()
        guard let anchor = conversation.staticTexts.allElementsBoundByIndex.first(where: {
            $0.isHittable && $0.label.hasPrefix("Stream paragraph ") && $0.frame.minY >= conversation.frame.minY
        }) else { return XCTFail("Expected a visible streaming paragraph anchor") }
        let label = anchor.label, y = anchor.frame.minY
        print("STREAM_ANCHOR_BEFORE \(label) y=\(y) \(app.staticTexts["conversation-native-scroll-state"].label)")
        XCTAssertFalse(conversation.staticTexts["Streaming response complete."].exists,
                       "Capture the reading anchor while the response is still streaming")
        let completion = conversation.staticTexts["Streaming response complete."]
        XCTAssertTrue(completion.waitForExistence(timeout: 45), "Observe a real streaming update before checking the anchor")
        print("STREAM_ANCHOR_AFTER \(app.staticTexts["conversation-native-scroll-state"].label)")
        XCTAssertTrue(conversation.staticTexts[label].isHittable)
        XCTAssertEqual(conversation.staticTexts[label].frame.minY, y, accuracy: 4,
                       "Streaming into the same response must preserve the paragraph being read")
        capture(app, "streaming-preserves-reading-position")
    }

    func testQuickDragDuringStreamingKeepsTheChosenPosition() {
        let app = launch(["NANOCODEX_DEMO_STREAMING_GROWTH": "1",
                          "NANOCODEX_DEMO_STREAM_INTERVAL_MS": "350"])
        selectInbox(app)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        let early = conversation.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", "Stream paragraph 12.")).firstMatch
        XCTAssertTrue(early.waitForExistence(timeout: 15))
        let completion = conversation.staticTexts["Streaming response complete."]
        // Release quickly while paragraphs keep arriving and changing row height.
        // This exercises the deferred metrics/idle race, including deceleration.
        let start = conversation.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.35))
        let end = conversation.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.65))
        start.press(forDuration: 0.01, thenDragTo: end)
        let latest = app.buttons["latest-messages"]
        XCTAssertTrue(latest.waitForExistence(timeout: 5))
        XCTAssertFalse(completion.exists, "The reader must leave the live tail before streaming finishes")
        // Stop inertia with a stationary touch, then capture the visible paragraph.
        conversation.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        guard let paragraph = conversation.staticTexts.allElementsBoundByIndex.first(where: {
            $0.isHittable && $0.label.hasPrefix("Stream paragraph ")
                && $0.frame.minY > conversation.frame.minY + 100
                && $0.frame.maxY < conversation.frame.maxY - 160
        }) else { return XCTFail("Expected a visible earlier paragraph after dragging") }
        let label = paragraph.label, y = paragraph.frame.minY
        let moved = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            let current = conversation.staticTexts[label]
            return !current.isHittable || abs(current.frame.minY - y) > 4
        }, object: nil)
        moved.isInverted = true
        XCTAssertEqual(XCTWaiter.wait(for: [moved], timeout: 3), .completed,
                       "Streaming must not pull the reader back after a quick drag")
        XCTAssertTrue(latest.isHittable)
        latest.tap()
        let followed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true AND hittable == true"), object: completion)
        XCTAssertEqual(XCTWaiter.wait(for: [followed], timeout: 30), .completed)
        gone(latest)
        capture(app, "quick-streaming-drag-and-resume")
    }

    func testLiveTailFollowsUpdatesAndOffersCompactJumpAfterReadingHistory() {
        let app = launch(["NANOCODEX_DEMO_STREAMING_GROWTH": "1"]); selectInbox(app)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        let update = conversation.staticTexts["Streaming response complete."]
        XCTAssertTrue(update.waitForExistence(timeout: 20))
        XCTAssertTrue(update.isHittable, "New response layout should follow the live tail")
        conversation.swipeDown(); conversation.swipeDown()
        let jump = app.buttons["latest-messages"]
        capture(app, "streaming-history-before-jump")
        XCTAssertTrue(jump.waitForExistence(timeout: 5))
        XCTAssertTrue(jump.isHittable)
        XCTAssertEqual(jump.frame.width, 44, accuracy: 2)
        XCTAssertEqual(jump.frame.height, 44, accuracy: 2)
        XCTAssertEqual(jump.frame.midX, conversation.frame.midX, accuracy: 2)
        XCTAssertEqual(jump.label, "Latest messages")
        XCTAssertLessThanOrEqual(jump.frame.maxY, composer(app).frame.minY)
        XCTAssertLessThan(composer(app).frame.minY - jump.frame.maxY, 80,
                          "The down arrow should sit immediately above the composer")
        capture(app, "compact-latest-messages")
        jump.tap()
        XCTAssertTrue(update.isHittable)
        gone(jump)
    }

    func testLatestButtonResumesFollowingSubsequentStreamingArrivals() {
        let app = launch(["NANOCODEX_DEMO_STREAMING_GROWTH": "1", "NANOCODEX_DEMO_STREAM_INTERVAL_MS": "1000"]); selectInbox(app)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        let early = conversation.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", "Stream paragraph 8.")).firstMatch
        XCTAssertTrue(early.waitForExistence(timeout: 10))
        XCTAssertTrue(early.isHittable, "The asynchronously prepared initial transcript must follow its tail")
        conversation.swipeDown()
        let jump = app.buttons["latest-messages"]
        XCTAssertTrue(jump.waitForExistence(timeout: 5))
        let completion = conversation.staticTexts["Streaming response complete."]
        XCTAssertFalse(completion.exists, "The jump must precede later arrivals to exercise continuous following")
        jump.tap()
        let following = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true AND hittable == true"), object: completion)
        XCTAssertEqual(XCTWaiter.wait(for: [following], timeout: 75), .completed,
                       "Go to bottom must keep following content arriving after the tap")
        gone(jump)
        capture(app, "latest-button-follows-later-arrivals")
    }

    func testDrawerButtonsAndEdgeSwipePreserveMiddleTranscriptAnchor() {
        let app = launch(["NANOCODEX_DEMO_LONG_THREAD": "1"]); selectInbox(app)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 5))
        conversation.swipeDown(); conversation.swipeDown()
        guard let anchor = conversation.staticTexts.allElementsBoundByIndex.first(where: {
            $0.isHittable && $0.label.hasPrefix("Progress note ") && $0.frame.minY >= conversation.frame.minY
        }) else { return XCTFail("Expected a visible middle transcript anchor") }
        let label = anchor.label, y = anchor.frame.minY
        for edgeSwipe in [false, true] {
            if edgeSwipe {
                // Begin inside the reserved 28-point navigation edge, keeping
                // vertical displacement zero to distinguish drawer from reading.
                let start = app.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0.45)).withOffset(CGVector(dx: 8, dy: 0))
                let end = app.coordinate(withNormalizedOffset: CGVector(dx: 0.85, dy: 0.45))
                start.press(forDuration: 0.1, thenDragTo: end)
            } else { app.buttons["conversation-drawer-open"].tap() }
            let close = app.buttons["conversation-drawer-close"]
            XCTAssertTrue(close.waitForExistence(timeout: 5))
            if edgeSwipe {
                let start = app.coordinate(withNormalizedOffset: CGVector(dx: 0.8, dy: 0.45))
                let end = app.coordinate(withNormalizedOffset: CGVector(dx: 0.05, dy: 0.45))
                start.press(forDuration: 0.1, thenDragTo: end)
            } else { close.tap() }
            XCTAssertTrue(conversation.waitForExistence(timeout: 5),
                          "Drawer dismissal must restore the native transcript accessibility node")
            let retained = conversation.staticTexts[label]
            let stable = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
                retained.isHittable && abs(retained.frame.minY - y) <= 4
            }, object: nil)
            XCTAssertEqual(XCTWaiter.wait(for: [stable], timeout: 5), .completed,
                           "Opening and closing the drawer must preserve the same row at the same point offset")
            let jump = app.buttons["latest-messages"]
            XCTAssertTrue(jump.exists, "Drawer navigation must leave the reader in history")
            XCTAssertLessThanOrEqual(jump.frame.width, 56, "Jump accessibility must not absorb the viewport")
            XCTAssertLessThanOrEqual(jump.frame.height, 60)
            XCTAssertGreaterThan(conversation.staticTexts.count, 0)

        }
        capture(app, "drawer-preserves-middle-reading-anchor")
    }

    func testLongThreadKeepsPlaceAcrossUpdatesHistoryAndForeground() {
        let app = launch(["NANOCODEX_DEMO_LONG_THREAD": "1", "NANOCODEX_DEMO_HISTORY_DELAY_MS": "6000", "NANOCODEX_RENDER_COUNTER": "1"]); selectInbox(app)

        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
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
            var visibleAnchor: (label: String, y: CGFloat)?
            let materialized = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
                // Bracket a stable label and finite coordinate with pending
                // pagination. Re-reading an index-bound element after the
                // wait can resolve a recycled cell during the prepend.
                guard loading.exists,
                      let first = conversation.staticTexts.matching(
                        NSPredicate(format: "label BEGINSWITH %@", "Progress note ")
                      ).allElementsBoundByIndex.first(where: {
                        $0.frame.minY >= conversation.frame.minY && $0.isHittable
                      }) else { return false }
                let label = first.label
                let anchored = conversation.staticTexts[label]
                let frame = anchored.frame
                guard frame.minY.isFinite, frame.height > 0, anchored.isHittable, loading.exists else { return false }
                visibleAnchor = (label, frame.minY)
                return true
            }, object: nil)
            guard XCTWaiter.wait(for: [materialized], timeout: 10) == .completed,
                  let first = visibleAnchor else {
                capture(app, "history-loading-missing-anchor")
                return XCTFail("Existing messages must remain readable while older history loads: "
                    + app.staticTexts["conversation-native-scroll-state"].label + "\n" + conversation.debugDescription)
            }
            let firstLabel = first.label
            let before = first.y
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

        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertEqual(composer(app).value as? String, "Review")
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
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
        XCTAssertFalse(app.buttons["steer-now"].exists)
        // The submitted row is below the viewport and intentionally has no
        // accessibility element until its native cell is realized. Verify it
        // after scrolling below; first verify sending preserves this anchor.
        gone(app.keyboards.firstMatch)
        XCTAssertTrue(conversation.staticTexts[label].isHittable)
        XCTAssertEqual(conversation.staticTexts[label].frame.minY, y, accuracy: 4, "Sending must keep the current reading position")
        gone(app.keyboards.firstMatch)
        gone(app.staticTexts["pending-message"])
        capture(app, "25-conversation-follow-up-queued")
        let steered = conversation.staticTexts.matching(NSPredicate(format: "label == %@", submitted)).firstMatch
        for _ in 0..<12 { if steered.isHittable { break }; conversation.swipeUp(velocity: .fast) }
        XCTAssertTrue(steered.isHittable)
        XCTAssertFalse(conversation.staticTexts["Steering sent to the active turn"].exists)
        XCTAssertEqual(conversation.staticTexts.matching(NSPredicate(format: "label == %@", submitted)).count, 1)
        composer(app).tap(); composer(app).typeText("Keep this next draft")

        XCTAssertEqual(composer(app).value as? String, "Keep this next draft")
    }
    func testSwitchingTabsRestoresEarlierReadingPosition() {
        let app = launch(["NANOCODEX_DEMO_LONG_THREAD": "1", "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        selectInbox(app)
        composer(app).tap(); composer(app).typeText("Keep my place")
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        scrollVisibleConversation(app, upward: false); scrollVisibleConversation(app, upward: false)
        let latest = app.buttons["latest-messages"]
        guard latest.waitForExistence(timeout: 5), latest.isHittable else {
            return XCTFail("Scroll into earlier history before recording the reading position")
        }
        let readingTop = max(conversation.frame.minY, app.buttons["conversation-drawer-open"].frame.maxY)
        let controls = app.buttons["toggle-all-tools"]
        let readingBottom = min(composer(app).frame.minY, controls.exists ? controls.frame.minY : conversation.frame.maxY)
        let anchor = conversation.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", "Progress note "))
            .allElementsBoundByIndex.first {
                $0.isHittable && $0.frame.minY >= readingTop && $0.frame.maxY <= readingBottom
            }
        XCTAssertNotNil(anchor, "Expected a history row unobscured by the floating header and composer controls")
        guard let anchor else { return }
        let label = anchor.label, y = anchor.frame.minY
        capture(app, "tabs-before-reading-switch")
        selectTab(app, id: "data", title: "Tighten the fuel forecast")
        selectTab(app, id: "inbox", title: "Build the agent inbox")
        XCTAssertEqual(composer(app).value as? String, "Keep my place")
        let restored = conversation.staticTexts[label]
        let position = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            return restored.isHittable && abs(restored.frame.minY - y) <= 8
        }, object: restored)
        XCTAssertEqual(XCTWaiter.wait(for: [position], timeout: 5), .completed,
                       "Returning to a tab must restore the earlier message at the same reading position")
        capture(app, "tabs-restored-reading-position")
    }

    func testLiveSidebarScrollAndSwipeKeepSelection() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in phone with existing conversations")
        }
        let app = XCUIApplication(); app.launch()
        let title = self.selectedConversationTab(app)
        XCTAssertTrue(title.waitForExistence(timeout: 30))
        let selected = title.label
        let selectedID = title.identifier
        app.buttons["conversation-drawer-open"].tap()
        let list = app.descendants(matching: .any)["conversation-list"].firstMatch
        XCTAssertTrue(list.waitForExistence(timeout: 5))
        for _ in 0..<3 { list.swipeUp(velocity: .fast) }
        for _ in 0..<3 { list.swipeDown(velocity: .fast) }
        capture(app, "live-conversations-after-fast-scroll")
        app.buttons["conversation-drawer-close"].tap()
        XCTAssertEqual(app.descendants(matching: .any)["conversation"].firstMatch.label, selected, "Browsing conversations must preserve selection")
        app.buttons["conversation-drawer-open"].tap()
        XCTAssertTrue(list.waitForExistence(timeout: 5))
        app.buttons["conversation-drawer-close"].tap()
        gone(list)
        XCTAssertEqual(self.selectedConversationTab(app).identifier, selectedID)
    }

    func testLiveExistingConversationsLoadAndSwitchWithoutBlanking() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in phone with existing conversations")
        }
        let app = XCUIApplication(); app.launch()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 30))
        let selected: [(String, String)]
        if let configured = ProcessInfo.processInfo.environment["NANOCODEX_LIVE_AGENT_IDS"] {
            selected = configured.split(separator: ",").map { ("conversation-row:" + $0, "") }
        } else {
            app.buttons["conversation-drawer-open"].tap()
            let windows = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "conversation-row:"))
            XCTAssertTrue(windows.firstMatch.waitForExistence(timeout: 10))
            selected = Array(windows.allElementsBoundByIndex.prefix(3)).map { ($0.identifier, $0.label) }
        }
        XCTAssertGreaterThanOrEqual(selected.count, 2)
        for pass in 0..<2 {
            for (id, title) in selected {
                let start = Date()
                selectAgentFromDrawer(app, title: title, id: String(id.dropFirst("conversation-row:".count)))
                XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))
                let loaded = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: app.descendants(matching: .any)["conversation-loading"].firstMatch)
                XCTAssertEqual(XCTWaiter.wait(for: [loaded], timeout: 10), .completed)
                print("PHONE_CONVERSATION_OPEN pass=\(pass) seconds=\(Date().timeIntervalSince(start))")
                capture(app, "live-conversation-open-\(pass)-\(id.suffix(8))")
            }
        }
    }

    func testLiveTabSwitchesRetainUserMessagesWithReplies() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else {
            throw XCTSkip("Requires a signed-in device or simulator with an existing conversation.")
        }
        let app = XCUIApplication(); app.launch()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 30))
        navigationAction(app, "New agent").tap()
        let originalInput = "Tab cache check " + String(UUID().uuidString.prefix(8)) + ". Reply with exactly TAB_CACHE_OK."
        queue(app, originalInput)
        let completed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: self.assistantText(app, matching: NSPredicate(format: "label CONTAINS %@", "TAB_CACHE_OK")))
        XCTAssertEqual(XCTWaiter.wait(for: [completed], timeout: 90), .completed)

        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))

        let original = self.selectedConversationTab(app).label
        let selectedTab = try XCTUnwrap(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "conversation-title:")).allElementsBoundByIndex.first { $0.isSelected })
        let originalID = String(selectedTab.identifier.dropFirst("conversation-title:".count))
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 30))
        selectAgentFromDrawer(app, title: original, id: originalID)
        XCTAssertEqual(latestUserText(app).label, originalInput)
        for index in 0..<3 {
            app.buttons["conversation-drawer-open"].tap()
            let other = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@ AND identifier != %@", "conversation-row:", "conversation-row:" + originalID)).firstMatch
            XCTAssertTrue(other.waitForExistence(timeout: 5))
            other.tap()
            XCTAssertFalse(app.buttons["conversation-title:" + originalID].isSelected)
            let back = navigationAction(app, "Back")
            XCTAssertTrue(back.isEnabled)
            back.tap()
            XCTAssertTrue(app.buttons["conversation-title:" + originalID].isSelected)
            XCTAssertEqual(latestUserText(app).label, originalInput)
            XCTAssertTrue(self.assistantText(app, matching: NSPredicate(format: "label CONTAINS %@", "TAB_CACHE_OK")).exists)
            let attachment = XCTAttachment(screenshot: app.screenshot())
            attachment.name = "live-card-exchange-\(index)"; attachment.lifetime = .keepAlways; add(attachment)
        }
    }
    func testConversationScrollingAndHorizontalSwipesKeepSelectedTab() {
        let app = launch(["NANOCODEX_DEMO_LONG_PREVIEW": "1"])
        let title = self.selectedConversationTab(app).label
        let card = app.descendants(matching: .any)["conversation"].firstMatch
        card.swipeUp(); card.swipeDown()
        XCTAssertEqual(self.selectedConversationTab(app).label, title)
        for direction in [true, false, true, false] {
            let start = card.coordinate(withNormalizedOffset: CGVector(dx: direction ? 0.8 : 0.2, dy: 0.5))
            let end = card.coordinate(withNormalizedOffset: CGVector(dx: direction ? 0.2 : 0.8, dy: 0.5))
            start.press(forDuration: 0.01, thenDragTo: end)
            XCTAssertEqual(self.selectedConversationTab(app).label, title, "Horizontal conversation gestures must not navigate or dismiss agents")
            XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.exists)
        }
        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.exists)
        XCTAssertFalse(app.buttons["undo-swipe"].exists)
        capture(app, "preview-gestures-keep-selected-tab")
    }

    func testUpwardPullsDoNotCreateAgents() {
        let app = launch(["NANOCODEX_DEMO_LONG_PREVIEW": "1"])
        let original = self.selectedConversationTab(app).label
        let card = app.descendants(matching: .any)["conversation"].firstMatch
        for distance in [CGFloat(45), 140, 210] {
            let start = card.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.97))
            start.press(forDuration: 0.01, thenDragTo: start.withOffset(CGVector(dx: 0, dy: -distance)),
                withVelocity: .slow, thenHoldForDuration: 1)
            XCTAssertEqual(self.selectedConversationTab(app).label, original, "Even a deliberate upward pull only scrolls the conversation")
            XCTAssertFalse(app.otherElements["new-thread-pull-indicator"].exists)
        }
        XCTAssertTrue(app.buttons["new-conversation"].isHittable)
        capture(app, "upward-pulls-do-not-create-agents")
    }

    func testConversationTabsScaleWithAccessibilityText() {
        let app = launch(arguments: ["-UIPreferredContentSizeCategoryName", "UICTContentSizeCategoryAccessibilityXXXL"])
        XCTAssertGreaterThan(selectedConversationTab(app).frame.height, 44)
        for id in ["new-conversation", "conversation-drawer-open"] {
            XCTAssertTrue(app.descendants(matching: .any).matching(identifier: id).firstMatch.isHittable, "\(id): \(app.descendants(matching: .any).matching(identifier: id).firstMatch.debugDescription)")
        }
        selectTab(app, id: "hands", title: "Reconnect the browser Hand")
        XCTAssertEqual(selectedConversationTab(app).label, "Reconnect the browser Hand")
        XCTAssertLessThanOrEqual(selectedConversationTab(app).frame.width, app.frame.width)
        for id in ["new-conversation", "conversation-drawer-open"] {
            XCTAssertTrue(app.descendants(matching: .any).matching(identifier: id).firstMatch.isHittable, "\(id): \(app.descendants(matching: .any).matching(identifier: id).firstMatch.debugDescription)")
            XCTAssertLessThanOrEqual(app.descendants(matching: .any).matching(identifier: id).firstMatch.frame.maxY, app.frame.maxY)
        }
        // SwiftUI Menu exposes a wrapper that reports isHittable=false on
        // iOS 18 at large text sizes. Exercise the actual visible tap target.
        let menu = app.descendants(matching: .any).matching(identifier: "app-menu").firstMatch
        XCTAssertTrue(menu.exists)
        XCTAssertTrue(app.frame.contains(menu.frame))
        menu.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        XCTAssertTrue(app.buttons["Account settings"].waitForExistence(timeout: 5))
        app.buttons["Account settings"].tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5))
        app.buttons["Done"].tap()
        XCTAssertEqual(selectedConversationTab(app).label, "Reconnect the browser Hand")
        capture(app, "accessibility-text-sidebar-and-settings")
    }

    func testBrowserBackRestoresDraftAndOverviewUsesLastSentMessage() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        selectTab(app, id: "durability", title: "Make long sessions bulletproof")
        composer(app).tap(); composer(app).typeText("Retain my draft when going back")
        selectTab(app, id: "hands", title: "Reconnect the browser Hand")
        let back = navigationAction(app, "Back")
        XCTAssertTrue(back.isEnabled)
        back.tap()
        XCTAssertEqual(self.selectedConversationTab(app).label, "Make long sessions bulletproof")
        XCTAssertEqual(composer(app).value as? String, "Retain my draft when going back")
        app.buttons["new-conversation"].tap()
        XCTAssertEqual(self.selectedConversationTab(app).label, "New agent")
        navigationAction(app, "Back").tap()
        XCTAssertEqual(self.selectedConversationTab(app).label, "Make long sessions bulletproof")
        XCTAssertEqual(composer(app).value as? String, "Retain my draft when going back")
        selectTab(app, id: "hands", title: "Reconnect the browser Hand")
        queue(app, "Make this older conversation recent")
        XCTAssertTrue(app.buttons["Stop turn"].waitForExistence(timeout: 5))
        app.buttons["conversation-drawer-open"].tap()
        let cards = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "conversation-row:"))
        XCTAssertTrue(cards.firstMatch.waitForExistence(timeout: 5))
        XCTAssertEqual(cards.element(boundBy: 0).identifier, "conversation-row:hands")
        capture(app, "overview-sorted-by-last-sent-message")
        app.buttons["conversation-drawer-close"].tap()
        capture(app, "top-tabs-bottom-browser-controls")
    }

    func testTabShowsSentMessageAndEmptyRosterCanCreateAgent() {
        let app = launch(["NANOCODEX_DEMO_EMPTY_AGENTS": "1", "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        XCTAssertTrue(app.otherElements["inbox-empty"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.descendants(matching: .any)["conversation"].firstMatch.exists)
        XCTAssertTrue(app.buttons["new-conversation"].isHittable)
        app.buttons["conversation-drawer-open"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["conversation-list"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "conversation-row:")).count, 0)
        capture(app, "empty-conversation-drawer-open")
        app.buttons["drawer-new-conversation"].tap()
        XCTAssertEqual(self.selectedConversationTab(app).label, "New agent")
        XCTAssertTrue(app.otherElements["conversation-empty"].waitForExistence(timeout: 5))
        queue(app, "Remember the message I just sent")
        XCTAssertTrue(latestUserText(app).waitForExistence(timeout: 5))
        XCTAssertEqual(latestUserText(app).label, "Remember the message I just sent")
        XCTAssertTrue(latestUserText(app).isHittable)
        capture(app, "first-tab-user-message")
    }

    func testTabDockStaysAboveKeyboardAndCreatesIndependentDraft() {
        let app = launch(["NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        let original = self.selectedConversationTab(app).label
        composer(app).tap(); composer(app).typeText("Keep my keyboard draft")
        let keyboard = app.keyboards.firstMatch
        XCTAssertTrue(keyboard.waitForExistence(timeout: 5))
        for id in ["new-conversation", "conversation-drawer-open", "send"] {
            let button = app.buttons[id]
            XCTAssertTrue(button.isHittable)
            XCTAssertLessThanOrEqual(button.frame.maxY, keyboard.frame.minY + 1)
        }
        capture(app, "tab-dock-above-keyboard")
        app.buttons["new-conversation"].tap()
        XCTAssertEqual(self.selectedConversationTab(app).label, "New agent")
        XCTAssertNotEqual(composer(app).value as? String, "Keep my keyboard draft")
        selectAgentFromDrawer(app, title: original)
        XCTAssertEqual(composer(app).value as? String, "Keep my keyboard draft")
    }

    func testCreateStopKeepsAgentsInSingleDrawerList() {
        let app = launch()
        app.buttons["new-conversation"].tap()
        XCTAssertEqual(self.selectedConversationTab(app).label, "New agent")
        XCTAssertFalse(app.buttons["send"].isEnabled)
        queue(app, "Start a checklist")
        gone(app.staticTexts["pending-message"])
        XCTAssertTrue(app.buttons["Stop turn"].isEnabled)
        app.buttons["Stop turn"].tap()
        let stopped = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@ AND enabled == false", "Send message"), object: app.buttons["send"])
        XCTAssertEqual(XCTWaiter.wait(for: [stopped], timeout: 5), .completed)
        for (id, title) in [("inbox", "Build the agent inbox"), ("data", "Tighten the fuel forecast")] {
            selectTab(app, id: id, title: title)
            app.buttons["Stop turn"].tap()
            XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: NSPredicate(format: "value == %@", "Stopped"), object: app.buttons["conversation-title:" + id])], timeout: 5), .completed)
        }
        let selected = self.selectedConversationTab(app).label
        app.buttons["conversation-drawer-open"].tap()
        XCTAssertEqual(app.segmentedControls.count, 0)
        XCTAssertTrue((app.buttons["conversation-row:inbox"].value as? String ?? "").contains("Stopped"))
        capture(app, "stopped-agents-remain-in-drawer")
        XCTAssertTrue(app.buttons["conversation-row:inbox"].waitForExistence(timeout: 5))
        app.buttons["conversation-drawer-close"].tap()
        XCTAssertEqual(self.selectedConversationTab(app).label, selected)

    }

    func testNewConversationOpensAndSendsDuringSlowCreation() {
        let app = launch(["NANOCODEX_DEMO_CREATE_DELAY_MS": "20000"])
        app.buttons["new-conversation"].tap()
        XCTAssertEqual(self.selectedConversationTab(app).label, "New agent")
        XCTAssertTrue(composer(app).isEnabled)
        queue(app, "A message before creation finishes")
        XCTAssertEqual(latestUserText(app).label, "A message before creation finishes")
        XCTAssertTrue(latestUserText(app).isHittable)
        XCTAssertFalse(app.scrollViews["pending-messages"].exists)
        XCTAssertFalse(app.staticTexts["Waiting to start"].exists)
        composer(app).tap(); composer(app).typeText("Keep my next draft")
        let admitted = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.buttons["send"].isEnabled
                && self.selectedConversationTab(app).value as? String == "Running"
                && !self.selectedConversationTab(app).identifier.hasPrefix("conversation-title:draft-")
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [admitted], timeout: 25), .completed)
        XCTAssertEqual(latestUserText(app).label, "A message before creation finishes")
        XCTAssertEqual(app.descendants(matching: .any)["conversation"].firstMatch.staticTexts.matching(NSPredicate(format: "label == %@", "A message before creation finishes")).count, 1)
        XCTAssertEqual(composer(app).value as? String, "Keep my next draft")
        XCTAssertTrue(app.keyboards.firstMatch.exists, "Creation must keep the active composer focused.")
        capture(app, "instant-conversation-send-and-draft")
    }
    func testFirstMessageRemainsVisibleThroughDelayedFailureAndRetry() {
        let app = launch(["NANOCODEX_DEMO_DELAY_MS": "4000", "NANOCODEX_DEMO_FAIL_ONCE": "submit", "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        app.buttons["new-conversation"].tap()
        queue(app, "Keep this bubble through delivery retry")
        let message = app.descendants(matching: .any)["conversation"].firstMatch.staticTexts["Keep this bubble through delivery retry"]
        XCTAssertTrue(message.isHittable, "Sending renders locally before the delayed API response")
        XCTAssertFalse(app.scrollViews["pending-messages"].exists)
        XCTAssertTrue(app.buttons["retry-pending"].waitForExistence(timeout: 12))
        XCTAssertTrue(message.isHittable)
        capture(app, "instant-send-retry-in-place")
        app.buttons["retry-pending"].tap()
        XCTAssertTrue(message.isHittable)
        gone(app.buttons["retry-pending"])
        XCTAssertEqual(app.descendants(matching: .any)["conversation"].firstMatch.staticTexts.matching(NSPredicate(format: "label == %@", "Keep this bubble through delivery retry")).count, 1)
    }
    func testLateCreationDoesNotNavigateAwayFromAnotherConversation() {
        let app = launch(["NANOCODEX_DEMO_CREATE_DELAY_MS": "5000"])
        let original = self.selectedConversationTab(app).label
        navigationAction(app, "New agent").tap()
        selectAgentFromDrawer(app, title: original)
        composer(app).tap(); composer(app).typeText("Keep this conversation selected")
        Thread.sleep(forTimeInterval: 6)
        XCTAssertEqual(self.selectedConversationTab(app).label, original)
        XCTAssertEqual(composer(app).value as? String, "Keep this conversation selected")
        capture(app, "instant-conversation-keeps-selection")
    }
    func testNewConversationCreationFailureRetainsDraftAndRetries() {
        let app = launch(["NANOCODEX_DEMO_CREATE_DELAY_MS": "1500", "NANOCODEX_DEMO_FAIL_ONCE": "create"])
        navigationAction(app, "New agent").tap()
        composer(app).tap(); composer(app).typeText("Keep my draft through retry")
        XCTAssertTrue(app.buttons["retry-creation"].waitForExistence(timeout: 5))
        XCTAssertEqual(composer(app).value as? String, "Keep my draft through retry")
        app.buttons["retry-creation"].tap()
        gone(app.buttons["retry-creation"])
        Thread.sleep(forTimeInterval: 2)
        XCTAssertEqual(composer(app).value as? String, "Keep my draft through retry")
        app.buttons["send"].tap()
        XCTAssertEqual(latestUserText(app).label, "Keep my draft through retry")
        capture(app, "instant-conversation-retry")
    }
    func testCancelBeforeCreationDoesNotSendCancelledMessage() {
        let app = launch(["NANOCODEX_DEMO_CREATE_DELAY_MS": "20000"])
        navigationAction(app, "New agent").tap()
        queue(app, "Do not send this message")
        app.buttons["Stop turn"].tap()
        queue(app, "Send only this replacement")
        let admitted = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.selectedConversationTab(app).value as? String == "Running"
                && !self.selectedConversationTab(app).identifier.hasPrefix("conversation-title:draft-")
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [admitted], timeout: 25), .completed)
        XCTAssertEqual(latestUserText(app).label, "Send only this replacement")

        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["Do not send this message"].exists)
        XCTAssertTrue(app.staticTexts["Send only this replacement"].exists)
        capture(app, "instant-conversation-cancel-before-creation")
    }
    func testUnfinishedConversationDraftSurvivesRelaunch() {
        let app = launch(["NANOCODEX_DEMO_CREATE_DELAY_MS": "60000", "NANOCODEX_DEMO_PROFILE": UUID().uuidString])
        navigationAction(app, "New agent").tap()
        composer(app).tap(); composer(app).typeText("Keep this unfinished conversation")
        app.terminate()
        app.launchEnvironment["NANOCODEX_DEMO_CREATE_DELAY_MS"] = "0"
        app.launch()
        selectAgentFromDrawer(app, title: "New agent")
        XCTAssertEqual(composer(app).value as? String, "Keep this unfinished conversation")
        capture(app, "instant-conversation-restored-draft")
    }
    func testInvalidAccountAndReturnToDemo() {
        let app = launch()
        navigationAction(app, "Account settings").tap()
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
        XCTAssertTrue(self.selectedConversationTab(app).waitForExistence(timeout: 5))
    }
    func testPhoneNumberUsesDeviceRegionAndHonorsInternationalPaste() {
        let app = XCUIApplication()
        app.launchArguments = ["--demo", "-AppleLocale", "en_GR", "-AppleLanguages", "(en)"]
        app.launch()
        XCTAssertTrue(self.selectedConversationTab(app).waitForExistence(timeout: 10))
        navigationAction(app, "Account settings").tap(); app.buttons["Connect account"].tap()
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
    func testMultipleDirectMessagesRemainInTranscriptWithKeyboard() {
        let app = launch(); selectInbox(app)
        for index in 1...4 {
            queue(app, "Synthetic instruction \(index)")
            XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.staticTexts["Synthetic instruction \(index)"].waitForExistence(timeout: 5))
        }
        composer(app).tap(); composer(app).typeText("A longer draft\nthat spans several lines\nand stays above the keyboard.")
        XCTAssertLessThanOrEqual(app.buttons["send"].frame.maxY, app.keyboards.firstMatch.frame.minY)
        XCTAssertFalse(app.buttons["steer-now"].exists)
        XCTAssertFalse(app.scrollViews["pending-messages"].exists)
        capture(app, "direct-messages-with-keyboard")
    }
    func testLiveEdgeSwipeKeepsSelectedConversation() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_LIVE"] == "1" else {
            throw XCTSkip("Requires an authorized signed-in phone")
        }
        let app = XCUIApplication(); app.launch()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 30))
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 15))
        gone(app.descendants(matching: .any)["conversation-loading"].firstMatch, timeout: 30)
        let original = selectedConversationTab(app).identifier
        let draft = composer(app).value as? String
        for _ in 0..<3 {
            openDrawerFromEdge(app)
            XCTAssertTrue(app.descendants(matching: .any)["conversation-list"].firstMatch.isHittable)
            capture(app, "phone-edge-swipe-open")
            app.buttons["conversation-drawer-close"].tap()
            gone(app.descendants(matching: .any)["conversation-list"].firstMatch)
            XCTAssertEqual(selectedConversationTab(app).identifier, original)
            XCTAssertEqual(composer(app).value as? String, draft)
        }
    }

    func testPerformanceCurrentSessionDrawerAndSheets() throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["NANOCODEX_INBOX_PERFORMANCE"] == "1", environment["NANOCODEX_INBOX_LIVE"] == "1" else {
            throw XCTSkip("Requires an explicitly authorized signed-in phone and both performance/live flags.")
        }
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 30))
        if let agentID = environment["NANOCODEX_PERFORMANCE_AGENT_ID"], !agentID.isEmpty {
            selectAgentFromDrawer(app, title: "", id: agentID)
        }
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 15))
        gone(app.descendants(matching: .any)["conversation-loading"].firstMatch, timeout: 30)
        let original = selectedConversationTab(app).identifier
        let originalDraft = composer(app).value as? String
        capture(app, "device-muse-conversation")
        let options = XCTMeasureOptions()
        options.iterationCount = 3
        var metrics: [XCTMetric] = [XCTCPUMetric(application: app), XCTMemoryMetric(application: app)]
        if #available(iOS 26.0, *) { metrics.append(XCTHitchMetric(application: app)) }
        measure(metrics: metrics, options: options) {
            openDrawerFromEdge(app)
            XCTAssertTrue(app.descendants(matching: .any)["conversation-list"].firstMatch.waitForExistence(timeout: 5))
            app.buttons["conversation-drawer-close"].tap()
            gone(app.descendants(matching: .any)["conversation-list"].firstMatch)
            XCTAssertEqual(selectedConversationTab(app).identifier, original)
            app.buttons["add-attachments"].tap()
            XCTAssertTrue(app.buttons["choose-photos"].waitForExistence(timeout: 5))
            app.buttons["Done"].tap()
            navigationAction(app, "Account settings").tap()
            XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5))
            app.buttons["Done"].tap()
            XCTAssertEqual(composer(app).value as? String, originalDraft)
        }
        app.buttons["conversation-drawer-open"].tap()
        capture(app, "device-muse-drawer")
        app.buttons["conversation-drawer-close"].tap()
        XCTAssertEqual(selectedConversationTab(app).identifier, original)
        XCTAssertEqual(composer(app).value as? String, originalDraft)
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
            XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 20), "Restore the saved account after each launch")
            stopMeasuring()
            XCTAssertFalse(app.textFields["phone-number"].exists)
            XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "identifier == %@ AND label == %@", "connection", "Demo")).firstMatch.exists)
        }
        app.terminate()
    }

    func testNativeTranscriptBoundsMountedCellsFor500Rows() {
        verifyNativeTranscriptMountedCells(rowCount: 500)
    }

    func testNativeTranscriptBoundsMountedCellsFor2000Rows() {
        verifyNativeTranscriptMountedCells(rowCount: 2_000)
    }

    private func verifyNativeTranscriptMountedCells(rowCount: Int) {
        let app = launch(["NANOCODEX_DEMO_RENDER_PROFILE": "1",
                          "NANOCODEX_DEMO_PROFILE": "native-mounts-" + UUID().uuidString,
                          "NANOCODEX_DEMO_RENDER_ROWS": String(rowCount),
                          "NANOCODEX_RENDER_COUNTER": "1"])
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        let counter = app.staticTexts["conversation-native-mounted-count"]
        XCTAssertTrue(conversation.waitForExistence(timeout: 10))
        XCTAssertTrue(counter.waitForExistence(timeout: 10))
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            counter.value as? String == String(rowCount) && (Int(counter.label) ?? 0) > 0
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 10), .completed)
        func assertBoundedHosts() {
            XCTAssertEqual(counter.value as? String, String(rowCount), "Keep the complete synthetic history in the data source")
            guard let mounted = Int(counter.label) else { return XCTFail("Expected actual native host count, got \(counter.label)") }
            XCTAssertGreaterThan(mounted, 0)
            // The same generous ceiling applies to both data sizes and counts
            // actual retained cell hosts, including any native reuse pool.
            XCTAssertLessThanOrEqual(mounted, 64, "Native hosted views must stay bounded independently of retained history")
        }
        assertBoundedHosts()
        for _ in 0..<12 {
            conversation.swipeDown()
            assertBoundedHosts()
        }
        let latest = app.buttons["latest-messages"]
        XCTAssertTrue(latest.isHittable, "Exercise real scrolling away from the tail")
        capture(app, "native-\(rowCount)-rows-reading")
        latest.tap()
        gone(latest)
        assertBoundedHosts()
        capture(app, "native-\(rowCount)-rows-tail")
    }

    func testNativeTranscriptPreservesExpandedToolAfterCellRecycling() {
        let app = launch(["NANOCODEX_DEMO_RENDER_PROFILE": "1",
                          "NANOCODEX_DEMO_RENDER_ROWS": "500",
                          "NANOCODEX_DEMO_RENDER_TOOL": "1",
                          "NANOCODEX_DEMO_PROFILE": "native-recycling-" + UUID().uuidString,
                          "NANOCODEX_RENDER_COUNTER": "1"])
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        let tool = conversation.buttons["tool-disclosure-profile-recycling-tool"]
        XCTAssertTrue(tool.waitForExistence(timeout: 10))
        XCTAssertTrue(tool.isHittable)
        XCTAssertEqual(tool.value as? String, "Collapsed")
        tool.tap()
        XCTAssertEqual(tool.value as? String, "Expanded")
        let output = conversation.staticTexts["Recycled tool output remains expanded."]
        XCTAssertTrue(output.waitForExistence(timeout: 5))
        for _ in 0..<12 { conversation.swipeDown() }
        XCTAssertFalse(tool.exists, "The expanded tool must leave the materialized accessibility tree before restoration")
        XCTAssertFalse(output.exists)
        let latest = app.buttons["latest-messages"]
        XCTAssertTrue(latest.isHittable)
        latest.tap()
        XCTAssertTrue(tool.waitForExistence(timeout: 10))
        XCTAssertEqual(tool.value as? String, "Expanded", "Disclosure state belongs to the conversation, not a recycled native cell")
        XCTAssertTrue(output.waitForExistence(timeout: 5))
        capture(app, "native-recycled-tool-expanded")
    }

    func testDraftTypingDoesNotRebuildConversationProjection() {
        let app = launch(["NANOCODEX_DEMO_RENDER_PROFILE": "1",
                          "NANOCODEX_DEMO_PROFILE": "projection-" + UUID().uuidString,
                          "NANOCODEX_RENDER_COUNTER": "1"])
        let counter = app.staticTexts["conversation-projection-count"]
        XCTAssertTrue(counter.waitForExistence(timeout: 5))
        let prepared = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in (UInt64(counter.label) ?? 0) > 0 }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [prepared], timeout: 5), .completed)
        XCTAssertTrue(app.descendants(matching: .any)["conversation"].firstMatch.staticTexts.matching(
            NSPredicate(format: "label BEGINSWITH %@", "Review note ")).firstMatch.waitForExistence(timeout: 5))
        composer(app).tap()
        let initial = counter.label
        XCTAssertNotNil(UInt64(initial))
        composer(app).typeText("Keep this draft while reviewing the conversation.")
        XCTAssertEqual(counter.label, initial, "Draft edits must reuse the revision-keyed render projection")
        XCTAssertEqual(composer(app).value as? String, "Keep this draft while reviewing the conversation.")
        app.buttons["send"].tap()
        let invalidated = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in (UInt64(counter.label) ?? 0) > (UInt64(initial) ?? 0) },
            object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [invalidated], timeout: 5), .completed,
                       "A submitted message must invalidate the render projection")
    }

    // Synthetic, opt-in measurements. XCTest interaction time includes driver
    // waits; these are simulator diagnostics, not physical-device FPS claims.
    func testPerformanceChatTimelineScrolling() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_PERFORMANCE"] == "1" else {
            throw XCTSkip("Opt-in synthetic chat profiling")
        }
        let app = launch(["NANOCODEX_DEMO_RENDER_PROFILE": "1",
                          "NANOCODEX_DEMO_RENDER_ROWS": "500",
                          "NANOCODEX_RENDER_COUNTER": "1"])
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 10))
        let options = XCTMeasureOptions()
        options.iterationCount = 3
        var metrics: [XCTMetric] = [XCTClockMetric(), XCTCPUMetric(application: app), XCTMemoryMetric(application: app)]
        if #available(iOS 26.0, *) { metrics.append(XCTHitchMetric(application: app)) }
        measure(metrics: metrics, options: options) {
            for _ in 0..<6 { conversation.swipeDown() }
            let latest = app.buttons["latest-messages"]
            XCTAssertTrue(latest.isHittable)
            latest.tap()
            gone(latest)
            let counter = app.staticTexts["conversation-native-mounted-count"]
            XCTAssertEqual(counter.value as? String, "500")
            let mounted = Int(counter.label) ?? 0
            XCTAssertGreaterThan(mounted, 0)
            XCTAssertLessThanOrEqual(mounted, 64)
            print("PROFILE_CHAT_MOUNTED \(mounted)/500")
        }
        capture(app, "profile-500-message-scroll")
    }

    func testPerformanceStreamingMarkdownAndTools() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_PERFORMANCE"] == "1" else {
            throw XCTSkip("Opt-in synthetic chat profiling")
        }
        let app = launch(["NANOCODEX_DEMO_RICH_STREAM": "1",
                          "NANOCODEX_DEMO_STREAMING_GROWTH": "1",
                          "NANOCODEX_DEMO_STREAM_INTERVAL_MS": "1000"])
        selectInbox(app)
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        let tool = conversation.buttons["tool-disclosure-demo-rich-tool"]
        XCTAssertTrue(tool.waitForExistence(timeout: 10))
        let options = XCTMeasureOptions()
        options.iterationCount = 3
        var metrics: [XCTMetric] = [XCTClockMetric(), XCTCPUMetric(application: app), XCTMemoryMetric(application: app)]
        if #available(iOS 26.0, *) { metrics.append(XCTHitchMetric(application: app)) }
        // Sequential windows over a growing document, not identical replays.
        // Keep one process alive so XCTest can harvest its CPU counters.
        measure(metrics: metrics, options: options) {
            for _ in 0..<8 { if tool.isHittable { break }; conversation.swipeDown() }
            XCTAssertTrue(tool.isHittable)
            tool.tap()
            let latest = app.buttons["latest-messages"]
            if latest.isHittable { latest.tap() }
            Thread.sleep(forTimeInterval: 5)
        }
        for _ in 0..<8 { if tool.isHittable { break }; conversation.swipeDown() }
        XCTAssertTrue(tool.isHittable)
        XCTAssertTrue(tool.label.contains("Completed"))
        capture(app, "profile-streaming-markdown-and-tools")
    }

    func testPerformanceDemoConversationRendering() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_INBOX_PERFORMANCE"] == "1" else {
            throw XCTSkip("Opt-in deterministic rendering profile; does not use a saved account.")
        }
        let app = launch(["NANOCODEX_DEMO_RENDER_PROFILE": "1", "NANOCODEX_DEMO_PROFILE": "render-audit-" + UUID().uuidString])

        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
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
        XCTAssertTrue(app.buttons["conversation-drawer-open"].waitForExistence(timeout: 20))
        defer { app.terminate() }
        // App-target metrics capture this process; keep it alive for every iteration.
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        func waitForConversation() {
            let cardTitle = self.selectedConversationTab(app)
            XCTAssertTrue(cardTitle.waitForExistence(timeout: 10))
            let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@ AND hittable == true", title), object: cardTitle)
            XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 10), .completed)
        }
        func browseAgent() {
            selectAgentFromDrawer(app, title: title)
            waitForConversation()
        }
        func inspectOverview() {
            app.buttons["conversation-drawer-open"].tap()
            let overview = app.descendants(matching: .any)["conversation-list"].firstMatch
            XCTAssertTrue(overview.waitForExistence(timeout: 5))
            app.buttons["conversation-drawer-close"].tap()
            gone(overview)
            waitForConversation()
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
            XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "identifier == %@ AND label == %@", "connection", "Demo")).firstMatch.exists)
            browseAgent()

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
            inspectOverview()
            XCTAssertTrue(conversation.exists)

            XCTAssertTrue(conversation.waitForExistence(timeout: 10))
            inspectOverview()
            browseAgent()

            XCTAssertTrue(conversation.waitForExistence(timeout: 10))
            stopMeasuring()

            // Restore pre-existing local input outside the measured interval.
            // Automation wall time is not a UI-response latency metric.
            if draftText() != originalDraft { replaceDraft(originalDraft) }
            XCTAssertEqual(draftText(), originalDraft)
        }
    }

    /// Run via apple/scripts/test-meetings.sh: meetings travel over real HTTP
    /// into the production Worker router and persistent D1, not URLProtocol JSON.
    func testMeetingsPersistAndSyncNativeNotes() throws {
        let app = XCUIApplication()
        let profile = "meetings-" + UUID().uuidString.lowercased()
        app.launchEnvironment["NANOCODEX_STARTUP_FIXTURE"] = "1"
        app.launchEnvironment["NANOCODEX_STARTUP_PROFILE"] = profile
        app.launchEnvironment["NANOCODEX_MEETING_JOURNEY_ORIGIN"] = ProcessInfo.processInfo.environment["NANOCODEX_MEETING_JOURNEY_ORIGIN"] ?? "http://127.0.0.1:8797"
        app.launch()
        XCTAssertTrue(app.buttons["main-tab-meetings"].waitForExistence(timeout: 30))
        app.buttons["main-tab-meetings"].tap()
        XCTAssertTrue(app.buttons["meeting-new"].waitForExistence(timeout: 10))
        app.buttons["meeting-new"].tap()
        let title = "Native planning " + String(UUID().uuidString.prefix(8))
        let titleField = app.textFields["meeting-title"].exists ? app.textFields["meeting-title"] : app.textViews["meeting-title"]
        XCTAssertTrue(titleField.waitForExistence(timeout: 5))
        // A vertical SwiftUI TextField can place the caret before its default
        // title on a center tap. Explicitly tap the trailing edge before deleting.
        titleField.coordinate(withNormalizedOffset: CGVector(dx: 0.95, dy: 0.5)).tap()
        let prior = titleField.value as? String ?? ""
        titleField.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: prior.count) + title)
        XCTAssertEqual(titleField.value as? String, title)
        // Dismiss the title keyboard before tapping the note editor, which may
        // extend underneath it on a compact phone.
        app.buttons["meeting-keyboard-done"].tap()
        let notes = app.textViews["meeting-notes"]
        notes.coordinate(withNormalizedOffset: CGVector(dx: 0.1, dy: 0.1)).tap()
        notes.typeText("Decision: ship the native meeting library first. Morgan sends the draft on Friday.")
        app.buttons["meeting-keyboard-done"].tap()
        let save = app.buttons["meeting-save"]
        for _ in 0..<5 where !save.isHittable { app.swipeUp() }
        XCTAssertTrue(save.isHittable)
        capture(app, "meetings-native-capture-draft")
        save.tap()
        let row = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'meeting-row-' AND label BEGINSWITH %@", title + ",")).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 15))
        capture(app, "meetings-native-library")
        row.tap()
        XCTAssertTrue(app.textViews["meeting-document-notes"].waitForExistence(timeout: 10))
        XCTAssertTrue((app.textViews["meeting-document-notes"].value as? String ?? "").contains("Morgan sends"))
        app.segmentedControls["meeting-document-tabs"].buttons["Transcript"].tap()
        let transcript = app.textViews["meeting-document-transcript"]
        XCTAssertTrue(transcript.waitForExistence(timeout: 5))
        transcript.tap(); transcript.typeText("Morgan: I will send the draft Friday. Taylor: We agreed to ship the native meeting library first.")
        app.buttons["meeting-keyboard-done"].tap()
        let saveChanges = app.buttons["meeting-document-save"]
        for _ in 0..<5 where !saveChanges.isHittable { app.swipeUp() }
        XCTAssertTrue(saveChanges.isHittable); saveChanges.tap()
        XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: saveChanges)], timeout: 15), .completed)
        for _ in 0..<5 where !app.segmentedControls["meeting-document-tabs"].isHittable { app.swipeDown() }
        app.segmentedControls["meeting-document-tabs"].buttons["Notes"].tap()
        let enhance = app.buttons["meeting-enhance"]
        XCTAssertTrue(enhance.waitForExistence(timeout: 5))
        enhance.tap()
        XCTAssertTrue(app.descendants(matching: .any)["meeting-enhanced-notes"].firstMatch.waitForExistence(timeout: 30))
        capture(app, "meetings-native-enhanced-notes")
        // A new app process reopens the protected SQLite journal and re-fetches
        // the same cloud document. A UI sheet is never the data's lifetime.
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["main-tab-meetings"].waitForExistence(timeout: 30))
        app.buttons["main-tab-meetings"].tap()
        XCTAssertTrue(row.waitForExistence(timeout: 15)); row.tap()
        XCTAssertTrue(app.textViews["meeting-document-notes"].waitForExistence(timeout: 10))
        app.segmentedControls["meeting-document-tabs"].buttons["Transcript"].tap()
        XCTAssertTrue((app.textViews["meeting-document-transcript"].value as? String ?? "").contains("Morgan: I will send"))
        capture(app, "meetings-native-restored-transcript")
        app.buttons["meeting-document-menu"].tap()
        app.buttons["Delete meeting"].firstMatch.tap()
        app.buttons["Delete meeting"].firstMatch.tap()
        XCTAssertTrue(app.buttons["meeting-new"].waitForExistence(timeout: 10))
        XCTAssertFalse(row.exists)
        capture(app, "meetings-native-deleted")
    }

    func testGlobalComposerAndSelectorClearOtherInputKeyboards() {
        let app = XCUIApplication()
        app.launchEnvironment = ["NANOCODEX_STARTUP_FIXTURE": "1", "NANOCODEX_STARTUP_PROFILE": UUID().uuidString.lowercased()]
        app.launch()
        XCTAssertTrue(app.buttons["main-tab-todo"].waitForExistence(timeout: 20))
        assertUnifiedInbox(app)
        selectInboxScope(app, "Mail")
        selectInboxScope(app, "Inbox")
        XCTAssertFalse(app.descendants(matching: .any)["todo-capture"].firstMatch.exists,
                       "Prepare a thought is an explicit sheet, not the always-new-thread bottom composer")
        XCTAssertTrue(app.buttons["new-thread-send"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["new-thread-send"].isEnabled)
        func assertChrome(_ name: String) {
            let keyboard = app.keyboards.firstMatch
            XCTAssertTrue(keyboard.waitForExistence(timeout: 5))
            let editor = composerJourneyField(app, identifier: "new-thread-composer")
            XCTAssertTrue(editor.isHittable)
            XCTAssertLessThanOrEqual(editor.frame.maxY, keyboard.frame.minY + 1)
            for id in ["main-tab-todo", "main-tab-chat", "main-tab-crm", "main-tab-meetings", "main-tab-apps"] {
                let button = app.buttons[id]
                XCTAssertTrue(button.exists); XCTAssertTrue(button.isHittable)
                XCTAssertLessThanOrEqual(button.frame.maxY, keyboard.frame.minY + 1)
                XCTAssertGreaterThanOrEqual(button.frame.minY, editor.frame.maxY - 1)
            }
            capture(app, name)
        }
        app.buttons["todo-search-open"].tap()
        let inboxSearch = app.textFields["todo-search"]
        XCTAssertTrue(inboxSearch.waitForExistence(timeout: 5))
        inboxSearch.tap(); inboxSearch.typeText("launch")
        assertChrome("global-chrome-above-inbox-search-keyboard")
        app.buttons["main-tab-crm"].tap()
        let crmSearch = app.textFields["crm-search"]
        XCTAssertTrue(crmSearch.waitForExistence(timeout: 5))
        crmSearch.tap(); crmSearch.typeText("Alex")
        assertChrome("global-chrome-above-crm-search-keyboard")
        let draft = composerJourneyField(app, identifier: "new-thread-composer")
        draft.tap(); draft.typeText("Unsent global input")
        assertChrome("global-chrome-above-new-thread-keyboard")
        XCTAssertTrue(app.buttons["crm-record-alex"].exists, "Typing in the composer must not filter or leave the directory")
        XCTAssertFalse(app.buttons["conversation-title:composer-agent-1"].exists, "Typing must not create a thread")
        app.buttons["main-tab-todo"].tap()
        assertUnifiedInbox(app)
        let inboxDraft = composerJourneyField(app, identifier: "new-thread-composer")
        XCTAssertEqual(inboxDraft.value as? String, "Unsent global input")
        // Search visibility is local view state; the query belongs to the model.
        let restoredSearch = app.textFields["todo-search"]
        if !restoredSearch.exists { app.buttons["todo-search-open"].tap() }
        XCTAssertTrue(restoredSearch.waitForExistence(timeout: 5))
        XCTAssertEqual(restoredSearch.value as? String, "launch",
                       "The unified Inbox query is independent of the global thread draft")
        // UIKit owns the tap-selected caret; explicitly select all before editing.
        inboxDraft.tap()
        inboxDraft.typeKey("a", modifierFlags: .command)
        inboxDraft.typeText("Unsent global input from Inbox")
        assertChrome("global-chrome-above-unified-inbox-composer-keyboard")
        XCTAssertEqual(inboxDraft.value as? String, "Unsent global input from Inbox")
        XCTAssertEqual(app.textFields["todo-search"].value as? String, "launch")
        XCTAssertTrue(app.buttons["new-thread-send"].isEnabled)
        XCTAssertFalse(app.descendants(matching: .any)["todo-capture"].firstMatch.exists,
                       "Typing in Inbox must not open or submit thought preparation")
        XCTAssertFalse(app.buttons["conversation-title:composer-agent-1"].exists, "An unsent Inbox draft must not create a thread")
    }

    private func launchComposerTransportJourney(failCreationOnce: Bool = false) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment = [
            "NANOCODEX_STARTUP_FIXTURE": "1",
            "NANOCODEX_STARTUP_PROFILE": UUID().uuidString.lowercased(),
            "NANOCODEX_STARTUP_COMPOSER_JOURNEY": "1",
            "NANOCODEX_STARTUP_COMPOSER_CREATE_FAIL_ONCE": failCreationOnce ? "1" : "0"
        ]
        app.launch()
        XCTAssertTrue(app.buttons["main-tab-crm"].waitForExistence(timeout: 20))
        app.buttons["main-tab-crm"].tap()
        XCTAssertTrue(app.buttons["crm-record-alex"].waitForExistence(timeout: 10))
        app.buttons["crm-record-alex"].tap()
        XCTAssertTrue(app.staticTexts["Example University"].waitForExistence(timeout: 10))
        return app
    }

    private func composerJourneyField(_ app: XCUIApplication, identifier: String) -> XCUIElement {
        let field = app.textFields[identifier]
        if field.waitForExistence(timeout: 3) { return field }
        let editor = app.textViews[identifier]
        XCTAssertTrue(editor.waitForExistence(timeout: 5), "Expected the production native composer: \(identifier)")
        return editor
    }

    private func composerRecordedRequests(_ app: XCUIApplication, creates: Int, turns: Int) -> [[String: Any]] {
        let prefix = "Composer fixture transport ledger: "
        let rows = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", prefix))
        for row in rows.allElementsBoundByIndex.reversed() {
            let text = row.label
            guard text.hasPrefix(prefix), let bytes = String(text.dropFirst(prefix.count)).data(using: .utf8),
                  let entries = (try? JSONSerialization.jsonObject(with: bytes)) as? [[String: Any]] else { continue }
            let creationCount = entries.filter { $0["path"] as? String == "/v1/agents" }.count
            let turnCount = entries.filter { ($0["path"] as? String ?? "").hasSuffix("/turns") }.count
            if creationCount == creates && turnCount == turns { return entries }
        }
        return []
    }

    private func awaitComposerRecordedRequests(_ app: XCUIApplication, creates: Int, turns: Int,
                                               selectedAgent: String = "composer-agent-1",
                                               file: StaticString = #filePath, line: UInt = #line) -> [[String: Any]] {
        let recorded = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            !self.composerRecordedRequests(app, creates: creates, turns: turns).isEmpty
        }, object: app)
        XCTAssertEqual(XCTWaiter.wait(for: [recorded], timeout: 15), .completed,
                       "Await external JSONL POST records, not just local UI labels/bubbles.", file: file, line: line)
        let entries = composerRecordedRequests(app, creates: creates, turns: turns)
        XCTAssertEqual(entries.count, creates + turns, file: file, line: line)
        for entry in entries {
            XCTAssertEqual(entry["method"] as? String, "POST", file: file, line: line)
            XCTAssertFalse((entry["idempotency"] as? String ?? "").isEmpty, file: file, line: line)
            if (entry["path"] as? String ?? "").hasSuffix("/turns") {
                XCTAssertEqual(entry["idempotency"] as? String, "inbox:" + (entry["id"] as? String ?? ""), file: file, line: line)
            }
        }
        XCTAssertTrue(app.buttons["conversation-title:" + selectedAgent].exists, file: file, line: line)
        XCTAssertTrue(app.buttons["conversation-title:" + selectedAgent].isSelected, file: file, line: line)
        XCTAssertTrue(app.buttons["send"].exists, "Non-Chat Send must switch to the ordinary Chat composer", file: file, line: line)
        XCTAssertFalse(app.buttons["new-thread-send"].exists, file: file, line: line)
        return entries
    }

    func testCRMGlobalSendCreatesOneAgentThenChatReusesItsTransportIdentity() {
        let app = launchComposerTransportJourney()
        let first = "Synthetic CRM composer first prompt"
        let reply = "Synthetic Chat same agent follow-up"
        let draft = composerJourneyField(app, identifier: "new-thread-composer")
        draft.tap(); draft.typeText(first)
        XCTAssertTrue(app.staticTexts["Example University"].exists, "Typing must not navigate away from the CRM person")
        XCTAssertTrue(app.buttons["new-thread-send"].isEnabled)
        app.buttons["new-thread-send"].tap()
        let initial = awaitComposerRecordedRequests(app, creates: 1, turns: 1)
        let firstTurn = initial.first { ($0["path"] as? String ?? "").hasSuffix("/turns") }
        XCTAssertEqual(firstTurn?["path"] as? String, "/v1/agents/composer-agent-1/turns")
        XCTAssertEqual(firstTurn?["input"] as? String, first)
        capture(app, "crm-global-send-new-agent-external-recorder")

        let chatDraft = composerJourneyField(app, identifier: "composer")
        chatDraft.tap(); chatDraft.typeText(reply)
        XCTAssertTrue(app.buttons["send"].isEnabled)
        app.buttons["send"].tap()
        let afterReply = awaitComposerRecordedRequests(app, creates: 1, turns: 2)
        let firstThreadTurns = afterReply.filter { ($0["path"] as? String ?? "").hasSuffix("/turns") }
        XCTAssertEqual(firstThreadTurns.map { $0["path"] as? String ?? "" },
                       ["/v1/agents/composer-agent-1/turns", "/v1/agents/composer-agent-1/turns"])
        XCTAssertEqual(firstThreadTurns.map { $0["input"] as? String ?? "" }, [first, reply])
        XCTAssertEqual(Set(firstThreadTurns.map { $0["id"] as? String ?? "" }).count, 2)
        capture(app, "chat-reply-same-agent-no-extra-create-external-recorder")

        // Leaving Chat must restore the global new-thread composer; its next
        // Send must create another identity, never reuse the previously focused ID.
        app.buttons["main-tab-crm"].tap()
        XCTAssertTrue(app.buttons["crm-record-alex"].waitForExistence(timeout: 5))
        app.buttons["crm-record-alex"].tap()
        XCTAssertTrue(app.staticTexts["Example University"].waitForExistence(timeout: 5))
        let next = "Synthetic second nonchat new thread prompt"
        let nextDraft = composerJourneyField(app, identifier: "new-thread-composer")
        let emptyValue = nextDraft.value as? String ?? ""
        XCTAssertTrue(emptyValue.isEmpty || emptyValue == nextDraft.placeholderValue)
        nextDraft.tap(); nextDraft.typeText(next)
        app.buttons["new-thread-send"].tap()
        let afterSecondGlobal = awaitComposerRecordedRequests(app, creates: 2, turns: 3, selectedAgent: "composer-agent-2")
        let creations = afterSecondGlobal.filter { $0["path"] as? String == "/v1/agents" }
        XCTAssertEqual(Set(creations.map { $0["idempotency"] as? String ?? "" }).count, 2)
        let allTurns = afterSecondGlobal.filter { ($0["path"] as? String ?? "").hasSuffix("/turns") }
        XCTAssertEqual(allTurns.map { $0["path"] as? String ?? "" },
                       ["/v1/agents/composer-agent-1/turns", "/v1/agents/composer-agent-1/turns", "/v1/agents/composer-agent-2/turns"])
        XCTAssertEqual(allTurns.map { $0["input"] as? String ?? "" }, [first, reply, next])
        capture(app, "second-nonchat-send-different-agent-external-recorder")
    }

    func testCRMGlobalSendCreationFailureRetriesSameCreationAndPrompt() {
        let app = launchComposerTransportJourney(failCreationOnce: true)
        let prompt = "Synthetic creation failure preserved prompt"
        let draft = composerJourneyField(app, identifier: "new-thread-composer")
        draft.tap(); draft.typeText(prompt)
        app.buttons["new-thread-send"].tap()
        let retry = app.buttons["retry-pending"].firstMatch
        XCTAssertTrue(retry.waitForExistence(timeout: 15), "Definite backend creation failure must retain the pending first message")
        XCTAssertTrue(app.staticTexts[prompt].exists)
        XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", "Composer fixture transport ledger:")).firstMatch.exists,
                       "Rejected creation must not fabricate an admitted turn")
        XCTAssertTrue(app.buttons["retry-creation"].exists)
        capture(app, "crm-create-failure-prompt-retained")
        XCTAssertTrue(retry.isEnabled)
        retry.tap() // real PendingMessage -> readyAgent -> same creation key
        let retried = awaitComposerRecordedRequests(app, creates: 2, turns: 1)
        let creations = retried.filter { $0["path"] as? String == "/v1/agents" }
        XCTAssertEqual(Set(creations.map { $0["idempotency"] as? String ?? "" }).count, 1)
        let admitted = retried.first { ($0["path"] as? String ?? "").hasSuffix("/turns") }
        XCTAssertEqual(admitted?["path"] as? String, "/v1/agents/composer-agent-1/turns")
        XCTAssertEqual(admitted?["input"] as? String, prompt)
        XCTAssertFalse(app.buttons["retry-pending"].exists)
        capture(app, "crm-create-retry-one-agent-one-admitted-prompt-external-recorder")

        let next = "Synthetic follow-up after creation retry"
        let chatDraft = composerJourneyField(app, identifier: "composer")
        chatDraft.tap(); chatDraft.typeText(next)
        app.buttons["send"].tap()
        let followUp = awaitComposerRecordedRequests(app, creates: 2, turns: 2)
        let turns = followUp.filter { ($0["path"] as? String ?? "").hasSuffix("/turns") }
        XCTAssertEqual(turns.map { $0["path"] as? String ?? "" },
                       ["/v1/agents/composer-agent-1/turns", "/v1/agents/composer-agent-1/turns"])
        XCTAssertEqual(turns.map { $0["input"] as? String ?? "" }, [prompt, next])
        capture(app, "crm-create-retry-chat-reuses-created-agent-external-recorder")
    }

    private func capture(_ app: XCUIApplication, _ name: String) {
        // Native sheet/disclosure animations can outlive accessibility queries.
        // Capture their settled layout, not an intermediate clipped frame.
        Thread.sleep(forTimeInterval: 0.4)
        // App-window capture on iOS 18 can retain portrait crop coordinates
        // after rotation. Screen capture uses the actual display bounds.
        let screenshot = XCUIDevice.shared.orientation.isLandscape ? XCUIScreen.main.screenshot() : app.screenshot()
        let attachment = XCTAttachment(screenshot: screenshot)
        attachment.name = name; attachment.lifetime = .keepAlways; add(attachment)
    }
    private func scrollVisibleConversation(_ app: XCUIApplication, upward: Bool) {
        let conversation = app.descendants(matching: .any)["conversation"].firstMatch
        let frame = conversation.frame
        // The transcript extends behind the floating header. Starting there
        // hits the title button instead of moving into earlier history.
        let top = max(frame.minY, app.buttons["conversation-drawer-open"].frame.maxY) + 16
        let controls = app.buttons["toggle-all-tools"]
        let bottom = min(frame.maxY, composer(app).frame.minY,
                         controls.exists ? controls.frame.minY : frame.maxY) - 28
        let height = bottom - top
        guard height > 40 else { return XCTFail("Expected an unobscured transcript region for scrolling") }
        let origin = app.coordinate(withNormalizedOffset: .zero)
        let low = origin.withOffset(CGVector(dx: frame.midX, dy: top + height * 0.85))
        let high = origin.withOffset(CGVector(dx: frame.midX, dy: top + height * 0.15))
        (upward ? low : high).press(forDuration: 0.05, thenDragTo: upward ? high : low, withVelocity: .slow, thenHoldForDuration: 0.2)
    }
}
