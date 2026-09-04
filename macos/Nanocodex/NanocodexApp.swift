import AppKit
import SwiftUI

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    var model: AppModel?
    private var terminating = false
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSWindow.allowsAutomaticWindowTabbing = false
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
    }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag { sender.windows.first(where: { $0.canBecomeMain })?.makeKeyAndOrderFront(nil) }
        return true
    }
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let model else { return .terminateNow }
        guard !terminating else { return .terminateLater }; terminating = true
        Task { await model.prepareToQuit(); sender.reply(toApplicationShouldTerminate: true) }
        return .terminateLater
    }
    func applicationWillTerminate(_ notification: Notification) { model?.shutdown() }
}

@main
struct NanocodexApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var model = AppModel()
    var body: some Scene {
        WindowGroup("Nanocodex", id: "main") {
            ContentView()
                .environmentObject(model)
                .preferredColorScheme(model.preferredColorScheme)
                .frame(minWidth: 820, minHeight: 600)
                .task {
                    if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] == nil { delegate.model = model; await model.start() }
                }
        }
        .defaultSize(width: 1200, height: 840)
        .windowToolbarStyle(.unifiedCompact)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("New Tab") { model.newTab() }.keyboardShortcut("t")
                Button("New Thread") { model.newTab() }.keyboardShortcut("n")
                Button("Close Tab") { model.closeTab(model.activeTabID) }.keyboardShortcut("w")
                Button("Reopen Closed Tab") { model.reopenTab() }.keyboardShortcut("t", modifiers: [.command, .shift])
            }
            CommandGroup(replacing: .appSettings) {
                Button("Settings…") { model.showingSettings = true }.keyboardShortcut(",")
            }
            CommandMenu("Tabs") {
                Button("Next Tab") { model.cycleTab(1) }.keyboardShortcut("]", modifiers: [.command, .shift])
                Button("Previous Tab") { model.cycleTab(-1) }.keyboardShortcut("[", modifiers: [.command, .shift])
                Divider()
                Picker("Tab Position", selection: $model.tabPosition) { Text("Sidebar").tag("left"); Text("Top").tag("top") }.onChange(of: model.tabPosition) { model.persistLayout() }
                Divider()
                Button("Search Threads…") { model.showingSearch = true }.keyboardShortcut("k")
            }
            CommandMenu("Agent") {
                Button("Stop Current Turn") { Task { await model.cancel() } }.keyboardShortcut(".").disabled(!model.isRunning)
                Button("Hands") { model.screen = .hands }.keyboardShortcut("h", modifiers: [.command, .shift])
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
        }
        .sheet(isPresented: $model.showingSettings) { SettingsView() }
    }
    private var workspace: some View {
        HStack(spacing: 0) {
            SidebarView().frame(width: model.tabPosition == "left" ? 250 : 205)
            VStack(spacing: 0) {
                if model.tabPosition == "top" { TopTabsView() }
                if let error = model.error ?? model.state.error {
                    HStack(spacing: 10) {
                        Image(systemName: "exclamationmark.circle").foregroundStyle(.orange)
                        Text(error).font(.callout).textSelection(.enabled)
                        Spacer()
                        Button { model.error = nil; Task { await model.refresh() } } label: { Image(systemName: "arrow.clockwise") }.help("Retry connection")
                        Button { model.error = nil } label: { Image(systemName: "xmark") }.help("Dismiss")
                    }.buttonStyle(.plain).padding(12).background(Color.orange.opacity(0.07))
                }
                if model.screen == .hands { HandsView() } else { ChatView() }
            }
            .background(Color(nsColor: .textBackgroundColor))
            .clipShape(.rect(topLeadingRadius: 14, bottomLeadingRadius: 0))
        }
        .background(Color(nsColor: .windowBackgroundColor))
        .toolbar {
            ToolbarItem(placement: .navigation) {
                Button { model.tabPosition = model.tabPosition == "left" ? "top" : "left"; model.persistLayout() } label: {
                    Image(systemName: model.tabPosition == "left" ? "sidebar.left" : "rectangle.topthird.inset.filled")
                }.help("Move tabs to \(model.tabPosition == "left" ? "the top" : "the sidebar")").accessibilityIdentifier("toggle-tab-position")
            }
            ToolbarItem(placement: .primaryAction) {
                Button { model.newTab() } label: { Image(systemName: "plus") }.help("New tab (⌘T)").accessibilityIdentifier("new-tab")
            }
        }
        .sheet(isPresented: $model.showingSearch) { ThreadSearchView() }
        .sheet(isPresented: $model.showingHandSetup) { HandSetupView(hand: model.editingHand) }
        .sheet(isPresented: $model.showingRemoteSetup) { RemoteSetupView() }
        .sheet(item: $model.selectedHandForLogs) { hand in HandLogView(id: hand.id) }
    }
}
