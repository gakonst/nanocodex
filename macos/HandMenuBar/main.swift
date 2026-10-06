import AppKit
import Foundation
import Darwin

// Account credentials remain owned by the CLI. This process consumes only its
// safe status projection and launches the existing interactive login on request.
struct HandStatus: Decodable {
    struct Local: Decodable {
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
            lines.append("Local service: \(operation == "start" ? "Starting…" : operation == "stop" ? "Stopping…" : "Restarting…")")
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
        if busy {
            lines.append("Hands: Refreshing…")
        } else if let inventory = status?.inventory {
            switch inventory.state {
            case "ready":
                let connected = inventory.hands.filter { $0.health == "connected" }.count
                lines.append("Hands: \(inventory.hands.count) listed · \(connected) connected")
            case "signed_out": lines.append("Hands: Sign in to view")
            case "expired": lines.append("Hands: Sign in again to view")
            case "partial": lines.append("Hands: Some connections unavailable")
            case "network_error": lines.append("Hands: Server unavailable")
            case "permission_denied": lines.append("Hands: Access denied")
            default: lines.append("Hands: Status unavailable")
            }
            if inventory.state == "ready" || !inventory.hands.isEmpty {
                let groups: [(String, [String])] = [
                    ("Computers & workspaces", ["computer", "workspace", "hand"]),
                    ("Virtual machines", ["vm"]),
                    ("Screens", ["screen", "screen_only"])
                ]
                var known: Set<String> = []
                for (heading, kinds) in groups {
                    let hands = inventory.hands.filter { kinds.contains($0.kind) }.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
                    if !hands.isEmpty {
                        resources.append(heading)
                        for hand in hands {
                            known.insert(hand.id)
                            resources.append(resource(hand, complete: true))
                        }
                    }
                }
                let others = inventory.hands.filter { !known.contains($0.id) }
                if !others.isEmpty {
                    resources.append("Other connections")
                    resources.append(contentsOf: others.map { resource($0, complete: true) })
                }
                if resources.isEmpty { resources.append("No Hands registered") }
                if inventory.state != "ready" { resources.insert("Partial inventory · some connections unavailable", at: 0) }
            }
        } else { lines.append("Hands: Status unavailable") }
        if let failure { lines.append(failure) }
        return MenuPresentation(summary: lines, resources: resources,
            canToggle: !busy && failure == nil && localKnown && local?.installed == true && !waiting,
            canRestart: !busy && failure == nil && localKnown && running,
            canSignIn: !busy && !signingIn && ["signed_out", "expired"].contains(accountState),
            stop: local?.loaded == true,
            warning: failure != nil || (!checking && (local?.error != nil || ["expired", "network_error", "permission_denied", "unknown"].contains(accountState) || ["partial", "network_error", "permission_denied", "unknown"].contains(status?.inventory.state ?? "unknown"))))
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

final class HandMenuBar: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private let cli: URL
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
    private var refreshTicks = 0

    init(cli: URL) { self.cli = cli; super.init() }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
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

    private func add(_ title: String, _ action: Selector? = nil, enabled: Bool = false) {
        let row = NSMenuItem(title: title, action: action, keyEquivalent: "")
        row.target = action == nil ? nil : self
        row.isEnabled = enabled
        menu.addItem(row)
    }

    func menuWillOpen(_ menu: NSMenu) { refreshStatus() }
    func applicationDidBecomeActive(_ notification: Notification) { refreshStatus() }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        item?.button?.performClick(nil)
        return false
    }

    private var presentation: MenuPresentation {
        MenuPresentation.make(status: status, busy: busy, operation: pendingOperation,
                              failure: lastFailure, signingIn: signInScript != nil)
    }

    private func render() {
        let view = presentation
        menu.removeAllItems()
        add("Nanocodex Hand · \(MenuPresentation.text(Host.current().localizedName ?? "This Mac"))")
        for line in view.summary { add(line) }
        if let quitFailure { add(quitFailure) }
        if !view.resources.isEmpty {
            menu.addItem(.separator())
            // Every connection remains accessible in the native menu.
            for line in view.resources.prefix(35) { add(line) }
            if view.resources.count > 35 {
                let overflow = NSMenuItem(title: "More connections…", action: nil, keyEquivalent: "")
                let submenu = NSMenu()
                submenu.autoenablesItems = false
                for line in view.resources.dropFirst(35) {
                    let row = NSMenuItem(title: line, action: nil, keyEquivalent: "")
                    row.isEnabled = false
                    submenu.addItem(row)
                }
                overflow.submenu = submenu
                menu.addItem(overflow)
            }
        }
        menu.addItem(.separator())
        add(signInScript == nil ? "Sign In…" : "Sign-in Open in Terminal", #selector(signIn), enabled: view.canSignIn)
        if let signInFailure { add(signInFailure) }
        add(view.stop ? "Stop Hand" : "Start Hand", #selector(toggleHand), enabled: view.canToggle)
        add("Restart Hand", #selector(restartHand), enabled: view.canRestart)
        add("Refresh Status", #selector(refreshStatus), enabled: !busy)
        menu.addItem(.separator())
        add("Open Hand Log", #selector(openLog), enabled: true)
        add("Copy Status", #selector(copyStatus), enabled: true)
        menu.addItem(.separator())
        add("Quit Hand", #selector(quitHand), enabled: !quitting && signInScript == nil && (!busy || pendingOperation == "menu-status"))
        item?.button?.toolTip = view.summary.joined(separator: "\n")
        item?.button?.setAccessibilityLabel("Nanocodex Hand · " + view.summary.dropFirst().joined(separator: ". "))
        item?.button?.image = NSImage(systemSymbolName: view.warning ? "exclamationmark.triangle" : "hand.raised.fill", accessibilityDescription: "Nanocodex Hand")
        item?.button?.image?.isTemplate = true
    }

    // Serialized child processes keep every network read off the AppKit thread.
    private func run(_ operation: String, completion: @escaping (CommandResult) -> Void) {
        guard !busy else { return }
        busy = true
        pendingOperation = operation
        commandGeneration += 1
        let generation = commandGeneration
        render()
        let child = Process()
        child.executableURL = cli
        child.arguments = ["hand", operation]
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
            DispatchQueue.main.async {
                guard let self, self.commandGeneration == generation else { return }
                self.command = nil
                self.busy = false
                completion(result)
            }
        }
        // Mutations retain the controller's deadline, including graceful Stop.
        // Only read-only observations may be terminated by the companion.
        if operation == "menu-status" {
            DispatchQueue.main.asyncAfter(deadline: .now() + 20) { [weak self, weak child] in
                guard let self, self.commandGeneration == generation, let child, child.isRunning else { return }
                self.lastFailure = "Status check timed out"
                self.status = nil
                child.terminate()
                self.render()
                DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self, weak child] in
                    guard let self, self.commandGeneration == generation, let child, child.isRunning else { return }
                    kill(child.processIdentifier, SIGKILL)
                }
            }
        }
    }

    @objc private func refreshStatus() {
        guard !busy else { return }
        if let script = signInScript, !FileManager.default.fileExists(atPath: script.path) {
            signInScript = nil
        }
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

    private func perform(_ operation: String) {
        run(operation) { [weak self] result in
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
            let cleanup = "/bin/rm -f -- \(Self.shellQuote(script.path)); /bin/rmdir -- \(Self.shellQuote(directory.path))"
            let source = "#!/bin/sh\ntrap \(Self.shellQuote(cleanup)) EXIT\n\(Self.shellQuote(cli.path)) account login\nresult=$?\nprintf '\\nNanocodex sign-in finished. You may close this window.\\n'\nexit \"$result\"\n"
            guard files.createFile(atPath: script.path, contents: Data(source.utf8), attributes: [.posixPermissions: 0o700]) else {
                throw NSError(domain: "HandMenuBar", code: 1)
            }
            signInScript = script
            signInFailure = nil
            render()
            let configuration = NSWorkspace.OpenConfiguration()
            configuration.activates = true
            NSWorkspace.shared.open([script], withApplicationAt: terminal, configuration: configuration) { [weak self] _, error in
                DispatchQueue.main.async {
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
            DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
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
if arguments.count != 2 || arguments[0] != "--cli" || !arguments[1].hasPrefix("/") {
    fputs("Usage: nanocodex-hand-menu-bar --cli /absolute/path/to/nanocodex\n", stderr)
    exit(64)
}
let application = NSApplication.shared
let delegate = HandMenuBar(cli: URL(fileURLWithPath: arguments[1]))
application.delegate = delegate
application.run()
