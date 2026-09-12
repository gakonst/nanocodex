import AppKit

final class Fixture: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    let field = NSTextField(string: "red alpha blue alpha green")
    let counter = NSTextField(labelWithString: "Count: 0")
    let result = NSTextView()
    let run = NSButton(title: "Run Rust conformance", target: nil, action: nil)
    let permissions = NSButton(title: "Request permissions", target: nil, action: nil)
    var count = 0
    var events: [[String: Any]] = []
    var running: Process?
    var keyMonitor: Any?
    let directory = Bundle.main.bundleURL.deletingLastPathComponent()
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        let menu = NSMenu()
        let appMenu = NSMenuItem(title: "Fixture", action: nil, keyEquivalent: "")
        let sub = NSMenu(); sub.addItem(withTitle: "Quit owned fixture", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appMenu.submenu = sub; menu.addItem(appMenu)
        let editItem = NSMenuItem(title: "Edit", action: nil, keyEquivalent: "")
        let edit = NSMenu(title: "Edit")
        for (title, action, key) in [("Cut", "cut:", "x"), ("Copy", "copy:", "c"), ("Paste", "paste:", "v"), ("Select All", "selectAll:", "a")] {
            let item = NSMenuItem(title: title, action: Selector(action), keyEquivalent: key)
            item.target = nil; edit.addItem(item)
        }
        editItem.submenu = edit; menu.addItem(editItem); NSApp.mainMenu = menu
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp, .flagsChanged]) { [weak self] event in
            if let self = self, let process = self.running, let quartz = event.cgEvent,
               quartz.getIntegerValueField(.eventSourceUnixProcessID) == Int64(process.processIdentifier) {
                // Retain delivery metadata only for this run's Rust helper.
                // Never export keyboard text or events from other processes.
                let isKeyboard = event.type == .keyDown || event.type == .keyUp
                self.events.append(["event":"helperKey", "type":event.type.rawValue,
                    "keyCode":event.keyCode, "modifierFlags":event.modifierFlags.rawValue,
                    "utf16Length":isKeyboard ? (event.characters?.utf16.count ?? 0) : 0, "repeat":isKeyboard ? event.isARepeat : false,
                    "sourcePid":process.processIdentifier,
                    "firstResponder":self.window.firstResponder.map { String(describing: type(of: $0)) } ?? "none"])
            }
            return event
        }
        window = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 960, height: 730), styleMask: [.titled, .closable, .resizable, .miniaturizable], backing: .buffered, defer: false)
        window.contentMinSize = NSSize(width:960,height:730)
        window.title = "Skyre Rust — owned native conformance fixture"
        let view = window.contentView!
        let label = NSTextField(labelWithString: "All controls below contain synthetic data. Run invokes the rebuilt Rust provider only against this app.")
        label.frame = NSRect(x: 20, y: 680, width: 920, height: 30); label.autoresizingMask = [.width,.minYMargin]; view.addSubview(label)
        field.frame = NSRect(x: 20, y: 625, width: 710, height: 30); field.autoresizingMask = [.width,.minYMargin]; field.setAccessibilityIdentifier("skyre.test.input"); view.addSubview(field)
        let increment = NSButton(title: "Increment", target: self, action: #selector(increment))
        increment.frame = NSRect(x: 20, y: 575, width: 140, height: 32); increment.autoresizingMask = [.minYMargin]; increment.setAccessibilityIdentifier("skyre.test.increment"); view.addSubview(increment)
        counter.frame = NSRect(x: 180, y: 580, width: 180, height: 25); counter.autoresizingMask = [.minYMargin]; counter.setAccessibilityIdentifier("skyre.test.counter"); view.addSubview(counter)
        let slider = NSSlider(value: 25, minValue: 0, maxValue: 100, target: self, action: #selector(sliderChanged(_:)))
        slider.frame = NSRect(x: 400, y: 575, width: 330, height: 30); slider.autoresizingMask = [.width,.minYMargin]; slider.setAccessibilityIdentifier("skyre.test.slider"); view.addSubview(slider)
        let disabled = NSButton(title: "Disabled fixture button", target: nil, action: nil); disabled.isEnabled = false
        disabled.frame = NSRect(x: 740, y: 620, width: 200, height: 35); disabled.autoresizingMask = [.minXMargin,.minYMargin]; disabled.setAccessibilityIdentifier("skyre.test.disabled"); view.addSubview(disabled)
        let checkbox = NSButton(checkboxWithTitle: "Fixture checkbox", target: self, action: #selector(checkboxChanged(_:)))
        checkbox.frame = NSRect(x: 740, y: 575, width: 200, height: 30); checkbox.autoresizingMask = [.minXMargin,.minYMargin]; checkbox.setAccessibilityIdentifier("skyre.test.checkbox"); view.addSubview(checkbox)
        run.frame = NSRect(x: 20, y: 525, width: 230, height: 36); run.autoresizingMask = [.minYMargin]; run.target = self; run.action = #selector(runTests); run.setAccessibilityIdentifier("skyre.test.run"); view.addSubview(run)
        permissions.frame = NSRect(x: 270, y: 525, width: 210, height: 36); permissions.autoresizingMask = [.minYMargin]; permissions.target = self; permissions.action = #selector(requestPermissions); permissions.setAccessibilityIdentifier("skyre.test.permissions"); view.addSubview(permissions)
        let scroll = NSScrollView(frame: NSRect(x: 20, y: 20, width: 920, height: 490)); scroll.hasVerticalScroller = true; scroll.autoresizingMask = [.width, .height]
        result.isEditable = false; result.isSelectable = true; result.autoresizingMask = [.width]; result.isVerticallyResizable = true; result.textContainer?.widthTracksTextView = true; result.font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular); result.string = "Ready. No Rust UI operations have run yet."; result.setAccessibilityIdentifier("skyre.test.results")
        scroll.documentView = result; result.frame = NSRect(x: 0, y: 0, width: 900, height: 490); view.addSubview(scroll)
        window.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true)
    }
    func record(_ type: String, _ value: Any) { events.append(["event":type, "value":value]) }
    @objc func increment() { count += 1; counter.stringValue = "Count: \(count)"; record("increment", count) }
    @objc func sliderChanged(_ slider: NSSlider) { record("slider", slider.doubleValue) }
    @objc func checkboxChanged(_ button: NSButton) { record("checkbox", button.state.rawValue) }
    func clipboard() -> [[String: Data]]? {
        var out: [[String: Data]] = []
        for item in NSPasteboard.general.pasteboardItems ?? [] {
            var values: [String:Data] = [:]
            for type in item.types { guard let bytes = item.data(forType: type) else { return nil }; values[type.rawValue] = bytes }
            out.append(values)
        }
        return out
    }
    @objc func runTests() { launch(requestPermissions: false) }
    @objc func requestPermissions() { launch(requestPermissions: true) }
    func launch(requestPermissions: Bool) {
        guard running == nil else { return }
        events = []
        let stamp = String(Int(Date().timeIntervalSince1970 * 1000))
        let stdout = directory.appendingPathComponent("run-\(stamp).stdout.json")
        let stderr = directory.appendingPathComponent("run-\(stamp).stderr.txt")
        FileManager.default.createFile(atPath: stdout.path, contents: nil)
        FileManager.default.createFile(atPath: stderr.path, contents: nil)
        let saved = clipboard()
        let process = Process()
        process.executableURL = directory.appendingPathComponent("skyre")
        process.arguments = requestPermissions ? ["permissions", "--request"] : ["eval", "--file", directory.appendingPathComponent("conformance.js").path, "--timeout", "120"]
        process.standardOutput = try! FileHandle(forWritingTo: stdout)
        process.standardError = try! FileHandle(forWritingTo: stderr)
        process.terminationHandler = { [weak self] task in DispatchQueue.main.async {
            guard let self = self else { return }
            let restored = saved != nil && self.clipboard() == saved
            let report: [String:Any] = ["operation": requestPermissions ? "requestPermissions" : "conformance", "exitCode": task.terminationStatus, "clipboardRestored": restored, "events": self.events, "finalField": self.field.stringValue, "stdout":stdout.path, "stderr":stderr.path]
            let data = try! JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            try! data.write(to: self.directory.appendingPathComponent("run-\(stamp).fixture.json"), options: .atomic)
            let stdoutData = (try? Data(contentsOf: stdout)) ?? Data()
            var text = String(data:stdoutData,encoding:.utf8) ?? ""
            if var decoded = (try? JSONSerialization.jsonObject(with:stdoutData)) as? [String:Any],
               let outputs = decoded["outputs"] as? [[String:Any]] {
                decoded["outputs"] = outputs.map { output -> [String:Any] in
                    if output["channel"] as? String == "image" { return ["channel":"image","value":"PNG retained in stdout artifact"] }
                    var output = output
                    if var value = output["value"] as? [String:Any], value["artifact"] != nil, value["data"] != nil {
                        value["data"] = "Binary payload retained in stdout artifact"; output["value"] = value
                    }
                    return output
                }
                if let rendered = try? JSONSerialization.data(withJSONObject:decoded,options:[.prettyPrinted,.sortedKeys]) { text = String(data:rendered,encoding:.utf8) ?? text }
            }
            let error = String(data: (try? Data(contentsOf: stderr)) ?? Data(), encoding: .utf8) ?? ""
            self.result.string = "Rust exit: \(task.terminationStatus) — clipboard preserved: \(restored)\n\n\(text.prefix(24000))\n\(error)"
            self.running = nil; self.run.isEnabled = true; self.permissions.isEnabled = true
        } }
        do { try process.run(); running = process; run.isEnabled = false; permissions.isEnabled = false; result.string = requestPermissions ? "Requesting macOS Accessibility and Screen Recording permission for this pinned helper…" : "Rust conformance running against this fixture…" }
        catch { result.string = "Could not launch Rust: \(error)" }
    }
}
let application = NSApplication.shared
let delegate = Fixture()
application.delegate = delegate
application.run()
