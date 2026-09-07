import SwiftUI

@main
struct NanocodexInboxApp: App {
    @StateObject private var model = InboxModel.shared
    @Environment(\.scenePhase) private var scenePhase
    var body: some Scene {
        WindowGroup("Nanocodex", id: "inbox") {
            InboxView(model: model)
                .onAppear { Task { await model.start() } }
                .onChange(of: scenePhase, initial: true) { _, phase in
                    if phase == .background {
                        model.voice.stop()
                        model.setActive(false)
                    } else if phase == .active {
                        model.setActive(true)
                    } else if phase == .inactive {
                        model.prepareHandForBackground()
                    }
                }
        }
        .backgroundTask(.appRefresh(InboxModel.handRefreshIdentifier)) {
            await model.refreshHandInBackground()
        }
    }
}
