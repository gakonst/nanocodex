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
        #endif
    }
}
