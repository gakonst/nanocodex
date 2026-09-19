import XCTest

final class RemoteScreenLifecycleUITests: XCTestCase {
    private enum FixtureFailure: Error { case requirement(String) }

    @MainActor
    private func requireUI(_ condition: Bool, _ message: String, app: XCUIApplication,
                           file: StaticString = #filePath, line: UInt = #line) throws {
        guard condition else {
            let screenshot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
            screenshot.name = "failure-" + message; screenshot.lifetime = .keepAlways; add(screenshot)
            let tree = XCTAttachment(string: app.debugDescription)
            tree.name = "failure-accessibility-tree"; tree.lifetime = .keepAlways; add(tree)
            XCTFail(message, file: file, line: line)
            throw FixtureFailure.requirement(message)
        }
    }

    @MainActor
    private func openScreenControls(_ app: XCUIApplication) throws {
        let options = app.buttons["thread-screen-options"]
        // Closing the full-screen controls returns to the existing thread dock.
        if !options.waitForExistence(timeout: 2) {
            let menu = app.buttons["app-menu"]
            try requireUI(menu.waitForExistence(timeout: 20), "App menu must exist", app: app)
            menu.tap()
            let screen = app.buttons["conversation-remote-screens"]
            try requireUI(screen.waitForExistence(timeout: 5), "Screens menu item must exist", app: app)
            try requireUI(screen.isEnabled, "Screens menu item must be enabled", app: app)
            screen.tap()
        }
        try requireUI(options.waitForExistence(timeout: 10), "Thread screen options must exist", app: app)
        options.tap()
        let controls = app.buttons["Screen controls"]
        try requireUI(controls.waitForExistence(timeout: 5), "Screen controls menu item must exist", app: app)
        controls.tap()
        try requireUI(app.buttons["close-screen-pane"].waitForExistence(timeout: 10), "Full-screen Done button must exist after presentation", app: app)
    }

    @MainActor
    func testFullScreenZoomDoneAndDraftRestoration() throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_SCREEN_FIXTURE"] == "1" else {
            throw XCTSkip("Run fixtures/remote-screen.mjs on loopback port 18965")
        }
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["--demo"]
        app.launchEnvironment["NANOCODEX_DEMO_SCREENS"] = "1"
        app.launchEnvironment["NANOCODEX_DEMO_PROFILE"] = "screen-card-" + UUID().uuidString
        app.launch()
        XCTAssertTrue(app.buttons["app-menu"].waitForExistence(timeout: 15))
        let composer = app.textFields["composer"].exists ? app.textFields["composer"] : app.textViews["composer"]
        let draft = "Keep my draft while I view a screen"
        composer.tap(); composer.typeText(draft)
        try openScreenControls(app)
        let desktop = app.buttons["remote-screen:fixture:desktop"]
        XCTAssertTrue(desktop.waitForExistence(timeout: 10))
        XCTAssertFalse(app.descendants(matching: .any)["screen-pane-divider"].exists)
        desktop.tap()
        let status = app.staticTexts["remote-status"]
        func requireWatching() {
            let watching = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", "Watching"), object: status)
            XCTAssertEqual(XCTWaiter.wait(for: [watching], timeout: 15), .completed)
        }
        requireWatching()
        let canvas = app.descendants(matching: .any)["remote-canvas"].firstMatch
        XCTAssertTrue(canvas.exists)
        // Controls occupy the full display; there are no sheet detents to resize.
        XCTAssertGreaterThan(canvas.frame.height, app.frame.height * 0.6)
        XCTAssertEqual(app.buttons["close-screen-pane"].label, "Done")
        canvas.pinch(withScale: 2, velocity: 1)
        let zoomed = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            (canvas.value as? String ?? "").hasPrefix("Zoom ") && canvas.value as? String != "Zoom 100%"
        }, object: canvas)
        XCTAssertEqual(XCTWaiter.wait(for: [zoomed], timeout: 5), .completed)
        let evidence = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        evidence.name = "native-screen-fullscreen-zoom"; evidence.lifetime = .keepAlways; add(evidence)
        app.buttons["Screens"].tap()
        XCTAssertTrue(desktop.waitForExistence(timeout: 10)); desktop.tap(); requireWatching()
        app.buttons["close-screen-pane"].tap()
        XCTAssertFalse(canvas.exists)
        XCTAssertEqual(composer.value as? String, draft)
        try openScreenControls(app); XCTAssertTrue(desktop.waitForExistence(timeout: 10)); desktop.tap(); requireWatching()
        let card = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        card.name = "native-screen-fullscreen-reopened"; card.lifetime = .keepAlways; add(card)
        app.buttons["close-screen-pane"].tap()
        XCTAssertTrue(canvas.waitForNonExistence(timeout: 5))
        XCTAssertEqual(composer.value as? String, draft)
        try openScreenControls(app); XCTAssertTrue(desktop.waitForExistence(timeout: 10)); desktop.tap(); requireWatching()
        app.buttons["close-screen-pane"].tap()
    }

    private struct FixtureLog: Decodable {
        struct Event: Decodable {
            struct Input: Decodable {
                struct Gamepad: Decodable {
                    let leftX, leftY, rightX, rightY: Double
                    let leftTrigger, rightTrigger: Double
                    let buttons: [String]
                    var isNeutral: Bool {
                        [leftX, leftY, rightX, rightY, leftTrigger, rightTrigger].allSatisfy { $0 == 0 } && buttons.isEmpty
                    }
                }
                let gamepad: Gamepad?
                let kind: String
                let key: Int?
                let button: Int?
                let down: Bool?
                let deltaX: Double?
                let deltaY: Double?
            }
            let type: String
            let connection: String
            let input: Input?
        }
        let cursor: Int
        let events: [Event]
    }

    @MainActor
    private func fixtureLog(after cursor: Int) async throws -> FixtureLog {
        let url = URL(string: "http://127.0.0.1:18965/fixture/events?after=\(cursor)")!
        var request = URLRequest(url: url)
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.timeoutInterval = 3
        let (data, _) = try await URLSession.shared.data(for: request)
        return try JSONDecoder().decode(FixtureLog.self, from: data)
    }

    @MainActor
    private func requireFixtureEvents(after cursor: Int, _ description: String,
                                      matching predicate: ([FixtureLog.Event]) -> Bool) async throws -> FixtureLog {
        let deadline = Date().addingTimeInterval(5)
        var log = try await fixtureLog(after: cursor)
        while !predicate(log.events), Date() < deadline {
            try await Task.sleep(for: .milliseconds(100))
            log = try await fixtureLog(after: cursor)
        }
        guard predicate(log.events) else {
            let evidence = XCTAttachment(string: String(describing: log.events))
            evidence.name = "fixture-events-at-failure"; evidence.lifetime = .keepAlways; add(evidence)
            try requireUI(false, description, app: XCUIApplication())
            throw FixtureFailure.requirement(description)
        }
        return log
    }

    @MainActor
    func testSyntheticControllerInputReleasesExitAndBackground() async throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_SCREEN_FIXTURE"] == "1" else {
            throw XCTSkip("Run fixtures/remote-screen.mjs on loopback port 18965")
        }
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .portrait
        let baseline = try await fixtureLog(after: 0).cursor
        let app = XCUIApplication()
        app.launchArguments = ["--demo"]
        app.launchEnvironment["NANOCODEX_DEMO_SCREENS"] = "1"
        app.launchEnvironment["NANOCODEX_DEMO_PROFILE"] = "screen-controller-" + UUID().uuidString
        app.launch()
        defer { XCUIDevice.shared.orientation = .portrait }
        try openScreenControls(app)
        let controller = app.buttons["remote-screen:fixture:controller"]
        try requireUI(controller.waitForExistence(timeout: 10), "Synthetic controller surface must exist", app: app); controller.tap()
        XCUIDevice.shared.orientation = .landscapeLeft
        let landscape = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.frame.width > app.frame.height
        }, object: app)
        await fulfillment(of: [landscape], timeout: 10)
        try requireUI(app.frame.width > app.frame.height, "App must rotate to landscape", app: app)
        let launchControls = app.buttons["remote-game-controls"]
        try requireUI(launchControls.waitForExistence(timeout: 10), "Controller entry must exist", app: app)
        let enabled = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: launchControls)
        await fulfillment(of: [enabled], timeout: 15)
        try requireUI(launchControls.isEnabled, "Controller entry must be enabled", app: app)
        launchControls.tap()
        let acquired = try await requireFixtureEvents(after: baseline, "Controller entry must acquire a synthetic lease") {
            $0.contains { $0.type == "acquire" }
        }
        let connection = try XCTUnwrap(acquired.events.first { $0.type == "acquire" }?.connection)
        let joystick = app.descendants(matching: .any)["remote-game-joystick"].firstMatch
        try requireUI(joystick.waitForExistence(timeout: 5), "Joystick must exist", app: app)
        let visibleControls = ["remote-game-joystick", "remote-game-camera", "remote-game-stop", "remote-game-close"] +
            ["space", "tab", "escape", "shift", "control", "1", "2", "3", "4", "5", "6"].map { "remote-game-key-" + $0 }
        for identifier in visibleControls {
            let control = app.descendants(matching: .any)[identifier].firstMatch
            try requireUI(control.waitForExistence(timeout: 5), "Control must exist: " + identifier, app: app)
            try requireUI(control.isHittable, "Landscape control must be hittable: " + identifier, app: app)
            try requireUI(!control.frame.isEmpty, "Control frame must be nonempty: " + identifier, app: app)
            try requireUI(app.frame.contains(control.frame), "Landscape control must fit on screen: " + identifier, app: app)
        }
        let center = joystick.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        center.press(forDuration: 0.1, thenDragTo: joystick.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.1)))
        let camera = app.descendants(matching: .any)["remote-game-camera"].firstMatch
        try requireUI(camera.exists, "Camera pad must exist", app: app)
        camera.coordinate(withNormalizedOffset: CGVector(dx: 0.2, dy: 0.5))
            .press(forDuration: 0.1, thenDragTo: camera.coordinate(withNormalizedOffset: CGVector(dx: 0.8, dy: 0.5)))
        app.buttons["remote-game-key-1"].press(forDuration: 0.2)
        let inputLog = try await requireFixtureEvents(after: acquired.cursor, "Movement, camera and action must each send press and release") { events in
            let inputs = events.filter { $0.connection == connection }.compactMap(\.input)
            return [26, 30].allSatisfy { key in
                inputs.contains { $0.kind == "key" && $0.key == key && $0.down == true } &&
                inputs.contains { $0.kind == "key" && $0.key == key && $0.down == false }
            } && [true, false].allSatisfy { down in
                inputs.contains { $0.kind == "button" && $0.button == 1 && $0.down == down }
            } && inputs.contains { $0.kind == "relativeMove" && (($0.deltaX ?? 0) != 0 || ($0.deltaY ?? 0) != 0) }
        }
        let landscapeEvidence = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        landscapeEvidence.name = "synthetic-controller-landscape-after-input"
        landscapeEvidence.lifetime = .keepAlways; add(landscapeEvidence)
        app.buttons["remote-game-stop"].tap()
        let stopped = try await requireFixtureEvents(after: inputLog.cursor, "Stop must release all held inputs") {
            $0.contains { $0.input?.kind == "releaseAll" }
        }
        app.buttons["remote-game-key-1"].tap()
        // A subsequent close is an ordered barrier: paused actions cannot be
        // hidden by checking the event log before the transport has drained.
        app.buttons["remote-game-close"].tap()
        let exited = try await requireFixtureEvents(after: stopped.cursor, "Exit must release the control lease") {
            $0.contains { $0.type == "release" }
        }
        XCTAssertFalse(exited.events.contains { $0.input?.kind == "key" && $0.input?.down == true })
        try requireUI(app.buttons["Take control"].waitForExistence(timeout: 5), "Exit must restore Take control", app: app)
        launchControls.tap()
        let reacquired = try await requireFixtureEvents(after: exited.cursor, "Explicit controller reentry may acquire control") {
            $0.contains { $0.type == "acquire" }
        }
        XCUIDevice.shared.press(.home)
        try requireUI(app.wait(for: .runningBackground, timeout: 10), "App must enter background", app: app)
        _ = try await requireFixtureEvents(after: reacquired.cursor, "Background must release the lease or close its synthetic transport") {
            $0.contains { $0.type == "release" || $0.type == "disconnect" }
        }
        app.activate()
        try requireUI(app.buttons["Take control"].waitForExistence(timeout: 15), "Foreground must require Take control", app: app)
        // Controller UI survives but input authority must require an explicit tap.
        let afterBackground = try await fixtureLog(after: reacquired.cursor)
        XCTAssertFalse(afterBackground.events.contains { $0.type == "acquire" })
        let evidence = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        evidence.name = "synthetic-controller-after-background"; evidence.lifetime = .keepAlways; add(evidence)
        app.buttons["remote-game-close"].tap()
        app.buttons["close-screen-pane"].tap()
    }

    @MainActor
    func testSyntheticNativeGamepadTouchStatesStopExitAndBackground() async throws {
        // xcodebuild forwards TEST_RUNNER_NANOCODEX_SCREEN_FIXTURE=1 into
        // the test runner as NANOCODEX_SCREEN_FIXTURE=1.
        guard ProcessInfo.processInfo.environment["NANOCODEX_SCREEN_FIXTURE"] == "1" else {
            throw XCTSkip("Start fixtures/remote-screen.mjs; use TEST_RUNNER_NANOCODEX_SCREEN_FIXTURE=1")
        }
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .portrait
        let baseline = try await fixtureLog(after: 0).cursor
        let app = XCUIApplication()
        app.launchArguments = ["--demo"]
        app.launchEnvironment["NANOCODEX_DEMO_SCREENS"] = "1"
        app.launchEnvironment["NANOCODEX_DEMO_PROFILE"] = "screen-gamepad-" + UUID().uuidString
        app.launch()
        defer { XCUIDevice.shared.orientation = .portrait }
        try openScreenControls(app)
        let surface = app.buttons["remote-screen:fixture:gamepad"]
        try requireUI(surface.waitForExistence(timeout: 10), "Native gamepad fixture must exist", app: app)
        surface.tap()
        XCUIDevice.shared.orientation = .landscapeLeft
        let landscape = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            app.frame.width > app.frame.height
        }, object: app)
        await fulfillment(of: [landscape], timeout: 10)
        try requireUI(app.frame.width > app.frame.height, "Native gamepad must be landscape", app: app)
        let entry = app.buttons["remote-game-controls"]
        try requireUI(entry.waitForExistence(timeout: 10), "Native gamepad entry must exist", app: app)
        let enabled = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: entry)
        await fulfillment(of: [enabled], timeout: 15)
        try requireUI(entry.isEnabled, "Native gamepad entry must be enabled", app: app)
        entry.tap()
        let acquired = try await requireFixtureEvents(after: baseline, "Native gamepad must acquire a lease") {
            $0.contains { $0.type == "acquire" }
        }
        let connection = try XCTUnwrap(acquired.events.first { $0.type == "acquire" }?.connection)
        try requireUI(app.descendants(matching: .any)["remote-native-gamepad"].firstMatch.waitForExistence(timeout: 5),
                      "Native gamepad mode must appear", app: app)
        var cursor = acquired.cursor
        for side in ["left", "right"] {
            let stick = app.descendants(matching: .any)["remote-gamepad-\(side)-analog"].firstMatch
            try requireUI(stick.waitForExistence(timeout: 5) && stick.isHittable && app.frame.contains(stick.frame),
                          "Native analog must fit and be hittable: " + side, app: app)
            stick.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
                .press(forDuration: 0.1, thenDragTo: stick.coordinate(withNormalizedOffset: CGVector(dx: 0.8, dy: 0.2)))
            let log = try await requireFixtureEvents(after: cursor, "Analog touch must send nonzero then neutral: " + side) { events in
                let states = events.filter { $0.connection == connection && $0.input?.kind == "gamepad" }.compactMap { $0.input?.gamepad }
                guard let moved = states.firstIndex(where: {
                    side == "left" ? ($0.leftX > 0 && $0.leftY < 0) : ($0.rightX > 0 && $0.rightY < 0)
                }) else { return false }
                return states.dropFirst(moved + 1).contains { $0.isNeutral }
            }
            cursor = log.cursor
        }
        // Coordinate presses exercise UIKit touch handling, not accessibility actions.
        // XCTest's public coordinate API serializes gestures; simultaneous
        // independent trigger + face-button holds require a separate device test.
        for name in ["a", "b", "x", "y", "dpadUp", "dpadDown", "dpadLeft", "dpadRight",
                     "leftShoulder", "rightShoulder", "leftStick", "rightStick", "back", "start",
                     "leftTrigger", "rightTrigger"] {
            let control = app.descendants(matching: .any)["remote-gamepad-" + name].firstMatch
            try requireUI(control.waitForExistence(timeout: 5) && control.isHittable && app.frame.contains(control.frame),
                          "Native button must fit and be hittable: " + name, app: app)
            control.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).press(forDuration: name == "a" ? 1.1 : 0.2)
            let log = try await requireFixtureEvents(after: cursor, "Button touch must send pressed then released: " + name) { events in
                let states = events.filter { $0.connection == connection && $0.input?.kind == "gamepad" }.compactMap { $0.input?.gamepad }
                guard let pressed = states.firstIndex(where: {
                    if name == "leftTrigger" { return $0.leftTrigger > 0 }
                    if name == "rightTrigger" { return $0.rightTrigger > 0 }
                    return $0.buttons.contains(name)
                }) else { return false }
                return states.dropFirst(pressed + 1).contains { $0.isNeutral }
            }
            if name == "a" {
                let held = log.events.filter {
                    $0.connection == connection && $0.input?.gamepad?.buttons.contains("a") == true
                }
                XCTAssertGreaterThanOrEqual(held.count, 10, "A held button must keep sending snapshots past the host watchdog interval")
            }
            cursor = log.cursor
        }
        let evidence = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        evidence.name = "synthetic-native-gamepad-landscape-touch-input"
        evidence.lifetime = .keepAlways; add(evidence)
        app.buttons["remote-game-stop"].tap()
        let stopped = try await requireFixtureEvents(after: cursor, "Stop must send neutral gamepad and releaseAll") { events in
            let inputs = events.filter { $0.connection == connection }.compactMap(\.input)
            return inputs.contains { $0.kind == "gamepad" && $0.gamepad?.isNeutral == true } &&
                inputs.contains { $0.kind == "releaseAll" }
        }
        app.descendants(matching: .any)["remote-gamepad-a"].firstMatch
            .coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        app.buttons["remote-game-close"].tap()
        let exited = try await requireFixtureEvents(after: stopped.cursor, "Close must release native gamepad lease") {
            $0.contains { $0.connection == connection && $0.type == "release" }
        }
        XCTAssertFalse(exited.events.contains { $0.connection == connection && $0.input?.gamepad?.isNeutral == false },
                       "Stopped controls must not send active gamepad states")
        try requireUI(app.buttons["Take control"].waitForExistence(timeout: 5), "Close must restore Take control", app: app)
        entry.tap()
        let reacquired = try await requireFixtureEvents(after: exited.cursor, "Explicit reentry must acquire native gamepad control") {
            $0.contains { $0.type == "acquire" }
        }
        let resumedConnection = try XCTUnwrap(reacquired.events.first { $0.type == "acquire" }?.connection)
        XCUIDevice.shared.press(.home)
        try requireUI(app.wait(for: .runningBackground, timeout: 10), "Native gamepad app must background", app: app)
        _ = try await requireFixtureEvents(after: reacquired.cursor, "Background must release or disconnect native gamepad lease") {
            $0.contains { $0.connection == resumedConnection && ($0.type == "release" || $0.type == "disconnect") }
        }
        app.activate()
        try requireUI(app.buttons["Take control"].waitForExistence(timeout: 15), "Foreground must require explicit control", app: app)
        let foreground = try await fixtureLog(after: reacquired.cursor)
        XCTAssertFalse(foreground.events.contains { $0.type == "acquire" }, "Foreground must not automatically acquire")
        app.buttons["remote-game-close"].tap()
        app.buttons["close-screen-pane"].tap()
    }

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
        try openScreenControls(app)
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
            if !field.exists { app.buttons["Remote keyboard"].tap() }
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
            let attachment = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
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
        app.buttons["close-screen-pane"].tap()
        XCTAssertTrue(app.buttons["thread-screen-options"].waitForExistence(timeout: 10))
    }

    @MainActor
    func testPublishedVMFirstDecodedFrames() throws {
        let environment = ProcessInfo.processInfo.environment
        guard let machine = environment["NANOCODEX_TEST_REMOTE_MACHINE_ID"], !machine.isEmpty,
              let surface = environment["NANOCODEX_TEST_REMOTE_SURFACE_ID"], !surface.isEmpty else {
            throw XCTSkip("Requires a saved account and an explicitly selected published screen")
        }
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchEnvironment["NANOCODEX_REMOTE_DIAGNOSTICS"] = "1"
        app.launch()
        try openScreenControls(app)
        let desktop = app.buttons["remote-screen:\(machine):\(surface)"]
        for sample in 1...3 {
            XCTAssertTrue(desktop.waitForExistence(timeout: 20)); desktop.tap()
            _ = try requireDecodedFrame(app, status: app.staticTexts["remote-status"], label: "sample-\(sample)")
            let screenshot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
            screenshot.name = "phone-vm-first-frame-\(sample)"; screenshot.lifetime = .keepAlways; add(screenshot)
            app.buttons["Screens"].tap()
        }
        app.buttons["close-screen-pane"].tap()
    }

    @MainActor
    private func requireDecodedFrame(_ app: XCUIApplication, status: XCUIElement, label: String, previous: String? = nil) throws -> String {
        XCTAssertTrue(status.waitForExistence(timeout: 10))
        var conditions = [
            NSPredicate(format: "label == %@", "Watching"),
            NSPredicate { element, _ in
                guard let value = (element as? XCUIElement)?.value as? String,
                      let record = try? JSONSerialization.jsonObject(with: Data(value.utf8)) as? [String: Any],
                      let frame = record["first_frame"] as? [String: Int],
                      let width = frame["width"], let height = frame["height"], width > 0, height > 0 else { return false }
                let env = ProcessInfo.processInfo.environment
                if let expected = env["NANOCODEX_TEST_REMOTE_WIDTH"].flatMap(Int.init), width != expected { return false }
                if let expected = env["NANOCODEX_TEST_REMOTE_HEIGHT"].flatMap(Int.init), height != expected { return false }
                return true
            },
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
            try openScreenControls(app)
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
                let evidence = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
                evidence.name = "remote-screen-lifecycle-\(cycle + 1)"
                evidence.lifetime = .keepAlways
                add(evidence)
            }
            app.buttons["close-screen-pane"].tap()
            XCTAssertTrue(app.buttons["thread-screen-options"].waitForExistence(timeout: 10))
            XCTAssertEqual(app.state, .runningForeground)
        }
    }
}
