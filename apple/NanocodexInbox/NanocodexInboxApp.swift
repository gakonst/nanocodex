import SwiftUI

@main
struct NanocodexInboxApp: App {
    @StateObject private var model = InboxModel()
    @Environment(\.scenePhase) private var scenePhase
    var body: some Scene {
        WindowGroup {
            InboxView(model: model)
                .task { await model.start() }
                .onChange(of: scenePhase) { _, phase in model.setActive(phase == .active) }
                #if os(macOS)
                .frame(minWidth: 420, minHeight: 680)
                #endif
        }
        #if os(macOS)
        .defaultSize(width: 560, height: 850)
        .windowStyle(.hiddenTitleBar)
        .commands {
            CommandMenu("Inbox") {
                Button("Next agent") { model.advance(reviewed: false) }.keyboardShortcut(.rightArrow, modifiers: .command)
                Button("Mark update seen") { model.advance(reviewed: true) }.keyboardShortcut("d", modifiers: .command)
                Button("Previous agent") { model.back() }.keyboardShortcut("[", modifiers: .command).disabled(!model.canGoBack)
            }
        }
        #endif
    }
}
