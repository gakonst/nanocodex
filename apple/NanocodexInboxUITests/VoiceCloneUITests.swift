import XCTest

/// Uses the production form with synthetic simulator audio and local HTTP responses.
final class VoiceCloneUITests: XCTestCase {
    override func setUp() { super.setUp(); continueAfterFailure = false }
    func testRecordReviewCreateAndUseVoice() {
        completeSelection(submitClone())
    }
    func testCreatedVoiceSurvivesCatalogRefreshFailure() {
        let app = submitClone(arguments: ["--clone-refresh-fails"])
        let message = app.staticTexts["Voice created, but the catalog could not refresh. You can still use this voice."]
        reveal(message, in: app)
        XCTAssertTrue(message.exists)
        completeSelection(app)
    }
    func testCancelledUploadKeepsSampleAndReconcilesCatalog() {
        let app = submitClone(arguments: ["--clone-upload-slow"])
        let cancel = app.buttons["clone-cancel-upload"]
        reveal(cancel, in: app)
        XCTAssertTrue(cancel.waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["save-voice-settings"].isEnabled)
        cancel.tap()
        let create = app.buttons["clone-create"]
        reveal(create, in: app, down: true)
        XCTAssertFalse(create.isEnabled)
        let review = app.buttons["Review recording"]
        reveal(review, in: app, down: true)
        XCTAssertTrue(review.isEnabled, "Cancelling must preserve the local sample")
        let refresh = app.buttons["Refresh voices"]
        reveal(refresh, in: app, down: true)
        refresh.tap()
        let clone = app.buttons["Fixture clone"]
        XCTAssertTrue(clone.waitForExistence(timeout: 5))
        clone.tap()
        app.buttons["save-voice-settings"].tap()
        XCTAssertTrue(app.staticTexts["Fixture voice saved"].waitForExistence(timeout: 5))
        attach(app, "Cancelled upload reconciled without creating a duplicate")
    }
    private func submitClone(arguments: [String] = []) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["--voice-clone-ui-fixture"] + arguments
        app.launch()
        XCTAssertTrue(app.buttons["ElevenLabs"].waitForExistence(timeout: 15))
        app.buttons["ElevenLabs"].tap()
        let name = app.textFields["clone-name"]
        XCTAssertTrue(name.waitForExistence(timeout: 15))
        reveal(name, in: app)
        name.tap(); name.typeText("Simulator voice\n")
        let record = app.buttons["Record voice sample"]
        reveal(record, in: app)
        record.tap()
        let stop = app.buttons["clone-stop-recording"]
        XCTAssertTrue(stop.waitForExistence(timeout: 5))
        // A changing production timer proves capture state survives the old ~3-second failure.
        let elapsed = app.staticTexts.matching(NSPredicate(format: "label MATCHES %@", "Recording ([4-9]|[1-9][0-9]) / 120 seconds")).firstMatch
        XCTAssertTrue(elapsed.waitForExistence(timeout: 12))
        XCTAssertTrue(stop.exists)
        attach(app, "Recording beyond three seconds")
        stop.tap()
        let review = app.buttons["Review recording"]
        XCTAssertTrue(review.waitForExistence(timeout: 10))
        review.tap()
        XCTAssertTrue(app.buttons["Stop playback"].waitForExistence(timeout: 5))
        attach(app, "Reviewing local sample")
        app.buttons["Stop playback"].tap()
        let create = app.buttons["clone-create"]
        reveal(create, in: app)
        XCTAssertFalse(create.isEnabled)
        let consent = app.switches["clone-consent"]
        reveal(consent, in: app)
        consent.switches.firstMatch.tap()
        XCTAssertEqual(consent.value as? String, "1")
        XCTAssertTrue(create.isEnabled)
        create.tap()
        return app
    }
    private func completeSelection(_ app: XCUIApplication) {
        let use = app.buttons["clone-use-voice"]
        reveal(use, in: app)
        XCTAssertTrue(use.waitForExistence(timeout: 10))
        attach(app, "Clone created")
        use.tap()
        XCTAssertTrue(app.staticTexts["Fixture voice saved"].waitForExistence(timeout: 5))
        attach(app, "Voice selection saved")
    }
    private func reveal(_ element: XCUIElement, in app: XCUIApplication, down: Bool = false) {
        for _ in 0..<10 {
            if element.exists && element.isHittable { return }
            // The first Form belongs to the presenting fixture, behind the sheet.
            // Scroll the topmost Form, never the covered presenter or sheet chrome.
            let scroll = app.collectionViews.element(boundBy: app.collectionViews.count - 1)
            if down { scroll.swipeDown() } else { scroll.swipeUp() }
        }
        XCTFail("Could not reveal \(element)")
    }
    private func attach(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name; attachment.lifetime = .keepAlways
        add(attachment)
    }
}
