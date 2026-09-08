import AppKit
import SwiftUI
import NanocodexRemote

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    var model: AppModel?
    private var startup: Task<Void, Never>?
    private var openMainWindow: (() -> Void)?
    private var handStatusItem: HandStatusItem?
    private var terminating = false
    func start(model: AppModel, openMainWindow: @escaping () -> Void) {
        self.openMainWindow = openMainWindow
        guard self.model == nil else { return }
        self.model = model
        handStatusItem = HandStatusItem(model: model, openMainWindow: openMainWindow)
        // A window's .task is cancelled when it closes; startup belongs to the app.
        startup = Task { await model.start() }
    }
    func showControlPanel() { handStatusItem?.show() }
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSWindow.allowsAutomaticWindowTabbing = false
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
    }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag { openMainWindow?() }
        if let window = sender.windows.first(where: { $0.canBecomeMain }) {
            window.deminiaturize(nil); window.makeKeyAndOrderFront(nil)
        }
        return true
    }
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let model else { return .terminateNow }
        guard !terminating else { return .terminateLater }; terminating = true
        startup?.cancel()
        Task { await model.prepareToQuit(); sender.reply(toApplicationShouldTerminate: true) }
        return .terminateLater
    }
    func applicationWillTerminate(_ notification: Notification) { model?.shutdown() }
}

@main
struct NanocodexApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var model = AppModel()
    @Environment(\.openWindow) private var openWindow
    var body: some Scene {
        Window("Nanocodex", id: "main") {
            ContentView()
                .environmentObject(model)
                .preferredColorScheme(model.preferredColorScheme)
                .frame(minWidth: 820, minHeight: 600)
                .onAppear {
                    if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] == nil {
                        delegate.start(model: model, openMainWindow: { openWindow(id: "main") })
                    }
                }
        }
        .defaultSize(width: 1440, height: 900)
        .windowToolbarStyle(.unifiedCompact)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("New Tab") { model.newTab() }.keyboardShortcut("t")
                Button("New Thread") { model.newTab() }.keyboardShortcut("n")
                Button("Close Tab") { model.closeActiveWorkspace() }.keyboardShortcut("w")
                Button("Reopen Closed Tab") { model.reopenTab() }.keyboardShortcut("t", modifiers: [.command, .shift])
            }
            CommandGroup(replacing: .appSettings) {
                Button("Settings…") { openWindow(id: "main"); model.showingSettings = true }.keyboardShortcut(",")
            }
            CommandMenu("Tabs") {
                Button("Next Tab") { model.cycleWorkspace(1) }.keyboardShortcut("]", modifiers: [.command, .shift])
                Button("Previous Tab") { model.cycleWorkspace(-1) }.keyboardShortcut("[", modifiers: [.command, .shift])
                Divider()
                Button("Search Threads…") { model.showingSearch = true }.keyboardShortcut("k")
            }
            CommandMenu("Workspace") {
                Button("New Agent to the Right") { model.splitAgent(axis: "horizontal") }.keyboardShortcut("\\")
                Button("New Agent Below") { model.splitAgent(axis: "vertical") }.keyboardShortcut("j", modifiers: [.command, .option])
                Button("Open Agent to the Right…") { model.showingPanePicker = true }.keyboardShortcut("\\", modifiers: [.command, .shift])
                Divider()
                Button("Inbox") { model.setFilter(.inbox) }.keyboardShortcut("1", modifiers: [.command, .option])
                Button("Running") { model.setFilter(.running) }.keyboardShortcut("2", modifiers: [.command, .option])
                Button("All Agents") { model.setFilter(.all) }.keyboardShortcut("3", modifiers: [.command, .option])
                Divider()
                Button("Previous Pane") { model.cyclePane(-1) }.keyboardShortcut(.leftArrow, modifiers: [.command, .option])
                Button("Next Pane") { model.cyclePane(1) }.keyboardShortcut(.rightArrow, modifiers: [.command, .option])
                Button("Move Pane Left") { model.movePane(-1) }.keyboardShortcut(.leftArrow, modifiers: [.command, .option, .shift])
                Button("Move Pane Right") { model.movePane(1) }.keyboardShortcut(.rightArrow, modifiers: [.command, .option, .shift])
                Button("Toggle Focus Mode") { model.toggleFocusMode() }.keyboardShortcut("f", modifiers: [.command, .shift])
                Divider()
                Button("Mark Update Seen") { model.review(model.activeTabID, seen: true) }.keyboardShortcut("d").disabled(model.activeTab == nil)
                Button("Revisit Later") { model.review(model.activeTabID, seen: false) }.keyboardShortcut("d", modifiers: [.command, .shift]).disabled(model.activeTab == nil)
            }
            CommandMenu("Agent") {
                Button("Agent Control Panel") { delegate.showControlPanel() }.keyboardShortcut("p", modifiers: [.command, .shift])
                Button("Stop Current Turn") { Task { await model.cancel() } }.keyboardShortcut(".").disabled(!model.isRunning)
                Button("Hands") { model.screen = .hands }.keyboardShortcut("h", modifiers: [.command, .shift])
                Button("Remote Screens") { model.showingScreens = true }.disabled(model.remoteService == nil)
                Button("Connections") { model.openAccount() }
                Button("Refresh") { Task { await model.refresh() } }.keyboardShortcut("r")
            }
        }
    }
}

struct ContentView: View {
    @EnvironmentObject private var model: AppModel
    var body: some View {
        Group {
            if model.isStarting {
                ProgressView("Connecting…").controlSize(.small).frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if model.showsOnboarding {
                OnboardingView()
            } else {
                workspace
            }
        }
        .toolbar {
            ToolbarItem(placement: .principal) { Text("Nanocodex").font(.system(size: 13, weight: .medium)).foregroundStyle(.secondary) }
            ToolbarItem { RemoteSharingIndicator(host: model.remoteMacHost, phoneHost: model.remotePhoneHost) }
        }
        .sheet(isPresented: $model.showingSettings) { SettingsView() }
        .sheet(isPresented: $model.showingScreens) {
            VStack {
                HStack { Spacer(); Button("Done") { model.showingScreens = false } }.padding([.top, .trailing])
                if let service = model.remoteService {
                    RemoteDashboard(service: service, host: model.remoteMacHost, phoneHost: model.remotePhoneHost).id(ObjectIdentifier(service))
                }
            }.frame(minWidth: 840, minHeight: 600)
        }
    }
    private var workspace: some View {
        HStack(spacing: 0) {
            VStack(spacing: 0) {
                TopTabsView()
                if let error = model.error ?? model.state.error {
                    HStack(spacing: 10) {
                        Image(systemName: "exclamationmark.circle").foregroundStyle(.orange)
                        Text(error).font(.callout).textSelection(.enabled)
                        Spacer()
                        Button { model.error = nil; Task { await model.refresh() } } label: { Image(systemName: "arrow.clockwise") }.help("Retry connection")
                        Button { model.error = nil } label: { Image(systemName: "xmark") }.help("Dismiss")
                    }.buttonStyle(.plain).padding(12).background(Color.orange.opacity(0.07))
                }
                if model.screen == .hands { HandsView() } else { TiledWorkspaceView() }
            }
            .background(Color(nsColor: .textBackgroundColor))

        }
        .background(Color(nsColor: .windowBackgroundColor))
        .sheet(isPresented: $model.showingSearch) { ThreadSearchView() }
        .sheet(isPresented: $model.showingHandSetup) { HandSetupView(hand: model.editingHand) }
        .sheet(isPresented: $model.showingRemoteSetup) { RemoteSetupView() }
        .sheet(item: $model.selectedHandForLogs) { hand in HandLogView(id: hand.id) }
    }
}
