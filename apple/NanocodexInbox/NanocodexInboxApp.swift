import SwiftUI
import InboxCore
import NanocodexVoice

@main
struct NanocodexInboxApp: App {
    init() { InboxModel.shared.configureAgentNotifications() }
    @State private var showQuickVoice = false
    @StateObject private var model = InboxModel.shared
    @Environment(\.scenePhase) private var scenePhase
    var body: some Scene {
        WindowGroup("Nanocodex", id: "inbox") {
            #if DEBUG && targetEnvironment(simulator)
            if ProcessInfo.processInfo.arguments.contains("--voice-clone-ui-fixture") {
                VoiceCloneUIFixture()
            } else {
                debugContent
            }
            #elseif DEBUG
            debugContent
            #else
            content
            #endif
        }
        .backgroundTask(.appRefresh(InboxModel.handRefreshIdentifier)) {
            await model.refreshHandInBackground()
        }
    }

    #if DEBUG
    @ViewBuilder private var debugContent: some View {
        if ProcessInfo.processInfo.arguments.contains("--soundcloud-loopback-smoke") {
            SpotifyLoopbackSmokeView(provider: .soundcloud)
        } else if ProcessInfo.processInfo.arguments.contains("--spotify-loopback-smoke") {
            SpotifyLoopbackSmokeView()
        } else {
            content.preferredColorScheme(demoColorScheme)
        }
    }

    private var demoColorScheme: ColorScheme? {
        guard ProcessInfo.processInfo.arguments.contains("--demo") else { return nil }
        return ["light": ColorScheme.light, "dark": ColorScheme.dark][ProcessInfo.processInfo.environment["NANOCODEX_DEMO_APPEARANCE"] ?? ""]
    }
    #endif

    private var content: some View {
        InboxView(model: model)
                .onAppear { Task { await model.start() } }
                .onReceive(NotificationCenter.default.publisher(for: UIApplication.didReceiveMemoryWarningNotification)) { _ in
                    model.releaseInactiveHistory()
                }
                .sheet(isPresented: $showQuickVoice) { QuickVoiceView(model: model) }
                .onOpenURL { url in
                    if url.scheme == "nanocodex", url.host == "voice", url.path == "/recovery", url.query == nil, url.fragment == nil {
                        Task { await model.openLockedVoiceRecovery() }
                        return
                    }
                    if QuickVoiceInput.matches(url) { showQuickVoice = true; return }
                    if url.scheme == "nanocodex", url.host == "connect", ["/spotify", "/soundcloud"].contains(url.path), url.query == nil, url.fragment == nil {
                        model.musicConnectorToOpen = MusicLoopbackProvider(rawValue: String(url.path.dropFirst()))
                    } else { model.openAgentActivity(url) }
                }
                .onChange(of: scenePhase, initial: true) { _, phase in
                    if phase == .background {
                        model.voice.stop()
                        model.setActive(false)
                    } else if phase == .active {
                        model.configureAgentNotifications()
                        model.setActive(true)
                    } else if phase == .inactive {
                        model.prepareHandForBackground()
                    }
                }
    }
}
