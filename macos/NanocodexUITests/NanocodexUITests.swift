import XCTest

final class NanocodexUITests: XCTestCase {
    @MainActor
    func testNativeTabsComposerAndHandsNavigation() throws {
        let app = XCUIApplication()
        app.launchEnvironment["NANOCODEX_DESKTOP_DATA"] = NSTemporaryDirectory() + "nanocodex-native-ui-" + UUID().uuidString
        app.launch()
        XCTAssertTrue(app.buttons["new-tab"].waitForExistence(timeout: 10))
        let composer = app.textViews["message-input"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        composer.click(); composer.typeText("A draft in the first tab")
        app.typeKey("t", modifierFlags: .command)
        XCTAssertEqual(composer.value as? String, "")
        composer.click(); composer.typeText("A second draft")
        app.typeKey("w", modifierFlags: .command)
        XCTAssertEqual(composer.value as? String, "A draft in the first tab")
        app.typeKey("t", modifierFlags: [.command, .shift])
        XCTAssertEqual(composer.value as? String, "A second draft")
        app.buttons["toggle-tab-position"].click()
        XCTAssertTrue(app.otherElements["top-tabs"].waitForExistence(timeout: 3))
        app.buttons["hands-navigation"].click()
        XCTAssertTrue(app.staticTexts["Your agents think in the cloud. Give them a place to work."].waitForExistence(timeout: 3))
        let screenshot = XCTAttachment(screenshot: app.screenshot()); screenshot.name = "Native Hands"; screenshot.lifetime = .keepAlways; add(screenshot)
        app.terminate()
    }
}
