import AppKit
import Foundation
import Darwin

// This process only observes and controls the independently installed Hand.
// It has no account login, chat runtime, screen publisher, or credential store.
struct HandStatus: Decodable {
    let installed: Bool
    let loaded: Bool
    let pid: Int?
}

struct CommandResult {
    let succeeded: Bool
    let output: Data
}

final class HandMenuBar: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private let cli: URL
    private var item: NSStatusItem?
    private let menu = NSMenu()
    private let stateRow = NSMenuItem(title: "Checking Hand…", action: nil, keyEquivalent: "")
    private let detailRow = NSMenuItem(title: "", action: nil, keyEquivalent: "")
    private let toggleRow = NSMenuItem(title: "Start Hand", action: #selector(toggleHand), keyEquivalent: "")
    private let restartRow = NSMenuItem(title: "Restart Hand", action: #selector(restartHand), keyEquivalent: "")
    private var status: HandStatus?
    private var busy = false
    private var pendingOperation = "status"
    private var timer: Timer?
    private var command: Process?
    private var lastFailure: String?
    private var commandGeneration = 0

    init(cli: URL) { self.cli = cli; super.init() }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        let autosaveName = "NanocodexStandaloneHand"
        let positionKey = "NSStatusItem Preferred Position " + autosaveName
        // AppKit's default insertion at the left edge of menu extras can hide
        // a new icon behind the camera housing. Seed only our first position;
        // preserve every subsequent user arrangement saved by AppKit.
        if UserDefaults.standard.object(forKey: positionKey) == nil {
            UserDefaults.standard.set(180, forKey: positionKey)
        }
        let statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        statusItem.autosaveName = autosaveName
        statusItem.isVisible = true
        statusItem.button?.image = NSImage(systemSymbolName: "hand.raised.fill", accessibilityDescription: "Nanocodex Hand")
        statusItem.button?.image?.isTemplate = true
        statusItem.button?.setAccessibilityIdentifier("nanocodex-hand-menu")
        item = statusItem
        menu.autoenablesItems = false
        menu.delegate = self
        let name = Host.current().localizedName ?? "This Mac"
        let title = NSMenuItem(title: "Nanocodex Hand · \(name)", action: nil, keyEquivalent: "")
        title.isEnabled = false
        menu.addItem(title)
        stateRow.isEnabled = false
        detailRow.isEnabled = false
        menu.addItem(stateRow)
        menu.addItem(detailRow)
        menu.addItem(.separator())
        for row in [toggleRow, restartRow] { row.target = self; row.isEnabled = false; menu.addItem(row) }
        add("Refresh Status", #selector(refreshStatus))
        menu.addItem(.separator())
        add("Open Hand Log", #selector(openLog))
        add("Copy Status", #selector(copyStatus))
        menu.addItem(.separator())
        let note = NSMenuItem(title: "The Hand keeps running when this menu quits.", action: nil, keyEquivalent: "")
        note.isEnabled = false
        menu.addItem(note)
        add("Quit Menu Bar", #selector(quitMenuBar))
        statusItem.menu = menu
        refreshStatus()
        timer = Timer.scheduledTimer(withTimeInterval: 30, repeats: true) { [weak self] _ in self?.refreshStatus() }
    }

    private func add(_ title: String, _ action: Selector) {
        let row = NSMenuItem(title: title, action: action, keyEquivalent: "")
        row.target = self
        menu.addItem(row)
    }

    func menuWillOpen(_ menu: NSMenu) { refreshStatus() }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        // Opening this small utility again reveals its controls without a
        // document window or another Hand/menu process.
        item?.button?.performClick(nil)
        return false
    }

    private var pendingLogin: Bool {
        let path = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/LaunchAgents/com.nanocodex.hand.plist")
        guard let data = try? Data(contentsOf: path),
              let plist = try? PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any] else { return false }
        return plist["NanocodexPendingLogin"] as? Bool == true
    }

    private func render() {
        let running = status?.pid != nil && status?.loaded == true
        let waiting = status?.installed == true && !running && pendingLogin
        let description: String
        if busy {
            switch pendingOperation {
            case "start": description = "Starting Hand…"
            case "stop": description = "Stopping Hand…"
            case "restart": description = "Restarting Hand…"
            default: description = "Checking Hand…"
            }
        }
        else if let failure = lastFailure { description = failure }
        else if let state = status {
            description = running ? "Hand running" : waiting ? "Sign in to connect this Mac" : state.loaded ? "Hand starting…" : state.installed ? "Hand stopped" : "Hand not installed"
        } else { description = "Checking Hand…" }
        stateRow.title = description
        if let state = status, let pid = state.pid, running {
            detailRow.title = "Service process \(pid)"
        } else if waiting {
            detailRow.title = "Run nanocodex account login in Terminal"
        } else if status?.installed == false {
            detailRow.title = "Run nanocodex hand install in Terminal"
        } else {
            detailRow.title = "Runs independently of the Nanocodex Mac app"
        }
        // A process receipt establishes service liveness, not account connectivity.
        item?.button?.toolTip = "Nanocodex Hand · \(description)"
        item?.button?.setAccessibilityLabel("Nanocodex Hand · \(description)")
        item?.button?.image = NSImage(systemSymbolName: lastFailure != nil ? "exclamationmark.triangle" : running ? "hand.raised.fill" : "hand.raised", accessibilityDescription: "Nanocodex Hand")
        item?.button?.image?.isTemplate = true
        toggleRow.title = status?.loaded == true ? "Stop Hand" : "Start Hand"
        toggleRow.isEnabled = !busy && lastFailure == nil && status?.installed == true && !waiting
        restartRow.isEnabled = !busy && lastFailure == nil && status?.loaded == true && running
    }

    // Read output off the main thread. There is only one bounded command at a
    // time, so opening the menu cannot overlap a Start/Stop request.
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
        child.standardOutput = output
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
            let data = output.fileHandleForReading.readDataToEndOfFile()
            child.waitUntilExit()
            let result = CommandResult(succeeded: child.terminationStatus == 0, output: data)
            DispatchQueue.main.async {
                guard let self, self.commandGeneration == generation else { return }
                self.command = nil
                self.busy = false
                completion(result)
            }
        }
        // The service controller owns mutation deadlines (including long
        // graceful shutdowns and update recovery). Cancelling it on a shorter
        // UI timer could interrupt a valid Stop/Restart. Only observations are
        // disposable; mutations remain serialized until the controller returns.
        if operation == "status" {
            DispatchQueue.main.asyncAfter(deadline: .now() + 10) { [weak self, weak child] in
                guard let self, self.commandGeneration == generation, let child, child.isRunning else { return }
                child.terminate()
                self.lastFailure = "Hand status timed out"
                self.render()
                DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self, weak child] in
                    guard let self, self.commandGeneration == generation, let child, child.isRunning else { return }
                    // This command is a read-only status observation. A child
                    // ignoring SIGTERM must not freeze the menu indefinitely.
                    kill(child.processIdentifier, SIGKILL)
                }
            }
        }
    }

    @objc private func refreshStatus() {
        guard !busy else { return }
        run("status") { [weak self] result in
            guard let self else { return }
            if result.succeeded, let state = try? JSONDecoder().decode(HandStatus.self, from: result.output) {
                self.status = state
                self.lastFailure = nil
            } else {
                self.status = nil
                self.lastFailure = "Unable to read Hand status"
            }
            self.render()
        }
    }

    private func perform(_ operation: String) {
        run(operation) { [weak self] result in
            guard let self else { return }
            if result.succeeded { self.refreshStatus() }
            else {
                // A failed action may still have changed the service. Never
                // automatically repeat it; the next observation reconciles it.
                self.lastFailure = "Could not \(operation) Hand — refresh status"
                self.render()
            }
        }
    }

    @objc private func toggleHand() { perform(status?.loaded == true ? "stop" : "start") }
    @objc private func restartHand() { perform("restart") }
    @objc private func openLog() {
        let path = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".nanocodex/service/daemon.log")
        if FileManager.default.fileExists(atPath: path.path) { NSWorkspace.shared.open(path) }
        else { NSWorkspace.shared.open(path.deletingLastPathComponent()) }
    }
    @objc private func copyStatus() {
        let text = "Nanocodex Hand\n\(stateRow.title)\n\(detailRow.title)"
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
    @objc private func quitMenuBar() { NSApp.terminate(nil) }
    func applicationWillTerminate(_ notification: Notification) {
        timer?.invalidate()
        // Only a status observation is safe to cancel. A pending service action
        // retains its outcome and must not be replayed by another menu process.
        if command?.arguments?.last == "status" { command?.terminate() }
    }
}

let arguments = CommandLine.arguments
if arguments.count != 3 || arguments[1] != "--cli" || !arguments[2].hasPrefix("/") {
    fputs("Usage: nanocodex-hand-menu-bar --cli /absolute/path/to/nanocodex\n", stderr)
    exit(64)
}
let application = NSApplication.shared
let delegate = HandMenuBar(cli: URL(fileURLWithPath: arguments[2]))
application.delegate = delegate
application.run()
