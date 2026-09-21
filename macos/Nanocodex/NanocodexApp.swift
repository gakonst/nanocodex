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
#if DEBUG
        // Exercise the actual SwiftUI Window and NSToolbar without starting
        // account services or device sharing during UI tests.
        if ProcessInfo.processInfo.environment["NANOCODEX_NATIVE_UI_FIXTURE"] == "1" {
            model.runtime.requestOverride = { _, _ in .null }
            model.state.connected = true
            model.state.defaultHandEnabled = false
            model.isStarting = false
            model.theme = ProcessInfo.processInfo.environment["NANOCODEX_NATIVE_UI_THEME"] ?? "system"
            model.workspaceFilter = .all
            model.tabs = [WorkspaceTab(id: "ui-first", threadId: "ui-thread", title: "Desktop workspace"), WorkspaceTab(id: "ui-second", title: "Release notes")]
            model.activeTabID = "ui-first"
            model.snapshots["ui-thread"] = ThreadSnapshot(id: "ui-thread", events: [], hasMore: false, connected: true, activeTurns: [], settings: AgentSettings())
            model.messages["ui-thread"] = [
                MessageEntry(id: "ui-user", turnId: "ui-turn", kind: .user, text: "Help me refine the desktop workspace."),
                MessageEntry(id: "ui-assistant", turnId: "ui-turn", kind: .assistant, text: "The workspace is ready. Your conversations, drafts, and screens stay together.\n\nUse **⌘K** to find a conversation or **⌘⇧O** to see your open tabs. Press **Esc**, then **v** or **h**, to arrange conversations side by side.\n\nSelect a working folder below when you’re ready to build.")
            ]
            return
        }
#endif
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
        .windowToolbarStyle(.unified)
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
                Button("Back") { model.navigateHistory(back: true) }.keyboardShortcut("[").disabled(!model.canGoBack)
                Button("Forward") { model.navigateHistory(back: false) }.keyboardShortcut("]").disabled(!model.canGoForward)
                Button("Tab Overview") { model.showingTabOverview = true }.keyboardShortcut("o", modifiers: [.command, .shift])
                Divider()
                Button("Next Tab") { model.cycleWorkspace(1) }.keyboardShortcut("]", modifiers: [.command, .shift])
                Button("Previous Tab") { model.cycleWorkspace(-1) }.keyboardShortcut("[", modifiers: [.command, .shift])
                Divider()
                Button("Search Threads…") { model.showingSearch = true }.keyboardShortcut("k")
            }
            CommandGroup(after: .toolbar) {
                Button("Zoom In") { model.changeZoom(1) }.keyboardShortcut("+", modifiers: .command).disabled(model.workspaceZoom >= 1.5)
                Button("Zoom Out") { model.changeZoom(-1) }.keyboardShortcut("-", modifiers: .command).disabled(model.workspaceZoom <= 0.75)
                Button("Actual Size") { model.resetZoom() }.keyboardShortcut("0", modifiers: .command)
            }
            CommandMenu("Workspace") {
                Button("Keyboard Shortcuts…") { model.showingKeyboardHelp = true }
                Divider()
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
                Button(model.showingScreens ? "Hide Remote Screens" : "Show Remote Screens") { model.showingScreens.toggle() }
                    .keyboardShortcut("s", modifiers: [.command, .option]).disabled(model.remoteService == nil)
                Button("Connections") { model.openAccount() }
                Button("Refresh") { Task { await model.refresh() } }.keyboardShortcut("r")
            }
        }
    }
}

/// Isolate the working split view from NavigationSplitView's native pane
/// context, which extends split views beneath the floating sidebar and toolbar.
private struct NativeWorkspaceHost: NSViewRepresentable {
    let model: AppModel
    func makeNSView(context: Context) -> NSHostingView<WorkspaceSplitContent> {
        let view = NSHostingView(rootView: WorkspaceSplitContent(model: model))
        view.sizingOptions = []
        view.safeAreaRegions = []
        view.setAccessibilityIdentifier("native-workspace-content")
        return view
    }
    func updateNSView(_ view: NSHostingView<WorkspaceSplitContent>, context: Context) {
        if view.rootView.model !== model { view.rootView = WorkspaceSplitContent(model: model) }
    }
    func sizeThatFits(_ proposal: ProposedViewSize, nsView: NSHostingView<WorkspaceSplitContent>, context: Context) -> CGSize? {
        proposal.replacingUnspecifiedDimensions()
    }
}

private struct WorkspaceSplitContent: View {
    @ObservedObject var model: AppModel
    var body: some View {
        HSplitView {
            Group {
                if model.screen == .hands { HandsView() } else { TiledWorkspaceView() }
            }.frame(minWidth: 340, maxWidth: .infinity, maxHeight: .infinity)
            if model.showingScreens, let service = model.remoteService {
                RemoteDashboard(service: service, host: model.remoteMacHost, phoneHost: model.remotePhoneHost,
                                onClose: { model.showingScreens = false })
                    .id(ObjectIdentifier(service))
                    .frame(minWidth: 320, idealWidth: 620, maxWidth: .infinity, maxHeight: .infinity)
                    .background(Color(nsColor: .textBackgroundColor))
            }
        }.environmentObject(model).preferredColorScheme(model.preferredColorScheme)
    }
}

private struct ZoomedWorkspaceContent: View, Equatable {
    @EnvironmentObject private var model: AppModel
    let size: CGSize
    static func == (lhs: Self, rhs: Self) -> Bool { lhs.size == rhs.size }
    var body: some View {
        NativeWorkspaceHost(model: model)
        .frame(width: size.width / model.workspaceZoom, height: size.height / model.workspaceZoom)
        .scaleEffect(model.workspaceZoom, anchor: .topLeading)
        .frame(width: size.width, height: size.height, alignment: .topLeading)
        .clipped()
    }
}

struct ContentView: View {
    @EnvironmentObject private var model: AppModel
    @State private var tabColumnVisibility: NavigationSplitViewVisibility = .detailOnly
    var body: some View {
        Group {
            if model.isStarting {
                ProgressView().accessibilityLabel("Opening conversations").controlSize(.small).frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if model.showsOnboarding {
                OnboardingView()
            } else {
                workspace
            }
        }
        .navigationTitle(model.screen == .hands ? "Hands" : "Nanocodex")
        .toolbar {
            if !model.isStarting && !model.showsOnboarding {
                WorkspaceToolbar(model: model, verticalTabs: model.tabPosition == "left" && tabColumnVisibility != .detailOnly) { vertical in
                    model.setTabPosition(vertical ? "left" : "top")
                    tabColumnVisibility = vertical ? .all : .detailOnly
                }
            }
        }
        .sheet(isPresented: $model.showingScheduledJobs) { MacScheduledJobsView() }
        .sheet(isPresented: $model.showingSettings) { SettingsView() }
    }
    private var workspace: some View {
        NavigationSplitView(columnVisibility: $tabColumnVisibility) {
            Group {
                if model.tabPosition == "left" { SidebarTabsView() }
            }
                .navigationSplitViewColumnWidth(min: 200, ideal: 248, max: 340)
                .toolbar(removing: .sidebarToggle)
                .accessibilityHidden(model.tabPosition != "left" || tabColumnVisibility == .detailOnly)
                .allowsHitTesting(model.tabPosition == "left" && tabColumnVisibility != .detailOnly)
        } detail: {
            VStack(spacing: 0) {
                if model.tabPosition != "left" || tabColumnVisibility == .detailOnly { TopTabsView() }
                if let error = model.error ?? model.state.error {
                    HStack(spacing: 10) {
                        Image(systemName: "exclamationmark.circle").foregroundStyle(.orange)
                        Text(error).font(.callout).textSelection(.enabled)
                        Spacer()
                        Button { model.error = nil; Task { await model.refresh() } } label: { Image(systemName: "arrow.clockwise") }.help("Retry connection")
                        Button { model.error = nil } label: { Image(systemName: "xmark") }.help("Dismiss")
                    }.buttonStyle(.plain).padding(12).background(Color.orange.opacity(0.07))
                }
                // Read the remaining detail column, not the full window. The
                // geometry frame consumes the native safe area exactly once.
                GeometryReader { geometry in
                    ZoomedWorkspaceContent(size: geometry.size).equatable()
                }
            }.frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .navigationSplitViewStyle(.balanced)
        .onChange(of: model.tabPosition, initial: true) { _, position in
            tabColumnVisibility = position == "left" ? .all : .detailOnly
        }
        .onChange(of: tabColumnVisibility) { _, visibility in
            if visibility == .detailOnly, model.workspaceFocus == .sidebar { model.enterNavigation() }
        }
        .toolbar(removing: .sidebarToggle)
        .background(Color(nsColor: .windowBackgroundColor))
        .sheet(isPresented: $model.showingSearch) { ThreadSearchView() }
        .sheet(isPresented: $model.showingTabOverview) { TabOverviewView() }
        .sheet(isPresented: $model.showingHandSetup) { HandSetupView(hand: model.editingHand) }
        .sheet(isPresented: $model.showingRemoteSetup) { RemoteSetupView() }
        .sheet(item: $model.selectedHandForLogs) { hand in HandLogView(id: hand.id) }
    }
}
