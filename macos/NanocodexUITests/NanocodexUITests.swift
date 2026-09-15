import XCTest

final class NanocodexUITests: XCTestCase {
    @MainActor
    func testBrowserChromeStaysBelowNativeToolbar() throws {
        let app = fixture(theme: "light")
        app.launch(); defer { app.terminate() }
        let toggle = app.descendants(matching: .any).matching(identifier: "toggle-tab-sidebar").firstMatch
        XCTAssertTrue(toggle.waitForExistence(timeout: 10))
        let toolbar = app.toolbars.firstMatch
        let horizontal = app.buttons["new-tab-in-strip"]
        if !horizontal.exists { toggle.click() }
        XCTAssertTrue(horizontal.waitForExistence(timeout: 3))
        XCTAssertGreaterThanOrEqual(horizontal.frame.minY, toolbar.frame.maxY - 1, "The real title bar must not cover horizontal tabs")
        XCTAssertGreaterThanOrEqual(app.staticTexts["user-message"].firstMatch.frame.minY, toolbar.frame.maxY,
                                    "The first conversation turn stays below the toolbar")
        XCTAssertFalse(app.radioButtons["workspace-filter-Inbox"].exists)
        capture(app, name: "native-browser-horizontal")
        toggle.click()
        let vertical = app.buttons["new-tab-in-sidebar"]
        XCTAssertTrue(vertical.waitForExistence(timeout: 3))
        XCTAssertGreaterThanOrEqual(vertical.frame.minY, toolbar.frame.maxY - 1, "The real title bar must not cover the sidebar header")
        XCTAssertTrue(app.textViews["message-input"].exists)
        XCTAssertGreaterThanOrEqual(app.staticTexts["user-message"].firstMatch.frame.minY, toolbar.frame.maxY,
                                    "Vertical tabs must not move the first turn beneath the title bar")
        let sidebar = app.otherElements["sidebar-tabs"]
        let detailCenter = (sidebar.frame.maxX + app.windows.firstMatch.frame.maxX) / 2
        XCTAssertEqual(app.textViews["message-input"].frame.midX, detailCenter, accuracy: 12,
                       "The composer is centered in the remaining detail column")
        capture(app, name: "native-browser-vertical")
    }

    @MainActor
    func testNativeTabsComposerAndHandsNavigation() throws {
        let app = fixture(theme: "light")
        app.launch()
        defer { app.terminate() }
        XCTAssertTrue(app.toolbars.buttons["new-tab"].waitForExistence(timeout: 10), "Conversation actions live in the native window toolbar")
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
        XCTAssertGreaterThanOrEqual(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "select-browser-tab-")).count, 2)
        app.typeKey("h", modifierFlags: [.command, .shift])
        XCTAssertTrue(app.otherElements["hands-page"].waitForExistence(timeout: 3))
        capture(app, name: "native-window-hands")
    }

    @MainActor
    func testNativeGlassWindowThemesAndPaneShortcuts() throws {
        for theme in ["light", "dark"] {
            let app = fixture(theme: theme)
            app.launch()
            XCTAssertTrue(app.toolbars.buttons["new-tab"].waitForExistence(timeout: 10))
            let editor = app.textViews["message-input"]
            XCTAssertTrue(editor.waitForExistence(timeout: 5))
            editor.click(); editor.typeText("Keep my draft while I arrange the workspace")
            capture(app, name: "native-window-\(theme)")
            app.typeKey(XCUIKeyboardKey.escape, modifierFlags: [])
            app.typeKey("v", modifierFlags: [])
            XCTAssertTrue(app.textViews.matching(identifier: "message-input").element(boundBy: 1).waitForExistence(timeout: 3))
            app.typeKey("h", modifierFlags: [])
            XCTAssertTrue(app.textViews.matching(identifier: "message-input").element(boundBy: 2).waitForExistence(timeout: 3))
            capture(app, name: "native-window-splits-\(theme)")
            app.typeKey(",", modifierFlags: .command)
            XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 3))
            capture(app, name: "native-window-settings-\(theme)")
            app.buttons["Done"].click()
            app.terminate()
        }
    }

    @MainActor
    private func fixture(theme: String) -> XCUIApplication {
        let app = ProcessInfo.processInfo.environment["NANOCODEX_UI_APP_PATH"].map { XCUIApplication(url: URL(fileURLWithPath: $0)) } ?? XCUIApplication()
        app.launchEnvironment["NANOCODEX_DESKTOP_DATA"] = NSTemporaryDirectory() + "nanocodex-native-ui-" + UUID().uuidString
        app.launchEnvironment["NANOCODEX_NATIVE_UI_FIXTURE"] = "1"
        app.launchEnvironment["NANOCODEX_NATIVE_UI_THEME"] = theme
        return app
    }

    @MainActor
    private func capture(_ app: XCUIApplication, name: String) {
        let screenshot = app.screenshot()
        let attachment = XCTAttachment(screenshot: screenshot)
        attachment.name = name; attachment.lifetime = .keepAlways; add(attachment)
        // The UI runner is sandboxed. Keep screenshots in the result bundle;
        // export them with xcresulttool after the test completes.
    }
}
