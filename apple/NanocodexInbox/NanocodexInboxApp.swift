import SwiftUI
import InboxCore
import UIKit

@main
struct NanocodexInboxApp: App {
    @UIApplicationDelegateAdaptor(NanocodexAppDelegate.self) private var appDelegate
    init() { InboxModel.shared.configureAgentNotifications() }
    @StateObject private var model = InboxModel.shared
    @Environment(\.scenePhase) private var scenePhase
    var body: some Scene {
        WindowGroup("Nanocodex", id: "inbox") {
            #if DEBUG
            if ProcessInfo.processInfo.arguments.contains("--soundcloud-loopback-smoke") {
                SpotifyLoopbackSmokeView(provider: .soundcloud)
            } else if ProcessInfo.processInfo.arguments.contains("--spotify-loopback-smoke") {
                SpotifyLoopbackSmokeView()
            } else {
                content
            }
            #else
            content
            #endif
        }
        .backgroundTask(.appRefresh(InboxModel.handRefreshIdentifier)) {
            await model.refreshHandInBackground()
        }
    }

    private var content: some View {
        InboxView(model: model)
                .onAppear { Task { await model.start() } }
                .onReceive(NotificationCenter.default.publisher(for: UIApplication.didReceiveMemoryWarningNotification)) { _ in
                    model.releaseInactiveHistory()
                }
                .onOpenURL { model.handleURL($0) }
                .onChange(of: scenePhase, initial: true) { _, phase in
                    if phase == .background {
                        if !model.carPlayConnected {
                            model.voice.stop()
                            model.setActive(false)
                        }
                    } else if phase == .active {
                        model.setActive(true)
                    } else if phase == .inactive && !model.carPlayConnected {
                        model.prepareHandForBackground()
                    }
                }
    }
}
