import AppKit
import Combine
import SwiftUI
import NanocodexUI

/// Owned by the app's model, so closing a window cannot end Hand activity.
@MainActor
final class HandBackgroundActivity {
    private var activity: NSObjectProtocol?
    private(set) var options: ProcessInfo.ActivityOptions?

    static func options(running: Bool, keepAwake: Bool) -> ProcessInfo.ActivityOptions? {
        guard running else { return nil }
        // Keep the Hand responsive under App Nap without keeping the display on.
        return keepAwake ? .userInitiated : .userInitiatedAllowingIdleSystemSleep
    }

    func update(running: Bool, keepAwake: Bool) {
        let next = Self.options(running: running, keepAwake: keepAwake)
        guard next != options else { return }
        stop()
        if let next {
            activity = ProcessInfo.processInfo.beginActivity(options: next, reason: "Nanocodex Hands running in the background")
            options = next
        }
    }

    func stop() {
        if let activity { ProcessInfo.processInfo.endActivity(activity) }
        activity = nil; options = nil
    }
}

@MainActor
final class HandStatusItem: NSObject, NSPopoverDelegate {
    private let model: AppModel
    private let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    private let popover = NSPopover()
    private var observation: AnyCancellable?
    private var dismissalObservation: AnyCancellable?
    private var eventMonitors: [Any] = []

    init(model: AppModel, openMainWindow: @escaping () -> Void) {
        self.model = model
        super.init()
        item.button?.image = NSImage(systemSymbolName: "hand.raised.fill", accessibilityDescription: "Nanocodex")
        item.button?.image?.isTemplate = true
        item.button?.imagePosition = .imageLeading
        item.button?.font = .systemFont(ofSize: 12, weight: .medium)
        item.button?.target = self; item.button?.action = #selector(toggle)
        item.button?.setAccessibilityIdentifier("hand-control-panel")
        // Own dismissal so AppKit cannot close on mouse-down and let the
        // status button's mouse-up action immediately reopen the panel.
        popover.behavior = .applicationDefined
        popover.delegate = self
        popover.contentSize = NSSize(width: 720, height: 560)
        popover.contentViewController = NSHostingController(rootView: HandControlPanel(model: model, openMainWindow: { [weak self] in
            self?.popover.performClose(nil)
            openMainWindow()
            NSApp.activate(ignoringOtherApps: true)
        }))
        observation = model.objectWillChange.sink { [weak self] in
            // Published values change after objectWillChange is delivered.
            Task { @MainActor [weak self] in self?.updateTitle() }
        }
        updateTitle()
    }

    private func updateTitle() {
        let count = model.connectedHandCount
        let hands = model.runtimeFailed || !model.state.connected ? "offline"
            : "\(count) \(count == 1 ? "Hand" : "Hands")"
        let activity = model.runningCount > 0 ? " · \(model.runningCount) running" : ""
        let title = "  Nanocodex · \(hands)\(activity)"
        if item.button?.title != title { item.button?.title = title }
        item.button?.toolTip = "Agent control panel · \(model.backgroundHandStatus)"
    }

    @objc private func toggle() {
        if popover.isShown { popover.performClose(nil) } else { show() }
    }

    func show() {
        guard !popover.isShown, let button = item.button else { return }
        popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        popover.contentViewController?.view.window?.makeKey()
        guard popover.isShown else { return }
        // Handle outside clicks ourselves, leaving the status button's click
        // exclusively to toggle(). This also works while another app is active.
        let clicks: NSEvent.EventTypeMask = [.leftMouseDown, .rightMouseDown, .otherMouseDown]
        if let monitor = NSEvent.addLocalMonitorForEvents(matching: clicks.union(.keyDown), handler: { [weak self] event in
            if event.type == .keyDown {
                if event.keyCode == 53 { self?.popover.performClose(nil); return nil }
                return event
            }
            if let self, event.window !== self.popover.contentViewController?.view.window,
               event.window !== self.item.button?.window {
                self.popover.performClose(nil)
            }
            // Dismiss without swallowing the click destined for the other window.
            return event
        }) { eventMonitors.append(monitor) }
        if let monitor = NSEvent.addGlobalMonitorForEvents(matching: clicks, handler: { [weak self] _ in
            self?.popover.performClose(nil)
        }) { eventMonitors.append(monitor) }
        dismissalObservation = NotificationCenter.default.publisher(for: NSApplication.didResignActiveNotification)
            .sink { [weak self] _ in self?.popover.performClose(nil) }
    }

    func popoverWillClose(_ notification: Notification) {
        eventMonitors.forEach(NSEvent.removeMonitor); eventMonitors.removeAll()
        dismissalObservation = nil
    }

    deinit { eventMonitors.forEach(NSEvent.removeMonitor) }
}

struct HandControlPanel: View {
    @ObservedObject var model: AppModel
    var openMainWindow: () -> Void
    @State private var stoppingAgents = Set<String>()
    private var online: Bool { model.state.connected && !model.runtimeFailed }
    private var agents: [WorkspaceTab] {
        let tabs = model.tabs.filter { $0.threadId != nil || model.working($0.id) }
        return tabs.filter { model.update(for: $0).running } + tabs.filter { !model.update(for: $0).running }
    }

    var body: some View {
        VStack(spacing: 0) {
            VStack(spacing: 18) {
                HStack(spacing: 11) {
                    Image(nsImage: NSImage(named: "icon") ?? NSImage()).resizable().frame(width: 36, height: 36)
                    VStack(alignment: .leading, spacing: 3) {
                        Text("Nanocodex").font(.system(size: 19, weight: .semibold)).tracking(-0.5)
                        Text("Agent control panel").font(.system(size: 12)).foregroundStyle(.secondary)
                    }
                    Spacer()
                    HStack(spacing: 5) {
                        Circle().fill(online ? Color.green : .orange).frame(width: 6, height: 6)
                        Text(online ? "LIVE" : "OFFLINE").font(.system(size: 10, weight: .semibold)).tracking(1)
                    }.padding(.horizontal, 10).padding(.vertical, 6)
                        .background((online ? Color.green : .orange).opacity(0.09), in: Capsule())
                }
                HStack(spacing: 10) {
                    metric("RUNNING", value: online ? model.runningCount : 0, symbol: "waveform.path", color: .green)
                    metric("TO REVIEW", value: model.attentionCount, symbol: "tray", color: .orange)
                    metric("HANDS ONLINE", value: online ? model.connectedHandCount : 0, symbol: "hand.raised", color: .blue)
                }
            }.padding(20)
                .background(LinearGradient(colors: [Color.green.opacity(0.045), .clear], startPoint: .topLeading, endPoint: .bottomTrailing))
            Divider()
            VStack(alignment: .leading, spacing: 14) {
                if model.runtimeFailed || !model.state.connected {
                    Label(model.backgroundHandStatus, systemImage: "exclamationmark.circle")
                        .font(.system(size: 12)).foregroundStyle(.orange)
                }
                HStack(alignment: .top, spacing: 16) {
                    VStack(alignment: .leading, spacing: 8) {
                        sectionHeader("OPEN AGENTS", count: agents.count)
                        ScrollView {
                            LazyVStack(spacing: 8) {
                                if agents.isEmpty {
                                    HStack(spacing: 12) {
                                        Image(systemName: "sparkles").font(.system(size: 20)).foregroundStyle(.tertiary)
                                        VStack(alignment: .leading, spacing: 4) {
                                            Text("Ready when you are").font(.system(size: 13, weight: .medium))
                                            Text("Start a task in Nanocodex. Follow its progress here.").font(.system(size: 12)).foregroundStyle(.secondary)
                                        }
                                    }.padding(14).frame(maxWidth: .infinity, alignment: .leading).background(cardBackground, in: RoundedRectangle(cornerRadius: 12))
                                }
                                ForEach(agents) { agentRow($0) }
                            }
                        }
                    }
                    .frame(maxWidth: .infinity)
                    Divider()
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            sectionHeader("HANDS", count: model.state.hands.count + model.otherAccountHands.count)
                            Spacer()
                            Button("Manage", systemImage: "arrow.up.right") { model.screen = .hands; openMainWindow() }
                                .font(.system(size: 11)).buttonStyle(.plain).foregroundStyle(.secondary)
                        }
                        ScrollView {
                            LazyVStack(alignment: .leading, spacing: 8) {
                                if model.state.hands.isEmpty {
                                    Text("Connect a Hand to run tools on this Mac.").font(.system(size: 12)).foregroundStyle(.secondary).padding(.vertical, 8)
                                }
                                ForEach(model.state.hands) { handRow($0) }
                                ForEach(model.otherAccountHands) { hand in
                                    AccountHandRow(hand: hand, compact: true) {
                                        model.useAccountHand(hand); openMainWindow()
                                    }
                                }
                                if let error = model.state.accountHandsError {
                                    Text(error).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                        }
                    }
                    .frame(width: 280)
                }
            }.padding(20).frame(maxWidth: .infinity, maxHeight: .infinity)
            Divider()
            VStack(alignment: .leading, spacing: 7) {
                Toggle("Make this Mac available as a Hand", isOn: Binding(get: { model.state.defaultHandEnabled != false }, set: { enabled in Task { await model.setDeviceHandEnabled(enabled) } }))
                    .toggleStyle(.checkbox).disabled(!online).accessibilityIdentifier("menu-device-hand-enabled")
                Toggle(isOn: $model.keepMacAwake) {
                    Label("Keep Mac awake while Hands are running", systemImage: model.keepMacAwake ? "sun.max.fill" : "moon")
                        .font(.system(size: 12, weight: .medium))
                }.toggleStyle(.checkbox).accessibilityIdentifier("menu-keep-mac-awake")
                Text("Hands stay connected with the window closed. Keeping awake uses more battery; lid-close and manual sleep can still pause work.")
                    .font(.system(size: 11)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }.padding(.horizontal, 20).padding(.vertical, 15)
            Divider()
            HStack {
                Button { openMainWindow() } label: {
                    Label("Open Nanocodex", systemImage: "arrow.up.right.square").font(.system(size: 12, weight: .medium))
                }.buttonStyle(.plain).accessibilityIdentifier("control-panel-open-app")
                Spacer()
                Button { model.showingSettings = true; openMainWindow() } label: { Image(systemName: "gearshape") }
                    .help("Settings").accessibilityLabel("Settings")
                Button("Quit") { NSApp.terminate(nil) }.help("Quit Nanocodex and stop this Mac’s Hands")
            }.buttonStyle(.borderless).font(.system(size: 12)).padding(.horizontal, 20).padding(.vertical, 13)
        }
        .frame(width: 720, height: 560)
        .background(ChatPalette.background)
        .preferredColorScheme(model.preferredColorScheme)
        .accessibilityIdentifier("agent-control-panel")
    }

    private var cardBackground: Color { Color.primary.opacity(0.035) }
    private func metric(_ title: String, value: Int, symbol: String, color: Color) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack {
                Text("\(value)").font(.system(size: 25, weight: .medium, design: .rounded)).monospacedDigit()
                Spacer()
                Image(systemName: symbol).font(.system(size: 14)).foregroundStyle(color)
            }
            Text(title).font(.system(size: 9, weight: .semibold)).tracking(0.7).foregroundStyle(.secondary)
        }.padding(12).frame(maxWidth: .infinity, alignment: .leading)
            .background(cardBackground, in: RoundedRectangle(cornerRadius: 12))
    }
    private func sectionHeader(_ title: String, count: Int) -> some View {
        HStack(spacing: 7) {
            Text(title).tracking(1)
            Text("\(count)").monospacedDigit().foregroundStyle(.tertiary)
        }.font(.system(size: 10, weight: .semibold)).foregroundStyle(.secondary)
    }
    func agentStatus(_ tab: WorkspaceTab) -> String {
        let update = model.update(for: tab)
        let failed = model.hasAttentionError(tab)
        return !online ? "Offline" : failed ? "Needs attention" : update.running ? (model.running(tab.id) ? "Running" : "Queued") : update.needsAttention(tab) ? "Ready for review" : "Idle"
    }
    private func agentRow(_ tab: WorkspaceTab) -> some View {
        let update = model.update(for: tab)
        let running = online && update.running
        let failed = model.hasAttentionError(tab)
        let color: Color = failed ? .orange : running ? .green : .secondary
        let status = agentStatus(tab)
        return HStack(spacing: 12) {
            Circle().fill(color).frame(width: 7, height: 7)
            Button {
                model.select(tab.id); openMainWindow()
            } label: {
                VStack(alignment: .leading, spacing: 5) {
                    Text(model.title(tab)).font(.system(size: 13, weight: .medium)).lineLimit(1)
                    HStack(spacing: 5) {
                        Text(status).foregroundStyle(color)
                        if running, let activity = model.transcript(tab.id).last(where: \.isActivity) {
                            Text("· " + activity.activityTitle).foregroundStyle(.secondary).lineLimit(1)
                        }
                    }.font(.system(size: 11))
                }.frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
            }.buttonStyle(.plain).accessibilityIdentifier("control-panel-agent-\(tab.id)")
            if running {
                Button {
                    stoppingAgents.insert(tab.id)
                    Task { await model.cancel(tabID: tab.id); stoppingAgents.remove(tab.id) }
                } label: { Image(systemName: "stop.fill").font(.system(size: 10)).frame(width: 26, height: 26) }
                    .buttonStyle(.borderless).background(Color.primary.opacity(0.05), in: Circle())
                    .disabled(stoppingAgents.contains(tab.id) || model.controllableTurns(tab.id).isEmpty)
                    .help("Stop the current turn").accessibilityLabel("Stop \(model.title(tab))")
            } else { Image(systemName: "arrow.up.right").font(.system(size: 11)).foregroundStyle(.tertiary) }
        }.padding(12).background(cardBackground, in: RoundedRectangle(cornerRadius: 12))
    }
    private func handRow(_ hand: Hand) -> some View {
        let connected = online && hand.status == "connected"
        let status = !online ? "Offline" : (hand.status ?? "stopped").capitalized
        return HStack(spacing: 12) {
            Image(systemName: hand.kind == "vm" ? "shippingbox" : "desktopcomputer")
                .font(.system(size: 17)).foregroundStyle(connected ? Color.blue : .secondary)
                .frame(width: 34, height: 34).background(Color.blue.opacity(0.06), in: RoundedRectangle(cornerRadius: 9))
            VStack(alignment: .leading, spacing: 4) {
                Text(hand.name).font(.system(size: 13, weight: .medium)).lineLimit(1)
                Text(status + (connected ? " · \(hand.activeCalls ?? 0) active calls" : ""))
                    .font(.system(size: 11)).foregroundStyle(hand.status == "error" ? Color.orange : .secondary)
            }.frame(maxWidth: .infinity, alignment: .leading)
            Button(hand.isRunning ? "Stop" : "Connect") {
                Task { if hand.isRunning { await model.stopHand(hand.id) } else { await model.startHand(hand.id) } }
            }.controlSize(.small).disabled(!online || model.busyHands.contains(hand.id))
                .accessibilityIdentifier("control-panel-hand-\(hand.id)")
        }.padding(12).background(cardBackground, in: RoundedRectangle(cornerRadius: 12))
    }
}
