import AppKit
import Foundation
import Darwin

// Account credentials remain owned by the CLI. This process consumes only its
// safe status projection and launches the existing interactive login on request.
struct HandStatus: Decodable {
    struct Local: Decodable {
        struct KeepAwake: Decodable {
            let enabled: Bool
            let active: Bool?
            let can_change: Bool?
            let environment_override: Bool?
            let error: String?
        }
        let keep_awake: KeepAwake?
        let installed: Bool?
        let loaded: Bool?
        let pid: Int?
        let pending_login: Bool?
        let error: String?
    }
    struct Account: Decodable {
        let state: String
        let display_name: String?
    }
    struct Inventory: Decodable {
        struct Hand: Decodable {
            let id: String
            let name: String
            let kind: String
            let health: String
            let detail: String?
        }
        let state: String
        let hands: [Hand]
    }
    let schema_version: Int
    let local: Local
    let account: Account
    let inventory: Inventory
}

struct CommandResult {
    let succeeded: Bool
    let output: Data
}

// One presentation is used by the native menu and Copy Status.
// Unknown observations never reuse a healthy inventory.
struct MenuPresentation {
    let summary: [String]
    let resources: [String]
    let canToggle: Bool
    let canRestart: Bool
    let canSignIn: Bool
    let stop: Bool
    let warning: Bool

    static func text(_ value: String) -> String {
        String(value.components(separatedBy: .controlCharacters).joined(separator: " ").prefix(160))
    }

    static func make(status: HandStatus?, busy: Bool, operation: String, failure: String?, signingIn: Bool) -> MenuPresentation {
        var lines = ["Menu companion: Running"]
        var resources: [String] = []
        let checking = busy && operation == "menu-status"
        let local = status?.local
        let running = local?.loaded == true && local?.pid != nil
        let waiting = local?.pending_login == true && !running
        let localKnown = local?.error == nil && local?.installed != nil && local?.loaded != nil
        if busy && !checking {
            lines.append(operation == "keep-awake" ? "Keep awake: Updating…" : "Local service: \(operation == "start" ? "Starting…" : operation == "stop" ? "Stopping…" : "Restarting…")")
        } else if checking {
            lines.append("Local service: Checking…")
        } else if !localKnown {
            lines.append("Local service: Status unavailable")
        } else if running {
            lines.append("Local service: Running (process \(local?.pid ?? 0))")
        } else if waiting {
            lines.append("Local service: Waiting for sign-in")
        } else if local?.loaded == true {
            lines.append("Local service: Starting…")
        } else {
            lines.append(local?.installed == true ? "Local service: Stopped" : "Local service: Not installed")
        }
        if let awake = local?.keep_awake, operation != "keep-awake" || !busy {
            if awake.error != nil { lines.append("Keep awake: Could not apply setting") }
            else if awake.environment_override == true { lines.append("Keep awake: Off (service override)") }
            else if awake.enabled && awake.active == false { lines.append("Keep awake: On · Assertion unavailable") }
            else { lines.append("Keep awake: \(awake.enabled ? "On" : "Off")") }
        }
        let accountState = status?.account.state ?? "unknown"
        if checking { lines.append("Account: Checking…") }
        else {
            switch accountState {
            case "verified":
                let name = text(status?.account.display_name ?? "")
                lines.append(name.isEmpty ? "Account: Signed in" : "Account: Signed in · \(name)")
            case "signed_out": lines.append("Account: Signed out")
            case "expired": lines.append("Account: Sign-in expired")
            case "network_error": lines.append("Account: Unable to verify — network unavailable")
            case "permission_denied": lines.append("Account: Verification denied")
            default: lines.append("Account: Status unavailable")
            }
        }
        if signingIn { lines.append("Sign-in: Continue in Terminal") }
        if busy && !(checking && status != nil) {
            lines.append("Hands: Refreshing…")
        } else if let inventory = status?.inventory {
            switch inventory.state {
            case "ready":
                let connected = inventory.hands.filter { $0.health == "connected" }.count
                lines.append("Hands: \(connected) connected\(checking ? " · Refreshing…" : "")")
            case "signed_out": lines.append("Hands: Sign in to view")
            case "expired": lines.append("Hands: Sign in again to view")
            case "partial": lines.append("Hands: Some connections unavailable")
            case "network_error": lines.append("Hands: Server unavailable")
            case "permission_denied": lines.append("Hands: Access denied")
            default: lines.append("Hands: Status unavailable")
            }
            if inventory.state == "ready" || !inventory.hands.isEmpty {
                let hands = inventory.hands.sorted {
                    let leftConnected = $0.health == "connected"
                    let rightConnected = $1.health == "connected"
                    if leftConnected != rightConnected { return leftConnected }
                    let comparison = $0.name.localizedStandardCompare($1.name)
                    return comparison == .orderedSame ? $0.id < $1.id : comparison == .orderedAscending
                }
                // Keep every record, including distinct Hands with the same name.
                resources = hands.map { resource($0, complete: true) }
                if resources.isEmpty { lines.append("No Hands registered") }

            }
        } else { lines.append("Hands: Status unavailable") }
        if let failure { lines.append(failure) }
        return MenuPresentation(summary: lines, resources: resources,
            canToggle: !busy && failure == nil && localKnown && local?.installed == true && !waiting,
            canRestart: !busy && failure == nil && localKnown && running,
            canSignIn: !busy && !signingIn && ["signed_out", "expired"].contains(accountState),
            stop: local?.loaded == true,
            // Checking is not evidence that the last observed warning cleared.
            // Keep it until a new observation succeeds; initial checking has
            // no prior observation to warn about.
            warning: failure != nil || ((status != nil || !checking) && (local?.error != nil || local?.keep_awake?.error != nil || ["expired", "network_error", "permission_denied", "unknown"].contains(accountState) || ["partial", "network_error", "permission_denied", "unknown"].contains(status?.inventory.state ?? "unknown"))))
    }

    private static func resource(_ hand: HandStatus.Inventory.Hand, complete: Bool) -> String {
        let name = text(hand.name).trimmingCharacters(in: .whitespaces)
        let label = name.isEmpty ? "Unnamed \(text(hand.kind))" : name
        let state: String
        switch complete ? hand.health : "unknown" {
        case "connected": state = "Connected"
        case "screen_advertised": state = "Screen advertised"
        case "available": state = ["screen", "screen_only"].contains(hand.kind) ? "Screen available" : "Available"
        case "offline", "disconnected": state = "Disconnected"
        case "unavailable": state = "Unavailable"
        default: state = "Status unknown"
        }
        let detail = text(hand.detail ?? "")
        return "\(label) · \(state)\(detail.isEmpty ? "" : " — " + detail)"
    }
}

// The daemon owns permission checks. The companion only presents its result.
struct PermissionStatus: Decodable {
    struct Daemon: Decodable { let pid: Int; let executable: String }
    struct Permission: Decodable { let granted: Bool; let pane: String }
    struct Permissions: Decodable { let input: Permission; let screenCapture: Permission }
    let schema_version: Int
    let daemon: Daemon
    let permissions: Permissions
}

final class HandDragView: NSImageView, NSDraggingSource {
    var file: URL?
    var dropped: (() -> Void)?
    override func mouseDown(with event: NSEvent) {}
    override func mouseDragged(with event: NSEvent) {
        guard let file else { return }
        let item = NSDraggingItem(pasteboardWriter: file as NSURL)
        item.setDraggingFrame(bounds, contents: image)
        beginDraggingSession(with: [item], event: event, source: self)
    }
    func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation { .copy }
    func draggingSession(_ session: NSDraggingSession, endedAt point: NSPoint, operation: NSDragOperation) {
        dropped?()
    }
}

final class PermissionGuide: NSObject, NSWindowDelegate {
    private let cli: URL
    private let onClose: () -> Void
    private let panel = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 390, height: 220), styleMask: [.titled, .closable, .nonactivatingPanel], backing: .buffered, defer: false)
    private let icon = HandDragView()
    private let instruction = NSTextField(wrappingLabelWithString: "Checking the running Hand…")
    private let input = NSTextField(labelWithString: "○  Accessibility")
    private let screen = NSTextField(labelWithString: "○  Screen Recording")
    private let message = NSTextField(wrappingLabelWithString: "")
    private var timers: [Timer] = []
    private var child: Process?
    private var closed = false
    private var currentPane: String?
    private var status: PermissionStatus?
    private var droppedAt: Date?
    private var restartAttempted = false
    private var verifyingRestart = false
    private var restartButton: NSButton!

    init(cli: URL, onClose: @escaping () -> Void) {
        self.cli = cli; self.onClose = onClose
        super.init()
        panel.title = "Allow Hand access"
        panel.delegate = self
        panel.level = .floating
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.collectionBehavior = [.moveToActiveSpace, .fullScreenAuxiliary]
        let content = panel.contentView!
        icon.frame = NSRect(x: 18, y: 136, width: 60, height: 60)
        icon.imageScaling = .scaleProportionallyUpOrDown
        icon.setAccessibilityLabel("Drag the running Hand executable into System Settings")
        icon.dropped = { [weak self] in self?.droppedAt = Date() }
        content.addSubview(icon)
        instruction.font = .systemFont(ofSize: 13, weight: .medium)
        instruction.frame = NSRect(x: 92, y: 136, width: 280, height: 60)
        content.addSubview(instruction)
        input.frame = NSRect(x: 20, y: 109, width: 350, height: 20)
        screen.frame = NSRect(x: 20, y: 85, width: 350, height: 20)
        content.addSubview(input); content.addSubview(screen)
        message.font = .systemFont(ofSize: 11)
        message.textColor = .secondaryLabelColor
        message.frame = NSRect(x: 20, y: 43, width: 350, height: 37)
        content.addSubview(message)
        func button(_ title: String, _ action: Selector, _ x: CGFloat, _ width: CGFloat) -> NSButton {
            let b = NSButton(title: title, target: self, action: action)
            b.bezelStyle = .rounded
            b.font = .systemFont(ofSize: 11)
            b.frame = NSRect(x: x, y: 9, width: width, height: 26)
            content.addSubview(b)
            return b
        }
        _ = button("Accessibility", #selector(openInput), 10, 96)
        _ = button("Screen Recording", #selector(openScreen), 107, 119)
        restartButton = button("Restart Hand", #selector(restart), 227, 94)
        _ = button("Done", #selector(done), 324, 56)
    }

    func show() {
        guard timers.isEmpty else { attach(); return }
        openPane("Privacy_Accessibility")
        for (interval, action) in [(0.25, { [weak self] in self?.attach() }), (2.0, { [weak self] in self?.poll() })] {
            let timer = Timer(timeInterval: interval, repeats: true) { _ in action() }
            timers.append(timer)
            RunLoop.main.add(timer, forMode: .common)
        }
        poll()
    }

    private func attach() {
        guard !closed else { return }
        let pids = Set(NSRunningApplication.runningApplications(withBundleIdentifier: "com.apple.systempreferences").map { $0.processIdentifier })
        let windows = CGWindowListCopyWindowInfo(.optionOnScreenOnly, kCGNullWindowID) as? [[String: Any]] ?? []
        let rectangles: [CGRect] = windows.compactMap { info in
            guard let pid = info[kCGWindowOwnerPID as String] as? Int32, pids.contains(pid),
                  info[kCGWindowLayer as String] as? Int == 0,
                  let dictionary = info[kCGWindowBounds as String] as? [String: Any],
                  let rect = CGRect(dictionaryRepresentation: dictionary as CFDictionary), rect.height > 150 else { return nil }
            return rect
        }
        guard let bounds = rectangles.max(by: { $0.width * $0.height < $1.width * $1.height }),
              let primary = NSScreen.screens.first else { panel.orderOut(nil); return }
        let x = bounds.maxX - panel.frame.width - 16
        let y = primary.frame.maxY - bounds.maxY + 16
        panel.setFrameOrigin(NSPoint(x: x, y: y))
        panel.orderFrontRegardless()
    }

    private func openPane(_ pane: String) {
        currentPane = pane
        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?\(pane)")!)
    }
    @objc private func openInput() { openPane("Privacy_Accessibility") }
    @objc private func openScreen() { openPane("Privacy_ScreenCapture") }
    @objc private func done() { panel.close() }
    func windowWillClose(_ notification: Notification) {
        guard !closed else { return }
        closed = true
        timers.forEach { $0.invalidate() }; timers.removeAll()
        // A restart may still finish after the guide is dismissed.
        if child?.arguments?.contains("--check") == true { child?.terminate() }
        onClose()
    }

    private func run(_ arguments: [String], completion: @escaping (Bool, Data) -> Void) {
        guard child == nil, !closed else { return }
        let process = Process()
        process.executableURL = cli
        process.arguments = ["hand"] + arguments
        process.standardInput = FileHandle.nullDevice
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        child = process
        restartButton.isEnabled = false
        do { try process.run() } catch {
            child = nil; restartButton.isEnabled = true
            completion(false, Data(error.localizedDescription.utf8)); return
        }
        if arguments.contains("--check") {
            DispatchQueue.global().asyncAfter(deadline: .now() + 22) { [weak process] in
                if let process, process.isRunning { process.terminate() }
            }
        }
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            DispatchQueue.main.async {
                guard let self else { return }
                self.child = nil
                self.restartButton.isEnabled = true
                guard !self.closed else { return }
                completion(process.terminationStatus == 0, data)
            }
        }
    }

    private func poll() {
        run(["permissions", "--check", "--json"]) { [weak self] success, data in
            guard let self else { return }
            guard success, let state = try? JSONDecoder().decode(PermissionStatus.self, from: data),
                  state.schema_version == 1, state.daemon.pid > 0,
                  state.daemon.executable.hasPrefix("/"),
                  FileManager.default.isExecutableFile(atPath: state.daemon.executable) else {
                self.icon.file = nil; self.icon.image = nil
                self.instruction.stringValue = "Unable to check the running Hand."
                self.input.stringValue = "—  Accessibility: unknown"
                self.screen.stringValue = "—  Screen Recording: unknown"
                self.message.stringValue = String((String(data: data, encoding: .utf8) ?? "Start the Hand and retry.").prefix(180))
                return
            }
            self.status = state
            self.icon.file = URL(fileURLWithPath: state.daemon.executable)
            self.icon.image = NSWorkspace.shared.icon(forFile: state.daemon.executable)
            self.icon.toolTip = state.daemon.executable
            self.instruction.stringValue = "Drag \(URL(fileURLWithPath: state.daemon.executable).lastPathComponent) into the list above (or turn it on if it’s already listed)."
            let inputAllowed = state.permissions.input.granted
            let screenAllowed = state.permissions.screenCapture.granted
            self.input.stringValue = "\(inputAllowed ? "✓" : "○")  Accessibility"
            self.screen.stringValue = "\(screenAllowed ? "✓" : "○")  Screen Recording"
            self.input.textColor = inputAllowed ? .systemGreen : .labelColor
            self.screen.textColor = screenAllowed ? .systemGreen : .labelColor
            if inputAllowed && screenAllowed {
                if self.verifyingRestart { self.done(); return }
                if !self.restartAttempted { self.restart(); return }
            } else if self.currentPane == nil || (self.currentPane == "Privacy_Accessibility" && inputAllowed) || (self.currentPane == "Privacy_ScreenCapture" && screenAllowed) {
                self.openPane(inputAllowed ? "Privacy_ScreenCapture" : "Privacy_Accessibility")
            }
            if !screenAllowed, let dropped = self.droppedAt, Date().timeIntervalSince(dropped) >= 10 {
                self.message.stringValue = "Already allowed Screen Recording? Restart Hand to refresh its permission."
            } else if !self.restartAttempted {
                self.message.stringValue = "Only the running Hand receives this access."
            }
        }
    }
    @objc private func restart() {
        guard child == nil else { return }
        restartAttempted = true
        verifyingRestart = false
        message.stringValue = "Restarting Hand…"
        run(["restart"]) { [weak self] success, data in
            guard let self else { return }
            if success {
                self.verifyingRestart = true
                self.message.stringValue = "Checking permissions after restart…"
                self.poll()
            } else {
                self.message.stringValue = "Restart failed. \(String((String(data: data, encoding: .utf8) ?? "Try Restart Hand again.").prefix(130)))"
            }
        }
    }
}

final class HandMenuBar: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private let cli: URL
    private let guideOnly: Bool
    private var permissionGuide: PermissionGuide?
    private var item: NSStatusItem?
    private let menu = NSMenu()
    private var status: HandStatus?
    private var busy = false
    private var pendingOperation = "menu-status"
    private var timer: Timer?
    private var command: Process?
    private var lastFailure: String?
    private var quitFailure: String?
    private var quitting = false
    private var commandGeneration = 0
    private var signInScript: URL?
    private var signInFailure: String?
    private var signInStarted: Date?
    private var refreshTicks = 0

    init(cli: URL, guideOnly: Bool) { self.cli = cli; self.guideOnly = guideOnly; super.init() }

    @objc private func showPermissionGuide() {
        if permissionGuide == nil {
            permissionGuide = PermissionGuide(cli: cli) { [weak self] in
                guard let self else { return }
                self.permissionGuide = nil
                if self.guideOnly { NSApp.terminate(nil) }
            }
        }
        permissionGuide?.show()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        if guideOnly { showPermissionGuide(); return }
        menu.autoenablesItems = false
        menu.delegate = self
        let autosaveName = "NanocodexStandaloneHand"
        let positionKey = "NSStatusItem Preferred Position " + autosaveName
        // Seed only the first position, clear of the camera housing.
        if UserDefaults.standard.object(forKey: positionKey) == nil {
            UserDefaults.standard.set(180, forKey: positionKey)
        }
        let statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        statusItem.autosaveName = autosaveName
        statusItem.isVisible = true
        statusItem.button?.setAccessibilityIdentifier("nanocodex-hand-menu")
        item = statusItem
        statusItem.menu = menu
        let pollTimer = Timer(timeInterval: 5, repeats: true) { [weak self] _ in
            guard let self else { return }
            self.refreshTicks += 1
            if self.signInScript != nil || self.refreshTicks % 6 == 0 { self.refreshStatus() }
        }
        timer = pollTimer
        RunLoop.main.add(pollTimer, forMode: .common)
        refreshStatus()
    }

    func menuWillOpen(_ menu: NSMenu) { refreshStatus() }
    func applicationDidBecomeActive(_ notification: Notification) { if !guideOnly { refreshStatus() } }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if guideOnly { showPermissionGuide(); return false }
        item?.button?.performClick(nil)
        return false
    }

    private var presentation: MenuPresentation {
        MenuPresentation.make(status: status, busy: busy, operation: pendingOperation,
                              failure: lastFailure, signingIn: signInScript != nil)
    }

    private func render() {
        let view = presentation
        let next = NSMenu()
        func add(_ title: String, _ action: Selector? = nil, enabled: Bool = false) {
            let row = NSMenuItem(title: title, action: action, keyEquivalent: "")
            row.target = action == nil ? nil : self
            row.isEnabled = enabled
            next.addItem(row)
        }
        add("Nanocodex Hand · \(MenuPresentation.text(Host.current().localizedName ?? "This Mac"))")
        for line in view.summary { add(line) }
        if let quitFailure { add(quitFailure) }
        if !view.resources.isEmpty {
            next.addItem(.separator())
            if view.resources.count <= 20 {
                addConnections(view.resources, to: next)
            } else {
                for start in stride(from: 0, to: view.resources.count, by: 20) {
                    let end = min(start + 20, view.resources.count)
                    let page = NSMenuItem(title: "Hands \(start + 1)–\(end)", action: nil, keyEquivalent: "")
                    page.isEnabled = true
                    let pageMenu = NSMenu()
                    pageMenu.autoenablesItems = false
                    addConnections(Array(view.resources[start..<end]), to: pageMenu)
                    page.submenu = pageMenu
                    next.addItem(page)
                }
            }
        }
        next.addItem(.separator())
        add(signInScript == nil ? "Sign In…" : "Sign-in Open in Terminal", #selector(signIn), enabled: view.canSignIn)
        if let signInFailure { add(signInFailure) }
        add(view.stop ? "Stop Hand" : "Start Hand", #selector(toggleHand), enabled: view.canToggle)
        add("Restart Hand", #selector(restartHand), enabled: view.canRestart)
        let keepAwake = NSMenuItem(title: "Keep Mac Awake", action: #selector(toggleKeepAwake), keyEquivalent: "")
        keepAwake.target = self
        keepAwake.state = status?.local.keep_awake.map { $0.enabled ? .on : .off } ?? .mixed
        keepAwake.isEnabled = !busy && status?.local.keep_awake?.can_change == true && status?.local.error == nil
        keepAwake.toolTip = "Prevents idle system sleep while the Hand runs. The screen can turn off and lock. Closing the lid or choosing Sleep can still suspend the Mac."
        next.addItem(keepAwake)
        add("Refresh Status", #selector(refreshStatus), enabled: !busy)
        next.addItem(.separator())
        add("Allow Screen & Input Permissions…", #selector(showPermissionGuide), enabled: true)
        add("Open Hand Log", #selector(openLog), enabled: true)
        add("Copy Status", #selector(copyStatus), enabled: true)
        next.addItem(.separator())
        add("Quit Hand", #selector(quitHand), enabled: !quitting && signInScript == nil && (!busy || pendingOperation == "menu-status"))
        updateMenu(menu, from: next)
        item?.button?.toolTip = view.summary.joined(separator: "\n")
        item?.button?.setAccessibilityLabel("Nanocodex Hand · " + (view.warning ? "Warning · " : "") + view.summary.dropFirst().joined(separator: ". "))
        item?.button?.image = NSImage(systemSymbolName: view.warning ? "exclamationmark.triangle" : "hand.raised.fill", accessibilityDescription: "Nanocodex Hand")
        item?.button?.image?.isTemplate = true
    }

    // Preserve NSMenuItem and submenu identities during tracking. Rebuilding
    // the tree invalidates AppKit's highlighted rows and accessibility refs.
    private func updateMenu(_ target: NSMenu, from desired: NSMenu) {
        let rows = desired.items
        for (index, fresh) in rows.enumerated() {
            if index >= target.items.count {
                desired.removeItem(fresh)
                target.addItem(fresh)
                continue
            }
            let current = target.items[index]
            if current.isSeparatorItem != fresh.isSeparatorItem {
                target.removeItem(at: index)
                desired.removeItem(fresh)
                target.insertItem(fresh, at: index)
                continue
            }
            current.title = fresh.title
            current.action = fresh.action
            current.target = fresh.target
            current.isEnabled = fresh.isEnabled
            current.state = fresh.state
            current.toolTip = fresh.toolTip
            if let child = fresh.submenu {
                if let existing = current.submenu { updateMenu(existing, from: child) }
                else {
                    fresh.submenu = nil
                    current.submenu = child
                }
            } else { current.submenu = nil }
        }
        while target.items.count > rows.count { target.removeItem(at: target.items.count - 1) }
    }

    private func addConnections(_ entries: [String], to target: NSMenu) {
        for title in entries {
            let row = NSMenuItem(title: title, action: nil, keyEquivalent: "")
            row.isEnabled = false
            target.addItem(row)
        }
    }

    // AppKit tracks an open menu in its own run-loop mode. Status completions
    // and deadlines must keep running while the user is reading that menu.
    private func after(_ interval: TimeInterval, _ action: @escaping () -> Void) {
        let deadline = Timer(timeInterval: interval, repeats: false) { _ in action() }
        RunLoop.main.add(deadline, forMode: .common)
    }

    // Serialized child processes keep every network read off the AppKit thread.
    private func run(_ operation: String, arguments: [String] = [], completion: @escaping (CommandResult) -> Void) {
        guard !busy else { return }
        busy = true
        pendingOperation = operation
        commandGeneration += 1
        let generation = commandGeneration
        render()
        let child = Process()
        child.executableURL = cli
        child.arguments = ["hand", operation] + arguments
        child.standardInput = FileHandle.nullDevice
        let output = Pipe()
        child.standardOutput = operation == "menu-status" ? output : FileHandle.nullDevice
        child.standardError = FileHandle.nullDevice
        command = child
        do { try child.run() }
        catch {
            command = nil
            busy = false
            completion(CommandResult(succeeded: false, output: Data()))
            return
        }
        DispatchQueue.global(qos: .utility).async { [weak self] in
            // Mutations use /dev/null so quitting the menu cannot break their
            // stdout pipe while the independent controller finishes.
            let data = operation == "menu-status" ? output.fileHandleForReading.readDataToEndOfFile() : Data()
            child.waitUntilExit()
            let result = CommandResult(succeeded: child.terminationStatus == 0, output: data)
            RunLoop.main.perform(inModes: [.common, .eventTracking]) {
                guard let self, self.commandGeneration == generation else { return }
                self.command = nil
                self.busy = false
                completion(result)
            }
        }
        // Mutations retain the controller's deadline, including graceful Stop.
        // Only read-only observations may be terminated by the companion.
        if operation == "menu-status" {
            after(20) { [weak self, weak child] in
                guard let self, self.commandGeneration == generation, let child, child.isRunning else { return }
                self.lastFailure = "Status check timed out"
                self.status = nil
                child.terminate()
                self.render()
                self.after(1) { [weak self, weak child] in
                    guard let self, self.commandGeneration == generation, let child, child.isRunning else { return }
                    kill(child.processIdentifier, SIGKILL)
                }
            }
        }
    }

    @objc private func refreshStatus() {
        guard !busy else { return }
        reconcileSignIn()
        run("menu-status") { [weak self] result in
            guard let self else { return }
            if result.succeeded, let state = try? JSONDecoder().decode(HandStatus.self, from: result.output), state.schema_version == 1 {
                self.status = state
                self.lastFailure = nil
            } else {
                self.status = nil
                if self.lastFailure != "Status check timed out" { self.lastFailure = "Unable to read Hand status" }
            }
            self.render()
        }
    }

    private func perform(_ operation: String, arguments: [String] = []) {
        run(operation, arguments: arguments) { [weak self] result in
            guard let self else { return }
            if result.succeeded {
                self.quitFailure = nil
                self.refreshStatus()
            }
            else {
                // A failed mutation may have changed the service; invalidate
                // its old observation and let Refresh reconcile without retry.
                self.status = nil
                self.lastFailure = "Could not \(operation) Hand — refresh status"
                self.render()
            }
        }
    }

    @objc private func toggleKeepAwake() {
        guard !busy, status?.local.error == nil, let awake = status?.local.keep_awake, awake.can_change == true else { return }
        perform("keep-awake", arguments: [awake.enabled ? "off" : "on"])
    }

    private func reconcileSignIn() {
        guard let script = signInScript else { return }
        let files = FileManager.default
        let marker = script.deletingLastPathComponent().appendingPathComponent("owner")
        var active = false
        if let value = try? String(contentsOf: marker, encoding: .utf8) {
            let fields = value.split(maxSplits: 1, whereSeparator: { $0.isWhitespace })
            if fields.count == 2, let pid = Int32(fields[0]), pid > 1 {
                let formatter = DateFormatter()
                formatter.locale = Locale(identifier: "en_US_POSIX")
                formatter.timeZone = TimeZone(secondsFromGMT: 0)
                formatter.dateFormat = "EEE MMM d HH:mm:ss yyyy"
                var info = proc_bsdinfo()
                let size = Int32(MemoryLayout<proc_bsdinfo>.size)
                if let started = formatter.date(from: String(fields[1]).trimmingCharacters(in: .whitespacesAndNewlines)),
                   proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, size) == size {
                    active = info.pbi_start_tvsec == UInt64(started.timeIntervalSince1970)
                }
            }
        } else if files.fileExists(atPath: script.path),
                  Date().timeIntervalSince(signInStarted ?? .distantPast) < 30 {
            // Terminal may still be launching. The script publishes its PID and
            // creation time before exec, so PID reuse cannot prolong this lease.
            active = true
        }
        if !active {
            try? files.removeItem(at: script.deletingLastPathComponent())
            signInScript = nil
            signInStarted = nil
        }
    }

    private static func shellQuote(_ text: String) -> String {
        "'" + text.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    @objc private func signIn() {
        guard presentation.canSignIn else { return }
        let files = FileManager.default
        guard let terminal = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.Terminal") else {
            signInFailure = "Terminal is unavailable"
            render()
            return
        }
        let directory = files.temporaryDirectory.appendingPathComponent("nanocodex-sign-in-" + UUID().uuidString, isDirectory: true)
        let script = directory.appendingPathComponent("Sign In to Nanocodex.command")
        do {
            try files.createDirectory(at: directory, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            let marker = directory.appendingPathComponent("owner")
            // exec preserves the shell PID/start time. Reconcile actual process
            // lifetime rather than relying on an EXIT trap after a crash.
            let source = "#!/bin/sh\n[ -f \(Self.shellQuote(script.path)) ] || exit 1\n{ printf '%s ' \"$$\"; LC_ALL=C TZ=UTC /bin/ps -p \"$$\" -o lstart=; } > \(Self.shellQuote(marker.path + ".new"))\n/bin/mv \(Self.shellQuote(marker.path + ".new")) \(Self.shellQuote(marker.path)) || exit 1\n[ -f \(Self.shellQuote(script.path)) ] || exit 1\nexec \(Self.shellQuote(cli.path)) account login\n"
            guard files.createFile(atPath: script.path, contents: Data(source.utf8), attributes: [.posixPermissions: 0o700]) else {
                throw NSError(domain: "HandMenuBar", code: 1)
            }
            signInScript = script
            signInStarted = Date()
            signInFailure = nil
            render()
            let configuration = NSWorkspace.OpenConfiguration()
            configuration.activates = true
            NSWorkspace.shared.open([script], withApplicationAt: terminal, configuration: configuration) { [weak self] _, error in
                RunLoop.main.perform(inModes: [.common, .eventTracking]) {
                    guard let self else { return }
                    if error != nil {
                        try? files.removeItem(at: directory)
                        self.signInScript = nil
                        self.signInFailure = "Could not open sign-in in Terminal"
                    }
                    self.render()
                }
            }
        } catch {
            try? files.removeItem(at: directory)
            signInFailure = "Could not prepare Terminal sign-in"
            render()
        }
    }

    @objc private func toggleHand() { perform(status?.local.loaded == true ? "stop" : "start") }
    @objc private func restartHand() { perform("restart") }
    @objc private func openLog() {
        let path = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".nanocodex/service/daemon.log")
        NSWorkspace.shared.open(FileManager.default.fileExists(atPath: path.path) ? path : path.deletingLastPathComponent())
    }
    @objc private func copyStatus() {
        let view = presentation
        let lines = ["Nanocodex Hand"] + view.summary + view.resources + (signInFailure.map { [$0] } ?? [])
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(lines.joined(separator: "\n"), forType: .string)
    }
    @objc private func quitHand() {
        guard !quitting && signInScript == nil && (!busy || pendingOperation == "menu-status") else { return }
        quitting = true
        quitFailure = nil
        if busy, let observation = command {
            // A slow network observation must not prevent quitting. Only this
            // read-only child is cancelled; service actions remain serialized.
            commandGeneration += 1
            observation.terminate()
            after(1) {
                if observation.isRunning { kill(observation.processIdentifier, SIGKILL) }
            }
            command = nil
            busy = false
        }
        run("stop") { [weak self] result in
            guard let self else { return }
            if result.succeeded {
                NSApp.terminate(nil)
            } else {
                self.status = nil
                self.quitting = false
                self.lastFailure = nil
                self.quitFailure = "Could not stop Hand; menu remains open — refresh status"
                self.render()
            }
        }
    }
    func applicationWillTerminate(_ notification: Notification) {
        timer?.invalidate()
        // Quit Hand waits for Stop; OS termination must not cancel a controller.
        if command?.arguments?.last == "menu-status" { command?.terminate() }
    }
}

let arguments = Array(CommandLine.arguments.dropFirst())
if ![2, 3].contains(arguments.count) || (arguments.count == 3 && arguments[2] != "--permission-guide") || arguments[0] != "--cli" || !arguments[1].hasPrefix("/") {
    fputs("Usage: nanocodex-hand-menu-bar --cli /absolute/path/to/nanocodex [--permission-guide]\n", stderr)
    exit(64)
}
let application = NSApplication.shared
let delegate = HandMenuBar(cli: URL(fileURLWithPath: arguments[1]), guideOnly: arguments.contains("--permission-guide"))
application.delegate = delegate
application.run()
