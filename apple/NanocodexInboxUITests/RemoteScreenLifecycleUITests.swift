import XCTest

final class RemoteScreenLifecycleUITests: XCTestCase {
    // This separate opt-in fixture sends shell input. The caller must inspect
    // the selected VM and confirm an idle terminal owned by the test first.
    @MainActor
    func testOwnedVMInputAndBackgroundRecovery() throws {
        let environment = ProcessInfo.processInfo.environment
        guard let machine = environment["NANOCODEX_TEST_REMOTE_MACHINE_ID"], machine.hasPrefix("vm:"),
              environment["NANOCODEX_TEST_REMOTE_SURFACE_ID"] == "desktop",
              environment["NANOCODEX_TEST_VM_IDLE_TERMINAL"] == "1",
              let fixture = environment["NANOCODEX_TEST_VM_INPUT_FIXTURE"], let id = UUID(uuidString: fixture) else {
            throw XCTSkip("Requires an explicitly inspected, owned idle VM terminal and a fresh fixture UUID")
        }
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchEnvironment["NANOCODEX_REMOTE_DIAGNOSTICS"] = "1"
        app.launch()
        let screens = app.buttons["conversation-remote-screens"]
        XCTAssertTrue(screens.waitForExistence(timeout: 20)); screens.tap()
        let desktop = app.buttons["remote-screen:\(machine):desktop"]
        XCTAssertTrue(desktop.waitForExistence(timeout: 20)); desktop.tap()
        let status = app.staticTexts["remote-status"]
        let initial = try requireDecodedFrame(app, status: status, label: "initial")
        let path = "/tmp/nanocodex-phone-" + id.uuidString.lowercased()
        let marker = String(id.uuidString.replacingOccurrences(of: "-", with: "").prefix(12))

        func takeControl() {
            XCTAssertTrue(app.buttons["Take control"].waitForExistence(timeout: 10))
            app.buttons["Take control"].tap()
            XCTAssertTrue(app.buttons["Release control"].waitForExistence(timeout: 10))
        }
        func command(_ value: String) {
            let field = app.textFields["Type on remote screen"]
            XCTAssertTrue(field.waitForExistence(timeout: 10)); field.tap(); field.typeText(value)
            app.buttons["Send"].tap(); app.buttons["Return"].tap()
        }
        func requireTerminal(light: Bool, label: String) throws {
            let canvas = app.descendants(matching: .any)["remote-canvas"].firstMatch
            XCTAssertTrue(canvas.waitForExistence(timeout: 10))
            let bounds = canvas.frame
            let scale = min(bounds.width / 1600, bounds.height / 900)
            let video = CGRect(x: bounds.midX - 800 * scale, y: bounds.midY - 450 * scale,
                width: 1600 * scale, height: 900 * scale)
            let deadline = Date().addingTimeInterval(15)
            var observed = false
            repeat {
                observed = try autoreleasepool {
                    let image = try XCTUnwrap(app.screenshot().image.cgImage)
                    let pixelsPerPoint = CGFloat(image.width) / app.frame.width
                    var peak = 0
                    for y in 1...3 { for x in 1...3 {
                        let point = CGPoint(x: video.minX + video.width * CGFloat(x) / 4,
                            y: video.minY + video.height * CGFloat(y) / 4)
                        let crop = try XCTUnwrap(image.cropping(to: CGRect(x: point.x * pixelsPerPoint,
                            y: point.y * pixelsPerPoint, width: 1, height: 1)))
                        var pixel = [UInt8](repeating: 0, count: 4)
                        let context = try XCTUnwrap(CGContext(data: &pixel, width: 1, height: 1,
                            bitsPerComponent: 8, bytesPerRow: 4, space: CGColorSpaceCreateDeviceRGB(),
                            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
                        context.draw(crop, in: CGRect(x: 0, y: 0, width: 1, height: 1))
                        peak = max(peak, (Int(pixel[0]) + Int(pixel[1]) + Int(pixel[2])) / 3)
                    } }
                    return light ? peak > 220 : peak < 50
                }
                if !observed { Thread.sleep(forTimeInterval: 0.15) }
            } while !observed && Date() < deadline
            let attachment = XCTAttachment(screenshot: app.screenshot())
            attachment.name = label; attachment.lifetime = .keepAlways; add(attachment)
            XCTAssertTrue(observed, "The decoded VM terminal must show the guarded command's expected pixel transition")
        }

        takeControl()
        command("printf '\\033[48;2;0;0;0m\\033[2J\\033[H'")
        try requireTerminal(light: false, label: "phone-vm-terminal-before-input")
        command("mkdir '\(path)' && printf '%s' '\(marker)' > '\(path)/marker' && printf '\\033[48;2;255;255;255m\\033[2J\\033[H'")
        try requireTerminal(light: true, label: "phone-vm-input-before-background")
        XCUIDevice.shared.press(.home)
        XCTAssertTrue(app.wait(for: .runningBackground, timeout: 10)); app.activate()
        let recovered = try requireDecodedFrame(app, status: status, label: "foreground-recovery", previous: initial)
        XCTAssertNotEqual(initial, recovered)
        XCTAssertTrue(app.buttons["Take control"].exists, "Recovery must not reacquire input control")
        XCTAssertFalse(app.buttons["Release control"].exists)
        try requireTerminal(light: true, label: "phone-vm-selection-survived-background")
        takeControl()
        command("test \"$(cat '\(path)/marker')\" = '\(marker)' && rm '\(path)/marker' && rmdir '\(path)' && printf '\\033[48;2;0;0;0m\\033[2J\\033[H'")
        try requireTerminal(light: false, label: "phone-vm-input-after-background-cleanup")
        command("printf '\\033[0m\\033[2J\\033[H'")
        app.buttons["Release control"].tap()
        app.buttons["Screens"].tap()
        XCTAssertTrue(desktop.waitForExistence(timeout: 10)); desktop.tap()
        _ = try requireDecodedFrame(app, status: status, label: "reselection")
        app.buttons["Done"].tap()
        XCTAssertTrue(screens.waitForExistence(timeout: 10))
    }

    @MainActor
    func testPublishedVMFirstDecodedFrames() throws {
        let environment = ProcessInfo.processInfo.environment
        guard let machine = environment["NANOCODEX_TEST_REMOTE_MACHINE_ID"], machine.hasPrefix("vm:"),
              environment["NANOCODEX_TEST_REMOTE_SURFACE_ID"] == "desktop" else {
            throw XCTSkip("Requires a saved account and an explicitly selected VM screen")
        }
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchEnvironment["NANOCODEX_REMOTE_DIAGNOSTICS"] = "1"
        app.launch()
        let screens = app.buttons["conversation-remote-screens"]
        XCTAssertTrue(screens.waitForExistence(timeout: 20)); screens.tap()
        let desktop = app.buttons["remote-screen:\(machine):desktop"]
        for sample in 1...3 {
            XCTAssertTrue(desktop.waitForExistence(timeout: 20)); desktop.tap()
            _ = try requireDecodedFrame(app, status: app.staticTexts["remote-status"], label: "sample-\(sample)")
            let screenshot = XCTAttachment(screenshot: app.screenshot())
            screenshot.name = "phone-vm-first-frame-\(sample)"; screenshot.lifetime = .keepAlways; add(screenshot)
            app.buttons["Screens"].tap()
        }
        app.buttons["Done"].tap()
    }

    @MainActor
    private func requireDecodedFrame(_ app: XCUIApplication, status: XCUIElement, label: String, previous: String? = nil) throws -> String {
        XCTAssertTrue(status.waitForExistence(timeout: 10))
        var conditions = [
            NSPredicate(format: "label == %@", "Watching"),
            NSPredicate(format: "value CONTAINS %@", "\"width\":1600"),
            NSPredicate(format: "value CONTAINS %@", "\"height\":900"),
        ]
        if let previous { conditions.append(NSPredicate(format: "NOT (value CONTAINS %@)", previous)) }
        let decoded = XCTNSPredicateExpectation(predicate: NSCompoundPredicate(andPredicateWithSubpredicates: conditions), object: status)
        XCTAssertEqual(XCTWaiter.wait(for: [decoded], timeout: 45), .completed)
        let value = try XCTUnwrap(status.value as? String)
        let record = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(value.utf8)) as? [String: Any])
        let attachment = XCTAttachment(string: value)
        attachment.name = "phone-vm-phases-" + label; attachment.lifetime = .keepAlways; add(attachment)
        print("PHONE_VM_FIRST_FRAME \(label) \(value)")
        XCTAssertEqual(app.state, .runningForeground)
        return try XCTUnwrap(record["connection_id"] as? String)
    }

    @MainActor
    func testPublishedScreenSurvivesRepeatedPresentation() throws {
        let environment = ProcessInfo.processInfo.environment
        guard let machine = environment["NANOCODEX_TEST_REMOTE_MACHINE_ID"], !machine.isEmpty,
              let surface = environment["NANOCODEX_TEST_REMOTE_SURFACE_ID"], !surface.isEmpty else {
            throw XCTSkip("Requires a saved account and an explicitly selected published screen")
        }
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launch()
        let screens = app.buttons["conversation-remote-screens"]
        let desktop = app.buttons["remote-screen:\(machine):\(surface)"]
        let status = app.staticTexts["remote-status"]

        func requireWatching() {
            XCTAssertTrue(status.waitForExistence(timeout: 10))
            let watching = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", "Watching"), object: status)
            XCTAssertEqual(XCTWaiter.wait(for: [watching], timeout: 45), .completed)
            XCTAssertTrue(app.buttons["Take control"].exists)
            XCTAssertFalse(app.buttons["Release control"].exists)
            XCTAssertEqual(app.state, .runningForeground)
        }

        // Use the saved account without changing its credentials, conversations,
        // drafts, or the remote machine. Each path destroys a live UIKit canvas.
        for cycle in 0..<4 {
            XCTAssertTrue(screens.waitForExistence(timeout: 20))
            XCTAssertTrue(screens.isEnabled)
            screens.tap()
            XCTAssertTrue(desktop.waitForExistence(timeout: 20))
            desktop.tap()
            requireWatching()

            app.buttons["Screens"].tap()
            XCTAssertTrue(desktop.waitForExistence(timeout: 10))
            desktop.tap()
            requireWatching()

            if cycle == 0 {
                XCUIDevice.shared.press(.home)
                XCTAssertTrue(app.wait(for: .runningBackground, timeout: 10))
                app.activate()
                requireWatching()
            }
            if cycle == 0 || cycle == 3 {
                let evidence = XCTAttachment(screenshot: app.screenshot())
                evidence.name = "remote-screen-lifecycle-\(cycle + 1)"
                evidence.lifetime = .keepAlways
                add(evidence)
            }
            app.buttons["Done"].tap()
            XCTAssertTrue(screens.waitForExistence(timeout: 10))
            XCTAssertEqual(app.state, .runningForeground)
        }
    }
}
