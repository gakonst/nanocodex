import SwiftUI
import QuickLook
import PhotosUI
import UniformTypeIdentifiers
import ImageIO
import AVKit
import InboxCore
import NanocodexRemote
import NanocodexVoice
import NanocodexContext
import NanocodexUI
import UIKit
import AVFoundation
import os.signpost

private struct ConversationNavigationActiveKey: EnvironmentKey {
    static let defaultValue = false
}

private extension EnvironmentValues {
    var conversationNavigationActive: Bool {
        get { self[ConversationNavigationActiveKey.self] }
        set { self[ConversationNavigationActiveKey.self] = newValue }
    }
}

private enum Ink {
    static let background = Color(uiColor: .systemBackground)
    static let card = Color(uiColor: .secondarySystemGroupedBackground)
    static let surface = ChatPalette.userBubble
    static let border = Color(uiColor: .separator)
    static let text = Color.primary
    static let muted = Color.secondary
    static let accent = Color.primary
    static let amber = Color.secondary
    static let assistant = Color(uiColor: UIColor { traits in
        traits.userInterfaceStyle == .dark ? .secondarySystemGroupedBackground
            : UIColor(red: 0.91, green: 0.91, blue: 0.92, alpha: 1)
    })
    static let running = Color(uiColor: UIColor { traits in
        traits.userInterfaceStyle == .dark ? UIColor(red: 0.35, green: 0.85, blue: 0.5, alpha: 1)
            : UIColor(red: 0.17, green: 0.45, blue: 0.24, alpha: 1)
    })
    static let userMessage = Color(uiColor: UIColor { traits in
        traits.userInterfaceStyle == .dark ? UIColor(red: 0.08, green: 0.25, blue: 0.47, alpha: 1)
            : UIColor(red: 0.84, green: 0.92, blue: 1, alpha: 1)
    })
}

struct InboxView: View {
    @ObservedObject var model: InboxModel
    @State private var showConversations = false
    @State private var drawerTranslation: CGFloat = 0
    @State private var readingPositions = ConversationReadingPositions()
    @State private var showScheduledJobs = false
    @State private var showConnectors = false
    @State private var showSettings = false
    @StateObject private var appUpdates = NativeAppUpdateModel()
    @Environment(\.scenePhase) private var updateScenePhase
    @State private var showScreens = false
    @State private var screenThreads: Set<String> = []
    @State private var screenExpanded = false
    @State private var controlsScreen: RemoteScreenSelection?
    @State private var screenViewerRevision = UUID()
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var composerFocused = false

    var body: some View {
        NavigationStack {
            inbox
                #if os(iOS)
                .navigationTitle("Conversations")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar(.hidden, for: .navigationBar)
                #endif
                .navigationDestination(isPresented: $showScheduledJobs) {
                    ScheduledJobsView(model: model) {
                        showScheduledJobs = false
                        composerFocused = false
                    }
                    #if os(iOS)
                    .toolbar(.visible, for: .navigationBar)
                    .navigationBarTitleDisplayMode(.inline)
                    #endif
                }
                .navigationDestination(isPresented: $showConnectors) {
                    ConnectorsView(model: model)
                        #if os(iOS)
                        .toolbar(.visible, for: .navigationBar)
                        .navigationBarTitleDisplayMode(.inline)
                        #endif
                }
        }
        .safeAreaInset(edge: .top, spacing: 0) {
            if !model.isDemo, let update = appUpdates.update {
                HStack(spacing: 12) {
                    Image(systemName: "arrow.down.app.fill").font(.title2)
                    VStack(alignment: .leading, spacing: 3) {
                        Text("Nanocodex update available").font(.headline)
                        Text(appUpdates.installRequested
                             ? "Confirm Install, then return to the Home Screen."
                             : "Build \(update.build) is ready to install.")
                            .font(.caption).foregroundStyle(.secondary)
                        if let error = appUpdates.error { Text(error).font(.caption).foregroundStyle(.red) }
                    }
                    Spacer(minLength: 0)
                    Button(appUpdates.installing ? "Opening…" : "Install") {
                        Task { await appUpdates.install(update) }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(appUpdates.installing)
                    .accessibilityIdentifier("install-update-banner")
                }
                .padding().background(.regularMaterial)
                .accessibilityIdentifier("app-update-banner")
            }
        }
        .task(id: updateScenePhase) {
            guard updateScenePhase == .active, !model.isDemo else { return }
            while !Task.isCancelled {
                await appUpdates.check()
                do { try await Task.sleep(for: .seconds(60)) }
                catch { return }
            }
        }
        .foregroundStyle(Ink.text)
        .tint(Ink.accent)
        .sheet(isPresented: $showSettings) {
            NavigationStack {
                settings
                    .toolbar {
                        ToolbarItem(placement: .confirmationAction) {
                            Button("Done") { showSettings = false }
                        }
                    }
            }
            .tint(Ink.accent)
            .presentationDragIndicator(.visible)
            .presentationCornerRadius(30)
        }
        .fullScreenCover(isPresented: $showScreens) {
            if let service = model.remoteService {
                NavigationStack {
                    RemoteDashboard(service: service, initialSelection: controlsScreen, onClose: { showScreens = false })
                }
            }
        }
        .sheet(isPresented: $model.showContext) { ContextInboxView(model: model).tint(Ink.accent) }
        .onChange(of: model.screenScope) { _, _ in
            screenThreads.removeAll(); screenExpanded = false; showScreens = false; controlsScreen = nil
        }
        .onChange(of: model.focused?.id) { _, _ in screenExpanded = false }
        .onChange(of: showScreens) { _, visible in
            // Recreate the passive panel after full controls release their lease.
            if !visible { screenViewerRevision = UUID() }
        }
        .onChange(of: model.focused?.activeTurns ?? []) { _, turns in
            if !turns.contains(model.selectedTurn) { model.selectedTurn = turns.first ?? "" }
        }
        .onChange(of: model.focusedConversationIdentity, initial: true) { _, _ in
            composerFocused = false
            if model.focused != nil { model.openThread() }
        }
        .onChange(of: model.musicConnectorToOpen) { _, provider in
            if provider != nil && model.connected { showSettings = false; showConnectors = true }
        }
        .onChange(of: model.connected) { _, connected in
            if connected && model.musicConnectorToOpen != nil { showConnectors = true }
            if !connected { screenThreads.removeAll(); screenExpanded = false; showConversations = false; showScreens = false; showScheduledJobs = false; showConnectors = false; showSettings = false; readingPositions.values.removeAll() }
        }

    }

    private var inbox: some View {
        ZStack {
            Ink.background.ignoresSafeArea()
            if model.restoringAccount {
                accountRestoration
            } else if model.connected {
                conversationWorkspace
            } else { ConnectView(model: model) }
        }
    }
    private var conversationWorkspace: some View {
        GeometryReader { geometry in
            let width = min(geometry.size.width - 24, 420)
            let reveal = showConversations ? width + drawerTranslation : drawerTranslation
            ZStack(alignment: .leading) {
                if showConversations || drawerTranslation > 0 {
                    ConversationDrawer(model: model, select: { id in
                        selectConversation(id)
                        setConversationsVisible(false)
                    }, close: { setConversationsVisible(false) }, create: createAgent,
                    settings: { showSettings = true })
                    .frame(width: width, height: geometry.size.height)
                    // Slide the conversation above a stationary list. Moving
                    // a newly inserted native scroll view can strand its rows
                    // offscreen when the same drag dismisses the keyboard.
                    .allowsHitTesting(showConversations)
                    .accessibilityHidden(!showConversations)
                    .transition(.opacity)
                }
                // Keep the transcript and editor mounted. Opening navigation must
                // not rebuild history, lose a draft, or start preview streams.
                inboxContent
                    .environment(\.conversationNavigationActive, showConversations || drawerTranslation != 0)
                    // Animate the outer drawer translation only. Inherited spring
                    // transactions must not animate transcript layout or restoration.
                    .transaction { $0.animation = nil }
                    .frame(width: geometry.size.width, height: geometry.size.height)
                    .background(Ink.background)
                    .clipShape(RoundedRectangle(cornerRadius: reveal > 0 ? 28 : 0))
                    .shadow(color: .black.opacity(reveal > 0 ? 0.12 : 0), radius: 16, x: -4)
                    .overlay {
                        if showConversations {
                            Color.clear.contentShape(Rectangle())
                                .onTapGesture { setConversationsVisible(false) }
                        }
                    }
                    .accessibilityHidden(showConversations)
                    .offset(x: reveal)
            }
            .clipped()
            .contentShape(Rectangle())
            .simultaneousGesture(DragGesture(minimumDistance: 16)
                .onChanged { value in
                    // Reserve navigation for a rightward pull from the left
                    // edge. Code blocks and transcript swipes keep their input.
                    guard showConversations || value.startLocation.x <= 28,
                          abs(value.translation.width) > abs(value.translation.height) * 1.5 else { return }
                    if showConversations {
                        drawerTranslation = max(-width, min(0, value.translation.width))
                    } else if value.translation.width > 0 {
                        composerFocused = false
                        drawerTranslation = min(width, value.translation.width)
                    }
                }
                .onEnded { value in
                    guard showConversations || value.startLocation.x <= 28 else { return }
                    let horizontal = abs(value.translation.width) > abs(value.translation.height) * 1.5
                    let visible: Bool
                    if showConversations {
                        visible = !(horizontal && (value.translation.width < -width * 0.25
                            || value.predictedEndTranslation.width < -width * 0.5))
                    } else {
                        visible = horizontal && (value.translation.width > width * 0.25
                            || value.predictedEndTranslation.width > width * 0.5)
                    }
                    setConversationsVisible(visible)
                })
        }
    }
    private func setConversationsVisible(_ visible: Bool) {
        composerFocused = false
        withAnimation(reduceMotion ? nil : .spring(response: 0.32, dampingFraction: 0.92)) {
            drawerTranslation = 0
            showConversations = visible
        }
    }
    private var inboxContent: some View {
        VStack(spacing: 0) {
            // Scrolled content can retain offscreen hit regions at large text
            // sizes. Keep navigation above those regions as well as visually.
            conversationHeader.zIndex(1)
                if let screen = model.latestScreenOutput,
                   !screenThreads.contains(model.focusedConversationIdentity ?? "") {
                    ChatLatestScreen(output: screen, onWatchLive: model.remoteService == nil ? nil : {
                        guard let identity = model.focusedConversationIdentity else { return }
                        screenThreads.insert(identity)
                    })
                        .id(model.focusedConversationIdentity)
                        .frame(maxWidth: 620)
                        .padding(.horizontal, 12).padding(.bottom, 6)
                }
            if let card = model.focused, let identity = model.focusedConversationIdentity,
               screenThreads.contains(identity), let service = model.remoteService {
                RemoteThreadScreen(service: service,
                    selection: Binding(get: { model.screenSelection(agentID: card.id) },
                                       set: { model.selectScreen($0, agentID: card.id) }),
                    expanded: $screenExpanded,
                    onClose: { screenThreads.remove(identity); screenExpanded = false },
                    onControls: { controlsScreen = $0; composerFocused = false; showScreens = true })
                    .id(model.screenScope + identity + screenViewerRevision.uuidString)
                    .frame(height: screenExpanded ? nil : 220)
                    .frame(maxHeight: screenExpanded ? .infinity : nil)
                    .padding(.horizontal, 12).padding(.bottom, 8)
                    .zIndex(1)
            }
            Group {
                    if let identity = model.focusedConversationIdentity {
                        ConversationView(model: model, identity: identity, readingPositions: readingPositions).id(identity)
                    } else { emptyState.frame(maxWidth: .infinity, maxHeight: .infinity) }
            }
            .frame(maxHeight: screenExpanded && screenThreads.contains(model.focusedConversationIdentity ?? "") ? 0 : .infinity)
            .clipped()
            .accessibilityHidden(screenExpanded && screenThreads.contains(model.focusedConversationIdentity ?? ""))
            .allowsHitTesting(!(screenExpanded && screenThreads.contains(model.focusedConversationIdentity ?? "")))
            .overlay(alignment: .topTrailing) {
                // A delayed reconnect must not push the transcript down while
                // the reader is moving through history.
                ConnectionStatusView(status: model.threadLoading ? "" : model.connection, retry: { model.retryConnection() }, signIn: { showSettings = true })
                    .padding(.horizontal, 16)
            }
            if let error = model.error {
                HStack(alignment: .top) {
                    Text(error).font(.caption).foregroundStyle(Ink.amber)
                    Spacer(minLength: 4)
                    Button { model.error = nil } label: { Image(systemName: "xmark") }
                        .accessibilityLabel("Dismiss error")
                }
                .padding(12).background(Ink.card, in: RoundedRectangle(cornerRadius: 12)).padding(.horizontal, 12)
            } else if let notice = model.notice, !composerFocused {
                Text(notice).font(.caption).foregroundStyle(Ink.muted).accessibilityIdentifier("notice")
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            VStack(spacing: 0) {
                if model.focused != nil {
                    AgentComposerView(model: model, focused: $composerFocused, onVoiceChat: {
                        composerFocused = false
                    }).frame(maxWidth: 620)
                }
            }
            .padding(.bottom, 4)
            .background {
                LinearGradient(colors: [Ink.background.opacity(0), Ink.background, Ink.background], startPoint: .top, endPoint: .bottom)
                    .ignoresSafeArea(edges: .bottom)
            }
        }
    }

    private var conversationHeader: some View {
        let card = model.focused
        return HStack(spacing: 12) {
            Button { setConversationsVisible(true) } label: {
                Image(systemName: "line.3.horizontal").frame(width: 44, height: 44).contentShape(Rectangle())
            }
            .modifier(InboxHeaderGlass())
            .accessibilityLabel("Conversations").accessibilityIdentifier("conversation-drawer-open")
            Button { setConversationsVisible(true) } label: {
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 6) {
                        if card?.isRunning == true {
                            Circle().fill(Ink.running).frame(width: 6, height: 6).accessibilityHidden(true)
                        }
                        Text(card?.title ?? "New conversation")
                            .font(.subheadline.weight(.semibold)).lineLimit(1)
                    }
                }
                .frame(minWidth: 0, maxWidth: .infinity, minHeight: 44, alignment: .leading)
                .contentShape(Rectangle())
            }
            .accessibilityLabel(card?.title ?? "New conversation")
            .accessibilityValue(card?.status ?? "")
            .accessibilityAddTraits(.isSelected)
            .accessibilityIdentifier("conversation-title:" + (card?.id ?? "empty"))
            HStack(spacing: 0) {
                Button(action: createAgent) {
                    Image(systemName: "square.and.pencil").frame(width: 44, height: 44)
                }.accessibilityLabel("New conversation").accessibilityIdentifier("new-conversation")
                    .keyboardShortcut("n", modifiers: .command)
                appMenu
            }.modifier(InboxHeaderGlass())
        }
        .buttonStyle(.plain)
        .font(.system(size: 18, weight: .medium))
        .padding(.horizontal, 16).padding(.vertical, 6)
    }

    private var appMenu: some View {
        Menu {
            Button { composerFocused = false; model.back() } label: {
                Label("Back", systemImage: "chevron.left")
            }.disabled(!model.canGoBack).accessibilityIdentifier("conversation-back")
            Button {
                guard let id = model.focusedConversationIdentity else { return }
                composerFocused = false; screenExpanded = false
                if screenThreads.contains(id) { screenThreads.remove(id) } else { screenThreads.insert(id) }
            } label: {
                Label(screenThreads.contains(model.focusedConversationIdentity ?? "") ? "Hide screen" : "Screen", systemImage: "display")
            }.disabled(model.remoteService == nil || model.focused == nil).accessibilityIdentifier("conversation-remote-screens")
            Button { composerFocused = false; model.showContext = true } label: {
                Label("Context from other apps", systemImage: "tray")
            }.accessibilityIdentifier("conversation-context")
            Divider()
            Button { composerFocused = false; showScheduledJobs = true } label: {
                Label("Scheduled jobs", systemImage: "clock")
            }.accessibilityIdentifier("inbox-scheduled-jobs")
            if !model.isDemo {
                Button { composerFocused = false; showConnectors = true } label: {
                    Label("Connectors", systemImage: "link")
                }.accessibilityIdentifier("inbox-connectors")
            }
            Button { composerFocused = false; showSettings = true } label: {
                Label("Account settings", systemImage: "gearshape")
            }
        } label: {
            Image(systemName: "ellipsis").frame(width: 44, height: 44).contentShape(Rectangle())
        }.accessibilityLabel("App menu").accessibilityIdentifier("app-menu")
    }

    private func selectConversation(_ id: String) {
        composerFocused = false
        model.select(id)
    }
    private var accountRestoration: some View {
        VStack(spacing: 20) {
            if let error = model.restorationError {
                Image(systemName: "tray").font(.system(size: 38)).foregroundStyle(Ink.muted)
                Text("Couldn’t open your conversations").font(.title3.weight(.semibold))
                Text(error).font(.subheadline).foregroundStyle(Ink.muted).multilineTextAlignment(.center)
                Button("Retry") { Task { await model.restoreSavedAccount() } }
                    .buttonStyle(.borderedProminent).disabled(model.signingIn)
                    .accessibilityIdentifier("retry-account-restoration")
            } else {
                ProgressView()
                    .accessibilityLabel("Opening conversations")
            }
        }
        .padding(24).frame(maxWidth: 620, maxHeight: .infinity)
        .accessibilityIdentifier("account-restoration")
    }
    private var emptyState: some View {
        VStack(spacing: 18) {
            Image(systemName: "bubble.left.and.bubble.right").font(.system(size: 46, weight: .ultraLight)).foregroundStyle(Ink.accent)
            Text("No conversations").font(.title2.weight(.medium))
            Text("Create a conversation and start with a message.").font(.subheadline).foregroundStyle(Ink.muted).multilineTextAlignment(.center)
            Button("New conversation") { createAgent() }.buttonStyle(.borderedProminent).foregroundStyle(Ink.background)
            Button("Context from other apps") { model.showContext = true }
        }.padding(24).accessibilityElement(children: .contain).accessibilityIdentifier("inbox-empty")
    }
    private var settings: some View {
        Form {
            Section("Account") {
                Text(model.isDemo ? "Demo · sample agents" : model.connection == "Sign in again" ? "Sign in again to reconnect your account." : "Nanocodex account connected")
                Text("Agents keep running when you switch conversations or close the app.").foregroundStyle(.secondary)
                Button(model.isDemo ? "Connect account" : model.connection == "Sign in again" ? "Sign in again" : "Disconnect account") {
                    do { try model.disconnect(); showSettings = false } catch { model.error = error.localizedDescription }
                }
            }
            if !model.isDemo {
                Section {
                    NavigationLink {
                        ConnectorsView(model: model)
                    } label: {
                        Label("Connectors", systemImage: "link")
                    }
                    .accessibilityIdentifier("settings-connectors")
                }
                Section("This device") {
                    Toggle("Make this device available as a Hand", isOn: $model.deviceHandEnabled)
                        .accessibilityIdentifier("device-hand-enabled")
                    Label(model.deviceHandStatus, systemImage: "hand.raised")
                        .accessibilityIdentifier("device-hand-status")
                    Text("Connects automatically to your account unless disabled. Agents can work with workspace files and query captured messages when capture is enabled.").font(.caption).foregroundStyle(.secondary)
                    Text("Tasks you start can keep this Hand connected in the background on iOS 26 or later. iOS shows progress and lets you stop the task. When idle, this phone connects only during brief background windows or while Nanocodex is open. Force-quitting ends background work.").font(.caption).foregroundStyle(.secondary)
                    if let error = model.handBackgroundError { Text(error).font(.caption).foregroundStyle(.secondary) }
                }
                NativeAppUpdateSection(updater: appUpdates)
            }
            Section {
                NavigationLink { DevicePermissionsView() } label: {
                    Label("Device access", systemImage: "hand.raised")
                }.accessibilityIdentifier("settings-device-access")
            }
            Section("Controls") {
                Text("Open Conversations at the top to switch agents. The compose button creates a conversation. Back, Screens, and captured context are in the more menu.")
                Text("The sidebar lists your conversations. Green identifies running agents. Drafts and reading positions stay with each conversation.").font(.caption)
                Text("Scroll up to read earlier messages. Send updates the active turn at its next safe opportunity. ⌘Return sends your message.").font(.caption)
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Settings")
        .accessibilityIdentifier("inbox-settings")
    }
    private func createAgent() {
        composerFocused = false
        #if os(iOS)
        UIImpactFeedbackGenerator(style: .soft).impactOccurred()
        #endif
        setConversationsVisible(false)
        model.newAgent()
    }

}

private struct InboxHeaderGlass: ViewModifier {
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    func body(content: Content) -> some View {
        if reduceTransparency {
            content.background(Ink.card, in: RoundedRectangle(cornerRadius: 24))
        } else if #available(iOS 26.0, *) {
            content.glassEffect(.regular, in: RoundedRectangle(cornerRadius: 24))
        } else {
            content.background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 24))
        }
    }
}

/// Navigation uses roster summaries only, without parsing Markdown or starting preview streams.
private struct SidebarRecency: Equatable {
    let id: String
    let sentAt: Double
}

private struct ConversationDrawer: View {
    @ObservedObject var model: InboxModel
    let select: (String) -> Void
    let close: () -> Void
    let create: () -> Void
    let settings: () -> Void
    @ScaledMetric(relativeTo: .subheadline) private var titleSize = 15
    @ScaledMetric(relativeTo: .footnote) private var detailSize = 13
    @ScaledMetric(relativeTo: .caption) private var statusSize = 12
    @State private var query = ""
    @State private var order: [String]

    init(model: InboxModel, select: @escaping (String) -> Void, close: @escaping () -> Void,
         create: @escaping () -> Void, settings: @escaping () -> Void) {
        self.model = model; self.select = select; self.close = close
        self.create = create; self.settings = settings
        _order = State(initialValue: model.cards.sorted(by: AgentCard.mostRecentlyMessagedFirst).map(\.id))
    }

    private var visibleCards: [AgentCard] {
        let search = query.trimmingCharacters(in: .whitespacesAndNewlines)
        // Include previously hidden tabs: navigation no longer has a hidden state.
        let byID = Dictionary(uniqueKeysWithValues: model.cards.map { ($0.id, $0) })
        let cards = order.compactMap { byID[$0] }
        return cards.filter { card in
            search.isEmpty
                || card.title.localizedCaseInsensitiveContains(search)
                || card.id.localizedCaseInsensitiveContains(search)
                || card.preview.localizedCaseInsensitiveContains(search)
        }
    }

    private func conversationRow(_ card: AgentCard) -> some View {
        let knownStatus = card.sidebarStatus
        let running = ["Running", "Stopping"].contains(knownStatus)
        let subtitle = card.error != nil ? "Couldn’t refresh" : card.sidebarActivity
        let status = [knownStatus, subtitle, card.error ?? ""].filter { !$0.isEmpty }.joined(separator: ". ")
        return VStack(alignment: .leading, spacing: 6) {
            Text(card.title)
                .font(.system(size: titleSize, weight: model.focused?.id == card.id ? .medium : .regular))
                .foregroundStyle(Ink.text).lineLimit(2)
            HStack(spacing: 6) {
                Circle().fill(running ? Ink.running : Ink.muted.opacity(0.65))
                    .frame(width: 5, height: 5).accessibilityHidden(true)
                Text(knownStatus).font(.system(size: statusSize)).foregroundStyle(Ink.muted)
            }
            if !subtitle.isEmpty {
                Text(subtitle).font(.system(size: detailSize)).foregroundStyle(Ink.muted)
                    .lineLimit(2).fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.horizontal, 12).padding(.vertical, 11)
        .frame(maxWidth: .infinity, minHeight: 56, alignment: .leading)
        .background(model.focused?.id == card.id ? Ink.surface : Color.clear,
                    in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .contentShape(Rectangle())
        // Tap recognition must fail when dragging. A plain Button can fire
        // on release after the drawer's simultaneous swipe gesture.
        .onTapGesture { select(card.id) }
        .accessibilityRepresentation {
            Button(card.title) { select(card.id) }
                .accessibilityValue(status)
                .accessibilityAddTraits(model.focused?.id == card.id ? [.isSelected] : [])
                .accessibilityIdentifier("conversation-row:" + card.id)
        }
    }

    var body: some View {
        VStack(spacing: 12) {
            HStack(spacing: 4) {
                Text("Agents").font(.headline.weight(.medium)).foregroundStyle(Ink.text)
                    .padding(.leading, 12)
                Spacer()
                Button(action: create) {
                    Image(systemName: "square.and.pencil").frame(width: 44, height: 44)
                }
                .accessibilityLabel("New conversation").accessibilityIdentifier("drawer-new-conversation")
                Button(action: close) {
                    Image(systemName: "sidebar.left").frame(width: 44, height: 44)
                }
                .accessibilityLabel("Return to conversation").accessibilityIdentifier("conversation-drawer-close")
            }
            .font(.system(size: 17, weight: .regular))
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").foregroundStyle(Ink.muted)
                TextField("Search agents", text: $query)
                    .textInputAutocapitalization(.never).autocorrectionDisabled()
                    .accessibilityIdentifier("conversation-search")
                if !query.isEmpty {
                    Button { query = "" } label: {
                        Image(systemName: "xmark.circle.fill").frame(width: 44, height: 44)
                    }.foregroundStyle(Ink.muted).accessibilityLabel("Clear search")
                }
            }
            .font(.system(size: detailSize)).padding(.horizontal, 12).frame(minHeight: 44)
            .background(Ink.surface, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            ScrollView {
                LazyVStack(spacing: 4) {
                    ForEach(visibleCards) { card in
                        conversationRow(card)
                    }
                    if visibleCards.isEmpty {
                        ContentUnavailableView("No matching conversations", systemImage: "bubble.left.and.bubble.right",
                                               description: Text("Try another search."))
                    }
                }
            }
            .scrollDismissesKeyboard(.interactively)
            .accessibilityIdentifier("conversation-list")
            Divider().overlay(Ink.border.opacity(0.3))
            Button(action: settings) {
                HStack(spacing: 10) {
                    Image(systemName: "gearshape")
                    Text("Settings").font(.system(size: detailSize))
                    Spacer()
                }
                .foregroundStyle(Ink.muted).padding(.horizontal, 12).frame(minHeight: 44)
                .contentShape(Rectangle())
            }.accessibilityLabel("Account settings")
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 16).padding(.top, 6).padding(.bottom, 8)
        .background(ChatPalette.sidebar)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("conversation-drawer")
        .accessibilityAction(.escape, close)
        .onChange(of: model.cards.map { SidebarRecency(id: $0.id, sentAt: $0.lastUserMessageAt) }) { _, _ in
            // Only user messages can move an existing conversation.
            order = model.cards.sorted(by: AgentCard.mostRecentlyMessagedFirst).map(\.id)
        }
    }
}

private struct ConnectionStatusView: View {
    let status: String
    let retry: () -> Void
    let signIn: () -> Void
    @State private var showDelay = false

    private var isWaiting: Bool { status == "Connecting" || status == "Reconnecting" }

    var body: some View {
        HStack(spacing: 0) {
            if status == "Sign in again" {
                Button("Sign in again", action: signIn).accessibilityIdentifier("connection-sign-in")
            } else if isWaiting, showDelay {
                Button(action: retry) {
                    ProgressView()
                }
                .frame(minHeight: 44)
                .accessibilityLabel("Updates delayed. Retry connection")
                .accessibilityIdentifier("connection-retry")
            }
        }
        .font(.system(size: 12)).foregroundStyle(Ink.muted).lineLimit(1)
        .task(id: isWaiting) {
            showDelay = false
            guard isWaiting else { return }
            // Opening another agent normally involves a brief stream handshake.
            // Only a sustained delay needs attention in the inbox header.
            do { try await Task.sleep(for: .seconds(5)) } catch { return }
            showDelay = true
        }
    }
}

private struct AgentComposerView: View {
    @ObservedObject var model: InboxModel
    @Binding var focused: Bool
    var onVoiceChat: @MainActor () -> Void = {}
    @State private var showExpandedEditor = false
    @State private var composerOverflows = false
    @State private var showPhotos = false
    @State private var showFiles = false
    @State private var showAttachmentMenu = false
    @State private var attachmentAction: AttachmentAction?
    @State private var selectedPhotos: [PhotosPickerItem] = []
    @State private var photoTarget: InboxModel.AttachmentTarget?
    @State private var fileTarget: InboxModel.AttachmentTarget?
    @State private var pickerError: String?
    @State private var queueContentHeight: CGFloat = 64
    private enum AttachmentAction { case camera, photos, files, context }
    #if os(iOS)
    @State private var showCamera = false
    @State private var cameraTarget: InboxModel.AttachmentTarget?
    @State private var cameraPermissionDenied = false
    #endif

    var body: some View {
        let queue = model.focusedQueue
        let visiblePending = queue.queuedMessages
        // Reuse derived state across labels, enabled state, and accessibility.
        // In particular, trimming the draft and looking up the active turn must
        // not repeat for every modifier on the send button.
        let card = model.focused
        let attachments = model.focusedAttachments
        let preparingAttachments = model.preparingAttachments
        let controllableTurns = model.controllableTurns
        let contextCount = card.map { model.contextForAgent($0.id).count } ?? 0
        let stopTarget = model.stopTarget
        let sendShowsStop = model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && attachments.isEmpty && !preparingAttachments && !stopTarget.isEmpty
        let stopRequest = sendShowsStop ? card.flatMap { model.cancellation(agentID: $0.id, turnID: stopTarget) } : nil
        let canSend = model.canSend
        VStack(spacing: 0) {
            if let error = model.creationError {
                HStack {
                    Text(error).font(.caption).foregroundStyle(Ink.muted)
                    Spacer()
                    Button("Retry") { model.retryCreation() }.accessibilityIdentifier("retry-creation")
                }.padding(12)
            }
            if contextCount > 0 {
                Button { focused = false; model.showContext = true } label: {
                    Label("Context for your next message (\(contextCount))", systemImage: "tray.full")
                        .font(.caption).padding(.vertical, 10)
                }.accessibilityIdentifier("composer-context")
            }
            if controllableTurns.count > 1 {
                Picker("Active turn", selection: $model.selectedTurn) {
                    ForEach(Array(controllableTurns.enumerated()), id: \.element) { index, id in Text("Turn \(index + 1)").tag(id) }
                }.pickerStyle(.menu)
            }
            if !visiblePending.isEmpty {
                ScrollView {
                    VStack(spacing: 0) {
                        ForEach(visiblePending) { message in
                            HStack(alignment: .center, spacing: 8) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(model.steeringTransfer(message.id)?.title ?? message.queueTitle)
                                        .font(.system(size: 11)).foregroundStyle(Ink.muted)
                                    Text(ContextPrompt.separate(message.input)?.request ?? message.input).font(.system(size: 14))
                                        .fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
                                        .accessibilityIdentifier("pending-message")
                                    if let names = queue.attachmentNames[message.id], !names.isEmpty {
                                        Label(names.joined(separator: ", "), systemImage: "photo")
                                            .font(.caption2).lineLimit(1).accessibilityIdentifier("pending-attachments")
                                    }
                                    if let attachments = message.attachments, !attachments.isEmpty {
                                        ScrollView(.horizontal) {
                                            HStack {
                                                ForEach(attachments) { attachment in
                                                    if attachment.isVideo {
                                                        AttachmentMovieThumbnail(attachment: attachment, poster: model.attachmentURL(attachment), movie: model.attachmentMovieURL(attachment))
                                                            .frame(width: 64, height: 64)
                                                    } else if model.attachmentURL(attachment) != nil {
                                                        AttachmentPhotoThumbnail(attachment: attachment, model: model)
                                                            .frame(width: 64, height: 64).accessibilityIdentifier("message-image")
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    if let error = model.steeringTransfer(message.id)?.error ?? message.error { Text(error).font(.caption2).foregroundStyle(Ink.muted) }
                                }.frame(maxWidth: .infinity, alignment: .leading)
                                if message.phase == .failed, message.remoteAdmission != true {
                                    Button { model.retryPending(message.id) } label: { Text("Retry").frame(minHeight: 44) }.accessibilityIdentifier("retry-pending")
                                        .disabled(model.busy.contains(message.agentID))
                                } else if let transfer = model.steeringTransfer(message.id), transfer.error != nil && transfer.canResume {
                                    Button("Retry steer") { model.steerNow(message.id) }.accessibilityIdentifier("retry-steering")
                                        .disabled(!model.connected)
                                }
                                Button { model.cancelPending(message.id) } label: {
                                    Image(systemName: "xmark").frame(width: 44, height: 44).contentShape(Rectangle())
                                }.accessibilityLabel("Cancel queued message")
                                    .disabled(!model.connected || (model.cancellation(agentID: message.agentID, turnID: message.id).map { $0.error == nil } ?? false))
                            }.font(.system(size: 13, weight: .medium)).buttonStyle(.plain)
                                .padding(.horizontal, 16).padding(.vertical, 8)
                        }
                    }
                    .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { queueContentHeight = $0 }
                }.frame(height: min(queueContentHeight, visiblePending.count > 1 ? 240 : 180))
                    .padding(.top, 4)
                    .accessibilityIdentifier("pending-messages")
                Rectangle().fill(Ink.border).frame(height: 0.5).padding(.horizontal, 16)
            }
            if !attachments.isEmpty {
                ScrollView(.horizontal) {
                    HStack(spacing: 10) {
                        ForEach(attachments) { attachment in
                            VStack(alignment: .leading, spacing: 4) {
                                Group {
                                    if attachment.isVideo {
                                        AttachmentMovieThumbnail(attachment: attachment, poster: model.attachmentURL(attachment), movie: model.attachmentMovieURL(attachment))
                                    } else {
                                        AttachmentPhotoThumbnail(attachment: attachment, model: model)
                                    }
                                }
                                    .frame(width: 88, height: 76)
                                    .overlay(alignment: .topTrailing) {
                                        Button { model.removeAttachment(attachment.id) } label: {
                                            Image(systemName: "xmark.circle.fill").symbolRenderingMode(.palette)
                                                .foregroundStyle(Ink.text, Ink.background).frame(width: 44, height: 44)
                                        }.buttonStyle(.plain).accessibilityLabel("Remove " + attachment.name)
                                            .accessibilityIdentifier("remove-attachment-" + attachment.id)
                                    }
                                Text(attachment.name).font(.caption2).lineLimit(1)
                            }.frame(width: 88).accessibilityElement(children: .contain)
                                .accessibilityIdentifier("attachment-" + attachment.id)
                        }
                    }.padding(.horizontal, 16).padding(.top, 10).padding(.bottom, 6)
                }.scrollIndicators(.hidden).accessibilityIdentifier("composer-attachments")
                if attachments.contains(where: \.isVideo) {
                    Text("Original video").font(.caption).foregroundStyle(Ink.muted)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 16).padding(.bottom, 6)
                        .accessibilityIdentifier("video-analysis-description")
                }
            }
            if preparingAttachments {
                ProgressView().accessibilityLabel("Preparing attachments").font(.caption).frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16).padding(.vertical, 8).accessibilityIdentifier("preparing-attachments")
            }
            if let error = pickerError ?? model.attachmentError {
                Text(error).font(.caption).foregroundStyle(Ink.muted).frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16).padding(.vertical, 8).accessibilityIdentifier("attachment-error")
            }
            HStack(alignment: .bottom, spacing: 2) {
                Button { focused = false; showAttachmentMenu = true } label: {
                    Image(systemName: "plus").frame(width: 44, height: 44).contentShape(Rectangle())
                }.accessibilityLabel("Add attachments").accessibilityIdentifier("add-attachments")
                ChatComposerEditor(text: $model.draft, focused: $focused, overflowing: $composerOverflows,
                                   onPasteImages: pasteImages)
                    .accessibilityIdentifier("composer")
                    .overlay(alignment: .topLeading) {
                        if model.draft.isEmpty {
                            Text("Ask Nanocodex").font(.body).foregroundStyle(.tertiary)
                                .padding(.top, 8).allowsHitTesting(false).accessibilityHidden(true)
                        }
                    }
                if let agentID = card?.id {
                    NanocodexVoiceControl(session: model.voice, onReturnToChat: onVoiceChat) {
                        focused = false
                        return try await model.voiceConfiguration(agentID: agentID)
                    }.id(model.focusedConversationIdentity)
                }
                Button {
                    if sendShowsStop, let card = model.focused {
                        model.stop(agentID: card.id, turnID: model.stopTarget)
                    } else if model.send() {
                        #if os(iOS)
                        focused = false
                        #endif
                    }
                } label: {
                    Group {
                        if sendShowsStop, let stopRequest, stopRequest.error == nil {
                            ProgressView().tint(Ink.background)
                        } else {
                            Image(systemName: sendShowsStop ? "stop.fill" : model.busy.contains(card?.id ?? "") ? "ellipsis" : "arrow.up")
                        }
                    }.font(.system(size: 16, weight: .semibold)).frame(width: 32, height: 32)
                        .background(Ink.accent.opacity(sendShowsStop || canSend ? 1 : 0.22), in: Circle()).foregroundStyle(Ink.background)
                        .frame(width: 44, height: 44).contentShape(Rectangle())
                }.buttonStyle(.plain)
                    .disabled(sendShowsStop ? stopRequest.map { $0.error == nil } ?? false : !canSend)
                    .accessibilityLabel(sendShowsStop ? stopRequest.map { $0.error == nil ? "Stopping turn" : "Retry stop" } ?? "Stop turn" : "Send message")
                    .accessibilityIdentifier("send")
                    .keyboardShortcut(sendShowsStop ? nil : KeyboardShortcut(.return, modifiers: .command))
            }
                .overlay(alignment: .topTrailing) {
                    if composerOverflows {
                        Button {
                            focused = false
                            showExpandedEditor = true
                        } label: {
                            Image(systemName: "arrow.up.left.and.arrow.down.right")
                                .frame(width: 44, height: 44).contentShape(Rectangle())
                        }.buttonStyle(.plain).foregroundStyle(Ink.muted)
                            .accessibilityLabel("Expand message editor")
                            .accessibilityIdentifier("expand-composer")
                    }
                }
                .padding(.horizontal, 4).padding(.bottom, 4).padding(.top, visiblePending.isEmpty ? 4 : 0).accessibilityElement(children: .contain).accessibilityIdentifier("composer-input")

        }.background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 28))
            .overlay(RoundedRectangle(cornerRadius: 28).strokeBorder(Color.primary.opacity(focused ? 0.18 : 0.1)))
            .shadow(color: .black.opacity(0.035), radius: 8, y: 2)
            .padding(.horizontal, 12).padding(.top, 4).padding(.bottom, 6).background(Ink.background)
            .onChange(of: model.focusedConversationIdentity) { _, _ in
                showExpandedEditor = false
                focused = false
            }
            .sheet(isPresented: $showAttachmentMenu, onDismiss: openSelectedAttachmentAction) {
                NavigationStack {
                    List {
                        Section {
                            attachmentOption("Photos & Videos", icon: "photo.on.rectangle", action: .photos, identifier: "choose-photos")
                            attachmentOption("Camera", icon: "camera", action: .camera, identifier: "choose-camera")
                            attachmentOption("Files", icon: "folder", action: .files, identifier: "choose-files")
                        }
                        Section {
                            attachmentOption("Context from other apps", icon: "tray.full", action: .context, identifier: "choose-context")
                        }
                    }
                    .navigationTitle("Add to conversation")
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar {
                        ToolbarItem(placement: .confirmationAction) {
                            Button("Done") { showAttachmentMenu = false }
                        }
                    }
                }
                .tint(Ink.accent)
                .presentationDetents([.medium, .large])
                .presentationDragIndicator(.visible)
                .presentationCornerRadius(30)
            }
            .sheet(isPresented: $showExpandedEditor) {
                ExpandedAgentComposer(
                    draft: $model.draft,
                    canSend: model.canSend,
                    attachmentCount: model.focusedAttachments.count,
                    onPasteImages: pasteImages,
                    onCollapse: {
                        showExpandedEditor = false
                        focused = true
                    },
                    onSend: {
                        if model.send() {
                            showExpandedEditor = false
                            focused = false
                        }
                    }
                )
            }
            #if os(iOS)
            .fullScreenCover(isPresented: $showCamera, onDismiss: { cameraTarget = nil }) {
                CameraPicker { image in
                    if let image, let target = cameraTarget { model.importCameraPhoto(image, target: target) }
                    showCamera = false
                }.ignoresSafeArea()
            }
            .alert("Allow camera access", isPresented: $cameraPermissionDenied) {
                Button("Open Settings") { UIApplication.shared.open(URL(string: UIApplication.openSettingsURLString)!) }
                Button("Cancel", role: .cancel) {}
            } message: { Text("Enable Camera in Settings to take a photo for your message.") }
            #endif
            .photosPicker(isPresented: $showPhotos, selection: $selectedPhotos, maxSelectionCount: nil, matching: .any(of: [.images, .videos]), preferredItemEncoding: .current)
            .task(id: showPhotos ? [] : selectedPhotos) {
                // Selection can arrive in several updates. Keep the picker binding
                // intact until dismissal, then import the complete batch once.
                guard !showPhotos, !selectedPhotos.isEmpty else { return }
                await Task.yield()
                // A final selection update restarts this task, including when
                // the presentation binding changes before the selection binding.
                guard !Task.isCancelled, !showPhotos,
                      !selectedPhotos.isEmpty, let target = photoTarget else { return }
                let items = selectedPhotos
                selectedPhotos = []; photoTarget = nil
                model.importAttachmentPhotos(items, target: target)
            }
            .fileImporter(isPresented: $showFiles, allowedContentTypes: [.image, .movie], allowsMultipleSelection: true) { result in
                guard let target = fileTarget else { return }
                fileTarget = nil
                switch result {
                case .success(let urls): model.importAttachmentFiles(urls, target: target)
                case .failure(let error): pickerError = error.localizedDescription
                }
            }
    }

    private func pasteImages(_ providers: [NSItemProvider]) {
        guard let target = model.captureAttachmentTarget() else { return }
        pickerError = nil
        model.importAttachmentProviders(providers, target: target)
    }

    private func attachmentOption(_ title: String, icon: String, action: AttachmentAction, identifier: String) -> some View {
        Button {
            attachmentAction = action
            showAttachmentMenu = false
        } label: {
            Label(title, systemImage: icon).frame(minHeight: 32)
        }
        .disabled(model.preparingAttachments)
        .accessibilityIdentifier(identifier)
    }

    private func openSelectedAttachmentAction() {
        guard let action = attachmentAction else { return }
        attachmentAction = nil
        switch action {
        case .camera:
            Task { await openCamera() }
        case .photos:
            guard let target = model.captureAttachmentTarget() else { return }
            photoTarget = target; selectedPhotos = []; pickerError = nil; showPhotos = true
        case .files:
            guard let target = model.captureAttachmentTarget() else { return }
            fileTarget = target; pickerError = nil; showFiles = true
        case .context:
            model.showContext = true
        }
    }

    #if os(iOS)
    @MainActor private func openCamera() async {
        guard let target = model.captureAttachmentTarget() else { return }
        pickerError = nil
        guard UIImagePickerController.isSourceTypeAvailable(.camera) else {
            pickerError = "Camera is unavailable on this device. Choose Photos or Files instead."
            return
        }
        let status = AVCaptureDevice.authorizationStatus(for: .video)
        let allowed: Bool
        if status == .notDetermined { allowed = await AVCaptureDevice.requestAccess(for: .video) }
        else { allowed = status == .authorized }
        guard allowed else { cameraPermissionDenied = true; return }
        cameraTarget = target; focused = false; showCamera = true
    }
    #endif
}

private struct ExpandedAgentComposer: View {
    @Binding var draft: String
    let canSend: Bool
    let attachmentCount: Int
    let onPasteImages: ([NSItemProvider]) -> Void
    let onCollapse: () -> Void
    let onSend: () -> Void
    @State private var editorFocused = false
    @State private var editorOverflow = false

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 8) {
                ChatComposerEditor(text: $draft, focused: $editorFocused, overflowing: $editorOverflow,
                                   expandsToFill: true, onPasteImages: onPasteImages)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                    .accessibilityLabel("Message")
                    .accessibilityIdentifier("expanded-composer")
                    .overlay(alignment: .topLeading) {
                        if draft.isEmpty {
                            Text("Ask Nanocodex")
                                .foregroundStyle(.secondary)
                                .padding(.horizontal, 5).padding(.top, 8)
                                .allowsHitTesting(false)
                                .accessibilityHidden(true)
                        }
                    }
                if attachmentCount > 0 {
                    Label("Attachments: \(attachmentCount)", systemImage: "paperclip")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            .padding(16)
            .background(Ink.background)
            .navigationTitle("Message")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(action: onCollapse) {
                        Label("Collapse", systemImage: "arrow.down.right.and.arrow.up.left")
                    }.accessibilityLabel("Collapse message editor")
                        .accessibilityIdentifier("collapse-composer")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Send", action: onSend)
                        .disabled(!canSend)
                        .keyboardShortcut(.return, modifiers: .command)
                        .accessibilityIdentifier("expanded-composer-send")
                }
            }
            .task { editorFocused = true }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
    }
}

#if os(iOS)
private struct CameraPicker: UIViewControllerRepresentable {
    let finish: (UIImage?) -> Void
    func makeCoordinator() -> Coordinator { Coordinator(finish: finish) }
    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.mediaTypes = [UTType.image.identifier]
        picker.cameraCaptureMode = .photo
        picker.delegate = context.coordinator
        return picker
    }
    func updateUIViewController(_ controller: UIImagePickerController, context: Context) {}
    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let finish: (UIImage?) -> Void
        init(finish: @escaping (UIImage?) -> Void) { self.finish = finish }
        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) { finish(nil) }
        func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            finish(info[.originalImage] as? UIImage)
        }
    }
}
#endif

private enum AttachmentImageSource: Hashable, Sendable {
    case file(URL)
    case inline(String)
    case data(Data)
}

private func videoTime(_ seconds: Double) -> String {
    let value = max(0, Int(seconds.rounded(.down)))
    return String(format: "%d:%02d", value / 60, value % 60)
}

private struct AttachmentMovieThumbnail: View {
    let attachment: MessageAttachment
    let poster: URL?
    let movie: URL?
    @State private var selection: URL?
    var body: some View {
        Button { selection = movie } label: {
            AttachmentImageView(source: poster.map(AttachmentImageSource.file))
                .overlay { Image(systemName: "play.circle.fill").font(.title).foregroundStyle(.white).shadow(radius: 3) }
                .overlay(alignment: .bottomLeading) {
                    Text(videoTime(attachment.video?.duration ?? 0)).font(.caption2.monospacedDigit()).foregroundStyle(.white)
                        .padding(.horizontal, 5).padding(.vertical, 2).background(.black.opacity(0.65), in: Capsule()).padding(5)
                }
        }.buttonStyle(.plain).disabled(movie == nil)
            .accessibilityLabel("Preview " + attachment.name).accessibilityIdentifier("preview-video-" + attachment.id)
            .nativeMediaPreview($selection, in: movie.map { [$0] } ?? [], title: attachment.name)
    }
}

private struct AttachmentPhotoThumbnail: View {
    let attachment: MessageAttachment
    let model: InboxModel
    @State private var selection: URL?
    var body: some View {
        Button { selection = model.attachmentOriginalURL(attachment) } label: {
            AttachmentImageView(source: model.attachmentURL(attachment).map(AttachmentImageSource.file))
        }.buttonStyle(.plain).accessibilityLabel("Preview " + attachment.name)
            .accessibilityIdentifier("preview-image-" + attachment.id).nativeMediaPreview($selection, in: selection.map { [$0] } ?? [], title: attachment.name)
    }
}

private struct VideoAttachmentView: View {
    let video: TranscriptVideo
    let model: InboxModel
    let agentID: String
    var body: some View {
        ChatMediaPreview(title: video.name, load: {
            if video.path != nil { return [try await model.downloadVideo(video, agentID: agentID)] }
            var urls: [URL] = []
            do {
                for image in video.images { urls.append(try await ChatMediaFile.inline(image)) }
                return urls
            } catch {
                for url in urls { try? FileManager.default.removeItem(at: url) }
                throw error
            }
        }) {
            HStack(spacing: 10) {
                Image(systemName: "play.rectangle.fill").font(.title2)
                VStack(alignment: .leading, spacing: 3) {
                    Text(video.name).font(.subheadline).lineLimit(2)
                    Text(videoTime(video.duration) + (video.path == nil ? " · Saved frames" : ""))
                        .font(.caption).foregroundStyle(Ink.muted)
                }
            }.frame(minHeight: 44).contentShape(Rectangle())
                .accessibilityLabel(video.path == nil ? "Open saved video frames" : "Play video")
                .accessibilityIdentifier("play-original-video")
        }.frame(maxWidth: 260, alignment: .leading)
            .accessibilityElement(children: .contain).accessibilityIdentifier("message-video")
    }
}

private struct OriginalImageAttachmentView: View {
    let attachment: MessageAttachment
    let model: InboxModel
    let agentID: String
    @State private var preview: Data?
    @State private var visible = false
    @State private var error: String?
    @State private var selection: URL?
    private var localPreview: URL? {
        model.attachmentURL(attachment) ?? model.attachmentOriginalURL(attachment)
    }
    private var thumbnail: some View {
        AttachmentImageView(source: localPreview.map(AttachmentImageSource.file) ?? preview.map(AttachmentImageSource.data), contentMode: .fit)
            .frame(maxWidth: 240).frame(height: 180)
            .accessibilityLabel("Open " + attachment.name).accessibilityIdentifier("message-image")
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let original = model.attachmentOriginalURL(attachment) {
                // ChatMediaPreview owns and deletes disposable downloads. The
                // phone's retained original must survive preview dismissal.
                Button { selection = original } label: { thumbnail }
                    .buttonStyle(.plain)
                    .nativeMediaPreview($selection, in: [original], title: attachment.name)
            } else {
                ChatMediaPreview(title: attachment.name, load: { [try await model.downloadAttachment(attachment, agentID: agentID)] }) {
                    thumbnail
                }
            }
            if let error { Text(error).font(.caption).foregroundStyle(.secondary) }
        }
        .onScrollVisibilityChange(threshold: 0.01) { visible = $0 }
        .task(id: visible ? attachment.id : nil) {
            guard visible, preview == nil, localPreview == nil else { return }
            do {
                let data = try await model.attachmentPreview(attachment, agentID: agentID)
                guard !Task.isCancelled else { return }
                preview = data
            }
            catch is CancellationError { }
            catch { self.error = error.localizedDescription }
        }
    }
}

private struct AttachmentImageView: View {
    let source: AttachmentImageSource?
    var contentMode: ContentMode = .fill
    @State private var thumbnail: CGImage?
    @State private var visible = false

    var body: some View {
        Group {
            if let thumbnail {
                Image(decorative: thumbnail, scale: 1).resizable().aspectRatio(contentMode: contentMode)
            } else {
                Image(systemName: "photo").font(.title2).foregroundStyle(Ink.muted)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Ink.surface).clipShape(RoundedRectangle(cornerRadius: 12))
        .accessibilityElement(children: .ignore).accessibilityLabel("Attached image")
        .accessibilityValue(thumbnail == nil ? "Loading image" : "Image loaded")
        .onScrollVisibilityChange(threshold: 0.01) { visible = $0 }
        .task(id: visible ? source : nil) {
            thumbnail = nil
            guard visible else { return }
            let captured = source
            let decoded = await Task.detached(priority: .utility) { Self.decode(captured) }.value
            guard !Task.isCancelled else { return }
            thumbnail = decoded
        }
    }

    private nonisolated static func decode(_ source: AttachmentImageSource?) -> CGImage? {
        let image: CGImageSource?
        switch source {
        case .file(let url):
            guard url.isFileURL else { return nil }
            image = CGImageSourceCreateWithURL(url as CFURL, nil)
        case .inline(let value):
            guard value.hasPrefix("data:image/"), let separator = value.firstIndex(of: ","),
                  value[..<separator].hasSuffix(";base64"),
                  let data = Data(base64Encoded: String(value[value.index(after: separator)...])) else { return nil }
            image = CGImageSourceCreateWithData(data as CFData, nil)
        case .data(let data): image = CGImageSourceCreateWithData(data as CFData, nil)
        case nil: return nil
        }
        guard let image else { return nil }
        return CGImageSourceCreateThumbnailAtIndex(image, 0, [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 480,
            kCGImageSourceShouldCacheImmediately: true,
        ] as CFDictionary)
    }
}

private struct ConnectView: View {
    @ObservedObject var model: InboxModel
    @State private var origin = "https://nanocodex.gakonst.workers.dev"
    @State private var phone = ""
    @State private var region = PhoneNumberInput.defaultRegion()
    @State private var code = SMSCodeInput()
    @State private var inputError: String?
    @State private var verificationScheduled = false
    @FocusState private var focus: Field?
    private enum Field { case phone, code }
    private var normalizedPhone: String? { try? PhoneNumberInput.normalize(phone, region: region) }
    private var canVerify: Bool {
        !model.signingIn && !verificationScheduled && model.challenge != nil
            && (model.signInRetryAt ?? .distantPast) <= .now
    }
    private func verify(_ value: String) {
        guard canVerify else { return }
        verificationScheduled = true; code.markSubmitted()
        Task { await model.verifySignIn(code: value); verificationScheduled = false }
    }
    var body: some View {
        ScrollView {
        VStack(alignment: .leading, spacing: 24) {
            Image(systemName: "bubble.left.and.bubble.right").font(.system(size: 36, weight: .regular)).foregroundStyle(Ink.accent)
            Text(model.challenge == nil ? "What are we working on?" : "Check your messages")
                .font(.system(size: 34, weight: .semibold))
            Text(model.challenge.map { "Enter the 6-digit code sent to \($0.phone)." }
                 ?? "Sign in with the same phone number you use on Nanocodex. Your agents will be here.")
                .foregroundStyle(Ink.muted)
            VStack(alignment: .leading, spacing: 14) {
                Text(model.challenge == nil ? "Phone number" : "Verification code").font(.subheadline.weight(.medium))
                if model.challenge == nil {
                    Picker("Country", selection: $region) {
                        ForEach(PhoneNumberInput.countries) { country in
                            Text(country.label).tag(country.id)
                        }
                    }.pickerStyle(.menu).accessibilityIdentifier("phone-country")
                    TextField(PhoneNumberInput.example(region: region), text: $phone)
                        .textContentType(.telephoneNumber)
                        #if os(iOS)
                        .keyboardType(.phonePad)
                        #endif
                        .focused($focus, equals: .phone).accessibilityIdentifier("phone-number")
                        .padding(15).background(Ink.surface, in: RoundedRectangle(cornerRadius: 12))
                        .onChange(of: phone) { _, _ in inputError = nil }
                } else {
                    TextField("000000", text: Binding(get: { code.text }, set: { value in
                        let automatic = canVerify && (model.challenge?.expiresAt ?? .distantPast) > .now
                        if let complete = code.update(value, canSubmit: automatic) { verify(complete) }
                    }))
                        .textContentType(.oneTimeCode)
                        #if os(iOS)
                        .keyboardType(.numberPad)
                        #endif
                        .font(.title2.monospacedDigit()).tracking(7).multilineTextAlignment(.center)
                        .focused($focus, equals: .code).accessibilityIdentifier("verification-code")
                        .padding(15).background(Ink.surface, in: RoundedRectangle(cornerRadius: 12))
                }
                Text(model.challenge == nil ? normalizedPhone.map { "We’ll text a code to \($0)." } ?? "We’ll add the country code for you." : "You’ll stay signed in securely on this device.")
                    .font(.caption).foregroundStyle(Ink.muted)
                    .accessibilityIdentifier("sign-in-hint")
            }.disabled(model.signingIn)
            if let error = inputError ?? model.signInError ?? model.error {
                Text(error).font(.subheadline).foregroundStyle(Ink.amber).accessibilityIdentifier("sign-in-error")
            }
            TimelineView(.periodic(from: .now, by: 1)) { context in
                let retry = max(0, Int(ceil((model.signInRetryAt ?? .distantPast).timeIntervalSince(context.date))))
                VStack(spacing: 18) {
                    Button {
                        if model.challenge == nil {
                            do {
                                let normalized = try PhoneNumberInput.normalize(phone, region: region)
                                inputError = nil
                                Task { await model.startSignIn(phone: normalized, origin: origin) }
                            } catch { inputError = error.localizedDescription }
                        } else {
                            verify(code.text)
                        }
                    } label: {
                        HStack(spacing: 9) {
                            if model.signingIn { ProgressView().tint(Ink.background) }
                            Text(model.signingIn ? "Connecting…" : retry > 0 ? "Try again in \(retry)s" : model.challenge == nil ? "Continue" : "Sign in")
                                .fontWeight(.semibold)
                        }.frame(maxWidth: .infinity).padding(.vertical, 14)
                            .foregroundStyle(Ink.background).background(Ink.accent, in: RoundedRectangle(cornerRadius: 13))
                    }
                    .buttonStyle(.plain).accessibilityIdentifier("sign-in-submit")
                    .disabled(model.signingIn || verificationScheduled || retry > 0 || (model.challenge == nil ? phone.trimmingCharacters(in: .whitespaces).isEmpty : code.text.count != 6 || !canVerify))
                    if let challenge = model.challenge {
                        let seconds = max(retry, max(0, Int(ceil(challenge.resendAt.timeIntervalSince(context.date)))))
                        if context.date >= challenge.expiresAt {
                            Text("This code expired. Request another one.").font(.caption).foregroundStyle(Ink.muted)
                        }
                        HStack {
                            Button("Change number") {
                                Task { if await model.cancelSignIn() { code = SMSCodeInput(); focus = .phone } }
                            }
                            Spacer()
                            Button(seconds > 0 ? "Resend in \(seconds)s" : "Resend code") {
                                Task { await model.startSignIn(phone: challenge.phone, origin: origin); code = SMSCodeInput() }
                            }.disabled(seconds > 0)
                        }.font(.subheadline).disabled(model.signingIn)
                    }
                }
            }
            if model.challenge == nil {
                DisclosureGroup("Advanced") {
                    TextField("Server", text: $origin).textFieldStyle(.roundedBorder).autocorrectionDisabled()
                        #if os(iOS)
                        .textInputAutocapitalization(.never).keyboardType(.URL)
                        #endif
                        .padding(.top, 8)
                }.font(.caption).foregroundStyle(Ink.muted).disabled(model.signingIn)
            }
            #if DEBUG
            Button("Explore the demo") {
                Task { if await model.cancelSignIn() { model.demo() } }
            }.disabled(model.signingIn)
            #endif
        }.padding(32).frame(maxWidth: 480)
        }.scrollDismissesKeyboard(.interactively)
            .onChange(of: model.challenge?.phone) { _, value in if value != nil { code = SMSCodeInput(); focus = .code } }
            .onChange(of: region) { _, _ in inputError = nil }
            .accessibilityIdentifier("phone-onboarding")
    }
}

private struct ConversationMessageView: View {
    let row: TranscriptRow
    @ObservedObject var model: InboxModel
    let agentID: String
    var body: some View {
        let steering = model.steeringTransfer(row.turnID ?? row.id)
        let canWithdraw = steering.map { transfer in
            model.cards.first(where: { $0.id == agentID })?.activeTurns.contains(transfer.targetTurnID) == true
        } ?? false
        let delivery = model.pending.first { $0.agentID == agentID && $0.id == (row.turnID ?? row.id) }
        ConversationMessageContent(row: row, model: model, agentID: agentID, steering: steering, canWithdraw: canWithdraw,
                                   delivery: delivery, canRetry: model.connected && !model.busy.contains(agentID)).equatable()
    }
}

private struct ConversationMessageContent: View, Equatable {
    let row: TranscriptRow
    let model: InboxModel
    let agentID: String
    let steering: SteeringTransfer?
    let canWithdraw: Bool
    let delivery: PendingMessage?
    let canRetry: Bool
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.row == rhs.row && lhs.agentID == rhs.agentID && lhs.steering == rhs.steering && lhs.canWithdraw == rhs.canWithdraw
            && lhs.delivery == rhs.delivery && lhs.canRetry == rhs.canRetry
            && lhs.model === rhs.model
    }
    var body: some View {
        HStack(alignment: .top, spacing: 0) {
            if row.role == "You" { Spacer(minLength: 44) }
            VStack(alignment: .leading, spacing: 10) {
                if row.role == "Thinking" {
                    ChatMarkdown(text: row.text, compact: true)
                        .foregroundStyle(Ink.muted)
                } else if row.role == "You", let receipt = BrowserReceiptPresentation.summary(row.text) {
                    Label(receipt, systemImage: "lock.shield").font(.subheadline)
                } else if row.role == "You", let content = ContextPrompt.separate(row.text) {
                    Text(content.request).font(.body).lineSpacing(3).textSelection(.enabled)
                    DisclosureGroup("Captured context (\(content.captures.count))") {
                        ForEach(Array(content.captures.enumerated()), id: \.offset) { _, capture in
                            VStack(alignment: .leading, spacing: 4) {
                                Text(capture.source + (capture.sender.isEmpty ? "" : " · " + capture.sender)).font(.caption.weight(.semibold))
                                Text(capture.text.isEmpty ? capture.url : capture.text).font(.subheadline).textSelection(.enabled)
                            }.padding(.vertical, 4)
                        }
                    }.font(.caption).foregroundStyle(Ink.muted)
                } else if row.role == "Agent", !row.text.isEmpty {
                    ChatMarkdown(text: row.text, compact: true)
                } else if !row.text.isEmpty {
                    Text(row.text).font(.system(size: row.role == "Status" ? 14 : 17))
                        .lineSpacing(5).textSelection(.enabled)
                        .foregroundStyle(row.role == "Status" ? Ink.muted : Ink.text)
                }
                if !row.detail.isEmpty { Text(row.detail).font(.caption).foregroundStyle(Ink.muted) }
                if let delivery, delivery.phase == .failed {
                    Label("Couldn’t confirm delivery", systemImage: "exclamationmark.circle").font(.caption).foregroundStyle(.secondary)
                    HStack {
                        Button("Retry") { model.retryPending(delivery.id) }.accessibilityIdentifier("retry-pending")
                        Button("Cancel") { model.cancelPending(delivery.id) }.accessibilityLabel("Cancel message")
                    }.font(.caption).disabled(!canRetry)
                }
                if let transfer = steering, transfer.direct == true, transfer.error != nil, transfer.canResume {
                    Button("Retry sending") { model.steerNow(transfer.id) }
                        .font(.caption).accessibilityIdentifier("retry-steering")
                        .disabled(!model.connected)
                }
                if let transfer = steering, canWithdraw, (transfer.wasAccepted || transfer.phase == .unconfirmed), transfer.phase != .withdrawn {
                    Button(transfer.phase == .withdrawing ? "Withdrawing…" : "Withdraw steering") { model.withdrawSteering(transfer.id) }
                        .font(.caption).accessibilityIdentifier("withdraw-steering")
                        .disabled(!model.connected || (transfer.phase == .withdrawing && transfer.error == nil))
                }
                if let images = row.images {
                    ForEach(Array(images.enumerated()), id: \.offset) { _, image in
                        ConversationUserImageView(source: image).frame(maxWidth: 240)
                            .accessibilityIdentifier("message-image")
                    }
                }
                if let attachments = delivery?.attachments, !attachments.isEmpty {
                    ForEach(attachments) { attachment in
                        if attachment.isVideo {
                            AttachmentMovieThumbnail(attachment: attachment, poster: model.attachmentURL(attachment), movie: model.attachmentMovieURL(attachment))
                                .frame(width: 240, height: 180)
                        } else {
                            AttachmentPhotoThumbnail(attachment: attachment, model: model).frame(width: 240, height: 180)
                        }
                    }
                } else {
                    ForEach(row.imageFiles ?? []) { OriginalImageAttachmentView(attachment: $0, model: model, agentID: agentID) }
                    ForEach(row.videos ?? []) { VideoAttachmentView(video: $0, model: model, agentID: agentID) }
                }
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel(row.role == "You" ? "Your message" : row.role == "Agent" ? "Assistant message" : row.role)
            .padding(.horizontal, row.role == "You" ? 12 : 0)
            .padding(.vertical, row.role == "You" || row.role == "Agent" ? 9 : 0)
            .background(row.role == "You" ? Ink.userMessage : Color.clear,
                        in: RoundedRectangle(cornerRadius: 18))
            .contextMenu {
                if row.role == "Agent", !row.text.isEmpty {
                    ChatCopyButton(text: row.text, showsLabel: true)
                }
            }
            .accessibilityAction(named: "Copy response") {
                UIPasteboard.general.string = row.text
            }
            if row.role != "You" { Spacer(minLength: row.role == "Agent" ? 16 : 0) }
        }.frame(maxWidth: .infinity, alignment: row.role == "You" ? .trailing : .leading)
    }
}

private final class ConversationReadingPositions {
    struct Position {
        var atLatest: Bool
        var rowID: String? = nil
        var offsetY: CGFloat = 0
        var childID: String? = nil
    }
    var values: [String: Position] = [:]
    var tools: [String: ConversationToolExpansion] = [:]
    func toolExpansion(for identity: String) -> ConversationToolExpansion {
        if let value = tools[identity] { return value }
        let value = ConversationToolExpansion()
        tools[identity] = value
        return value
    }
}

@Observable private final class ConversationToolExpansion {
    var collapsedAll = false
    var expanded: [String: Bool] = [:]
    func binding(_ id: String, initiallyExpanded: Bool = false) -> Binding<Bool> {
        Binding(get: { self.expanded[id] ?? (initiallyExpanded && !self.collapsedAll) },
                set: { self.expanded[id] = $0 })
    }
    func collapseAll() { collapsedAll = true; expanded.removeAll() }
}

private struct ConversationRenderedItem: Identifiable, Equatable, Sendable {
    var id: String
    var content: ConversationItem?
    var output: ChatGeneratedOutput?
    var sourceRowID: String?
    var message: TranscriptRow? { content?.message }
    static func project(_ groups: [ConversationItem], outputs: [String: [ChatGeneratedOutput]]) -> [Self] {
        var result: [Self] = []
        var seenByTurn: [String: Set<String>] = [:]
        for group in groups {
            result.append(Self(id: group.id, content: group))
            for row in group.activity {
                for output in outputs[row.id] ?? [] where seenByTurn[row.turnID ?? row.id, default: []].insert(output.id).inserted {
                    result.append(Self(id: group.id + ":output:" + output.id, output: output, sourceRowID: row.id))
                }
            }
        }
        return result
    }
}

// Use measured row heights; decoding follows native viewport visibility.
private struct ConversationUserImageView: View {
    let source: String
    @State private var visible = false
    var body: some View {
        ChatImageAttachment(source: source, loadsThumbnail: visible)
            .onScrollVisibilityChange(threshold: 0.01) { visible = $0 }
    }
}

private struct ConversationOutputView: View {
    let output: ChatGeneratedOutput
    @State private var visible = false
    var body: some View {
        ChatGeneratedOutputView(output: output, loadsThumbnail: visible)
            .onScrollVisibilityChange(threshold: 0.01) { visible = $0 }
    }
}

// A single immutable projection per transcript revision. Composer updates read
// the retained snapshot; grouping and output projection run off the main actor.
@MainActor
private final class ConversationRenderProjection: ObservableObject {
    struct Value: Sendable {
        var revision: UUID
        var identity: String
        var rows: [TranscriptRow]
        var pending: [PendingMessage]
        var items: [ConversationRenderedItem]
        var itemsByID: [String: ConversationRenderedItem]
    }
    @Published private(set) var value: Value?
    private(set) var rebuildCount: UInt64 = 0

    private var preparation: Task<Void, Never>?
    private var preparationID = UUID()

    func request(_ model: InboxModel, identity: String) {
        guard preparation == nil else { return }
        // Revision changes coalesce behind one worker instead of cancelling
        // expensive grouping on every incoming tool event.
        let requestID = UUID()
        preparationID = requestID
        preparation = Task { [weak self] in
            guard let self else { return }
            defer { if self.preparationID == requestID { self.preparation = nil } }
            while !Task.isCancelled, model.focusedConversationIdentity == identity {
                await self.prepare(model, identity: identity)
                if self.value?.revision == model.focusedTranscriptRevision { return }
                await Task.yield()
            }
        }
    }

    func cancel() {
        preparation?.cancel()
        preparation = nil
        preparationID = UUID()
    }

    private func prepare(_ model: InboxModel, identity: String) async {
        let revision = model.focusedTranscriptRevision
        if value?.revision == revision, value?.identity == identity { return }
        // Capture all inputs before suspension, so a completed older snapshot
        // never mixes its rows with a newer revision's turns or media.
        let turns = model.focused?.activeTurns ?? []
        let outputs = model.generatedOutputsByRow
        guard let queue = await model.prepareFocusedQueue(),
              !Task.isCancelled, model.focusedConversationIdentity == identity else { return }
        let rows = queue.rows
        let pending = queue.messages
        rebuildCount = rebuildCount == .max ? .max : rebuildCount + 1
        let worker = Task.detached(priority: .userInitiated) { () -> Value? in
            guard !Task.isCancelled else { return nil }
            let log = OSLog(subsystem: "ai.nanocodex.inbox", category: "ConversationRendering")
            let signpost = OSSignpostID(log: log)
            os_signpost(.begin, log: log, name: "ConversationProjection", signpostID: signpost)
            defer { os_signpost(.end, log: log, name: "ConversationProjection", signpostID: signpost) }
            let groups = ConversationItem.group(rows, activeTurns: turns)
            guard !Task.isCancelled else { return nil }
            let items = ConversationRenderedItem.project(groups, outputs: outputs)
            guard !Task.isCancelled else { return nil }
            return Value(revision: revision, identity: identity, rows: rows, pending: pending,
                         items: items, itemsByID: Dictionary(uniqueKeysWithValues: items.map { ($0.id, $0) }))
        }
        let prepared = await withTaskCancellationHandler {
            await worker.value
        } onCancel: {
            worker.cancel()
        }
        guard !Task.isCancelled, let prepared,
              model.focusedConversationIdentity == identity else { return }
        value = prepared
    }
}

private struct ConversationView: View {
    @ObservedObject var model: InboxModel
    let identity: String
    let readingPositions: ConversationReadingPositions
    @StateObject private var projection = ConversationRenderProjection()

    var body: some View {
        let revision = model.focusedTranscriptRevision
        // Never display another conversation's retained projection while loading.
        let rendered = projection.value.flatMap { $0.identity == identity ? $0 : nil }
        let preparing = rendered?.revision != revision
        ConversationContentView(model: model,
                                identity: identity, readingPositions: readingPositions,
                                tools: readingPositions.toolExpansion(for: identity),
                                revision: .init(projectionRevision: rendered?.revision, preparing: preparing,
                                                rows: rendered?.rows ?? [], items: rendered?.items ?? [],
                                                itemsByID: rendered?.itemsByID ?? [:], pending: rendered?.pending ?? [],
                                                title: model.focused?.title ?? "Conversation",
                                                activeTurns: model.focused?.activeTurns ?? [],
                                                loading: model.threadLoading || (rendered == nil && preparing), error: model.threadError,
                                                hasOlder: model.hasOlder, loadingOlder: model.loadingOlder || preparing,
                                                hasNewer: model.hasNewer, loadingNewer: model.loadingNewer || preparing))
            .task(id: revision) { projection.request(model, identity: identity) }
            .onDisappear { projection.cancel() }
            #if DEBUG
            .overlay(alignment: .topTrailing) {
                if ProcessInfo.processInfo.environment["NANOCODEX_RENDER_COUNTER"] == "1" {
                    Text(String(projection.rebuildCount)).font(.caption2)
                        .accessibilityIdentifier("conversation-projection-count")
                        .allowsHitTesting(false)
                }
            }
            #endif
    }
}

// Scroll and reading-position state must invalidate this view independently
// of transcript revisions. Keep equality boundaries on rendered messages only.
private struct ConversationContentView: View {
    struct Revision: Equatable {
        var projectionRevision: UUID?
        var preparing: Bool
        var rows: [TranscriptRow]
        var items: [ConversationRenderedItem]
        var itemsByID: [String: ConversationRenderedItem]
        var pending: [PendingMessage]
        var title: String
        var activeTurns: [String]
        var loading: Bool
        var error: String?
        var hasOlder: Bool
        var loadingOlder: Bool
        var hasNewer: Bool
        var loadingNewer: Bool
    }
    private var historyBoundaryItemID: String? {
        guard let boundary = model.newerHistoryBoundary else { return nil }
        let earlier = Set(revision.rows.filter { $0.cursor.map { $0 <= boundary } == true }.map(\.id))
        return revision.items.last { item in
            earlier.contains(item.sourceRowID ?? item.id)
                || item.content?.activity.contains(where: { earlier.contains($0.id) }) == true
        }?.id
    }
    private let verticalPadding: CGFloat = 24
    let model: InboxModel
    let identity: String
    let readingPositions: ConversationReadingPositions
    let tools: ConversationToolExpansion
    let revision: Revision
    private struct UserNavigationTargets: Equatable {
        var previous: String?
        var next: String?
    }
    @State private var userNavigationTargets = UserNavigationTargets()
    @State private var selectedUserMessage: String?
    @State private var pendingUserDirection: HistoryDirection?
    @State private var navigationKnownIDs: Set<String> = []
    @State private var navigationProjectionRevision: UUID?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.conversationNavigationActive) private var navigationActive
    @State private var followsLatest = true
    @State private var isInteractingTranscript = false
    @State private var isScrollGestureActive = false
    @State private var scrollsTowardLatest = false
    @State private var hasInitialPosition = false
    @State private var pendingReadingRestore: ConversationReadingPositions.Position?
    @State private var rowGeometry = ConversationRowGeometry()
    private var historyRestore: (id: String, offsetY: CGFloat, childID: String?)? {
        get { rowGeometry.historyRestore }
        nonmutating set { rowGeometry.historyRestore = newValue }
    }
    @State private var historyContent = ConversationContentPosition()
    @State private var historyReady = false
    private enum HistoryDirection { case older, newer }
    @State private var historyDirection: HistoryDirection?
    @State private var historyRequestInFlight = false
    @State private var historyRequestRevision: UUID?
    private func rememberHistoryPosition(in viewport: GeometryProxy) {
        let visible = rowGeometry.frames.filter { revision.itemsByID[$0.key] != nil && $0.value.maxY > 0 && $0.value.minY < viewport.size.height }
        let sourceRows = visible.keys.flatMap { key -> [String] in
            guard let item = revision.itemsByID[key] else { return [] }
            return (item.content?.activity.map(\.id) ?? []) + (item.sourceRowID.map { [$0] } ?? [])
        }
        model.protectHistoryRows(Set(visible.keys).union(sourceRows))
        guard let first = visible.min(by: { $0.value.minY < $1.value.minY }) else { return }
        historyRestore = (first.key, first.value.minY, nil)
    }
    private func loadHistory(_ direction: HistoryDirection, in viewport: GeometryProxy) {
        guard model.focusedConversationIdentity == identity, pendingReadingRestore == nil,
              historyReady, !revision.preparing, !model.threadLoading, !model.loadingOlder, !model.loadingNewer,
              !historyRequestInFlight, direction == .older ? model.hasOlder : model.hasNewer else { return }
        historyRequestInFlight = true
        historyDirection = nil
        historyRequestRevision = model.historyMutationRevision
        rememberHistoryPosition(in: viewport)
        Task {
            if direction == .older { await model.loadOlder() }
            else { await model.loadNewer() }
            guard model.focusedConversationIdentity == identity else { return }
            if model.historyMutationRevision == historyRequestRevision { historyRestore = nil }
            historyRequestInFlight = false
        }
    }
    private func restoreHistoryPosition(using scroll: ScrollViewProxy) {
        guard !revision.preparing, !model.loadingOlder, !model.loadingNewer,
              let target = historyRestore else { return }
        var transaction = Transaction()
        transaction.disablesAnimations = true
        withTransaction(transaction) {
            let position = ConversationReadingPositions.Position(atLatest: false, rowID: target.id, offsetY: target.offsetY, childID: target.childID)
            readingPositions.values[identity] = position
            pendingReadingRestore = position
            scroll.scrollTo(target.id, anchor: .top)
            historyRestore = nil
        }
    }
    private func updateHistoryPosition(in viewport: GeometryProxy) {
        guard model.focusedConversationIdentity == identity, hasInitialPosition,
              pendingReadingRestore == nil, !revision.preparing, !navigationActive, historyContent.isMeasured else { return }
        // Ignore the transient top layout before a newly opened conversation
        // reaches its initial position at the bottom.
        if !historyReady {
            guard historyContent.atLatest || readingPositions.values[identity]?.atLatest == false else { return }
            historyReady = true
        }
        model.setHistoryAtLatest(historyContent.atLatest)
        if historyDirection == .older, historyContent.approachingTop { model.prefetchOlder() }
        // Layout changes also cross these thresholds. Only the reader's chosen
        // direction may load a page, so trimming cannot undo their navigation.
        if historyDirection == .older, historyContent.nearTop { loadHistory(.older, in: viewport) }
        else if historyDirection == .newer,
                historyContent.atLatest || rowGeometry.frames["history-gap"].map({ $0.minY < viewport.size.height + 240 }) == true {
            loadHistory(.newer, in: viewport)
        }
    }
    private func saveReadingPosition(in viewport: GeometryProxy) {
        guard model.focusedConversationIdentity == identity, hasInitialPosition,
              pendingReadingRestore == nil, historyReady, historyContent.isMeasured,
              !historyRequestInFlight, !revision.preparing, !navigationActive else { return }
        if !followsLatest {
            let visible = rowGeometry.frames.filter {
                revision.itemsByID[$0.key] != nil && $0.value.maxY > 0 && $0.value.minY < viewport.size.height
            }
            let sourceRows = visible.keys.flatMap { key -> [String] in
            guard let item = revision.itemsByID[key] else { return [] }
            return (item.content?.activity.map(\.id) ?? []) + (item.sourceRowID.map { [$0] } ?? [])
        }
            model.protectHistoryRows(Set(visible.keys).union(sourceRows))
        }
        if followsLatest && !model.needsLatestHistory {
            readingPositions.values[identity] = .init(atLatest: true)
        } else if let first = rowGeometry.frames.filter({ revision.itemsByID[$0.key] != nil && $0.value.maxY > 0 && $0.value.minY < viewport.size.height })
            .min(by: {
                let leftFull = $0.value.minY >= 0 && $0.value.maxY <= viewport.size.height
                let rightFull = $1.value.minY >= 0 && $1.value.maxY <= viewport.size.height
                return leftFull == rightFull ? $0.value.minY < $1.value.minY : leftFull
            }) {
            // Store a point offset, not a fraction of the current viewport.
            // The keyboard may still be changing its height when switching tabs.
            readingPositions.values[identity] = .init(atLatest: false, rowID: first.key, offsetY: first.value.minY)
        }
    }
    private func followLatest(using scroll: ScrollViewProxy) {
        guard followsLatest, pendingReadingRestore == nil,
              !model.needsLatestHistory, !isScrollGestureActive, !navigationActive else { return }
        // Animate only the scroll offset, not the transcript's text or tool state.
        // Initial positioning and accessibility Reduce Motion remain immediate.
        withAnimation(hasInitialPosition && !reduceMotion ? .smooth(duration: 0.24) : nil) {
            scroll.scrollTo("latest", anchor: .bottom)
        }
    }
    private var userMessages: [ConversationRenderedItem] {
        revision.items.filter { $0.message?.role == "You" }
    }
    private func userTarget(_ direction: HistoryDirection) -> String? {
        let users = userMessages
        if let selectedUserMessage, let index = users.firstIndex(where: { $0.id == selectedUserMessage }) {
            let next = direction == .older ? index - 1 : index + 1
            return users.indices.contains(next) ? users[next].id : nil
        }
        if historyContent.atLatest { return direction == .older ? users.last?.id : nil }
        return direction == .older ? userNavigationTargets.previous : userNavigationTargets.next
    }
    private func jumpToUser(_ id: String, using scroll: ScrollViewProxy) {
        followsLatest = false
        historyDirection = nil
        historyRestore = nil
        selectedUserMessage = id
        pendingUserDirection = nil
        pendingReadingRestore = .init(atLatest: false, rowID: id, offsetY: 0)
        readingPositions.values[identity] = pendingReadingRestore
        // Measured restoration retains the target through streaming and layout changes.
        scroll.scrollTo(id, anchor: .top)
    }
    private func navigateUser(_ direction: HistoryDirection, using scroll: ScrollViewProxy) {
        if let id = userTarget(direction) { jumpToUser(id, using: scroll); return }
        navigationKnownIDs = Set(revision.items.map(\.id))
        pendingUserDirection = direction
        pendingReadingRestore = nil
        historyRestore = nil
        historyDirection = nil
        followsLatest = false
        fetchUserHistory(direction)
    }
    private func fetchUserHistory(_ direction: HistoryDirection) {
        navigationProjectionRevision = revision.projectionRevision
        Task {
            guard model.focusedConversationIdentity == identity else { return }
            let before = model.focusedTranscriptRevision
            if direction == .older { await model.loadOlder() }
            else { await model.loadNewer() }
            guard model.focusedConversationIdentity == identity else { return }
            if model.focusedTranscriptRevision == before { pendingUserDirection = nil }
        }
    }
    private func continueUserNavigation(using scroll: ScrollViewProxy) {
        guard model.focusedConversationIdentity == identity,
              let direction = pendingUserDirection, !revision.preparing,
              !model.loadingOlder, !model.loadingNewer else { return }
        // Model history finishes before its off-main render projection. Wait for
        // that publication before deciding whether another page is necessary.
        guard revision.projectionRevision != navigationProjectionRevision || revision.error != nil else { return }
        // Only inspect the requested side of the retained window. A live user
        // message can arrive at the opposite end while history is in flight.
        let page = direction == .older
            ? Array(revision.items.prefix { !navigationKnownIDs.contains($0.id) })
            : Array(revision.items.reversed().prefix { !navigationKnownIDs.contains($0.id) }.reversed())
        let candidates = page.filter { $0.message?.role == "You" }
        if let target = direction == .older ? candidates.last : candidates.first {
            jumpToUser(target.id, using: scroll)
        } else if revision.error == nil && (direction == .older ? model.hasOlder : model.hasNewer) {
            navigationKnownIDs.formUnion(revision.items.map(\.id))
            fetchUserHistory(direction)
        } else { pendingUserDirection = nil }
    }
    private func threadControls(using scroll: ScrollViewProxy) -> some View {
        HStack(spacing: 0) {
            Button {
                followsLatest = false
                if let first = rowGeometry.frames.filter({ revision.itemsByID[$0.key] != nil && $0.value.maxY > 0 })
                    .min(by: { $0.value.minY < $1.value.minY }) {
                    pendingReadingRestore = .init(atLatest: false, rowID: first.key,
                        offsetY: revision.itemsByID[first.key]?.message == nil ? max(0, first.value.minY) : first.value.minY)
                }
                tools.collapseAll()
            } label: { Image(systemName: "rectangle.compress.vertical").frame(width: 44, height: 44) }
                .accessibilityLabel("Collapse all tool calls")
                .accessibilityIdentifier("collapse-all-tools")
            Divider().frame(height: 20)
            Button { navigateUser(.older, using: scroll) } label: {
                Image(systemName: "arrow.up").frame(width: 44, height: 44)
            }.accessibilityLabel("Previous user message").accessibilityIdentifier("previous-user-message")
                .disabled(userTarget(.older) == nil && !model.hasOlder)
            Button { navigateUser(.newer, using: scroll) } label: {
                Image(systemName: "arrow.down").frame(width: 44, height: 44)
            }.accessibilityLabel("Next user message").accessibilityIdentifier("next-user-message")
                .disabled(userTarget(.newer) == nil && !model.hasNewer)
        }
        .buttonStyle(.plain)
        .background(.regularMaterial, in: Capsule())
        .overlay(Capsule().strokeBorder(Ink.border, lineWidth: 0.5))
        .disabled(revision.loading || revision.preparing || model.loadingOlder || model.loadingNewer || pendingUserDirection != nil)
        .padding(.horizontal, 20).padding(.vertical, 4)
        .frame(maxWidth: .infinity, alignment: .trailing)
    }
    var body: some View {
        ScrollViewReader { scroll in
            VStack(spacing: 0) {
            // Measure only the transcript viewport: scrollTo anchors exclude
            // the thread controls below it when restoring a reading offset.
            GeometryReader { viewport in
            let boundaryItemID = historyBoundaryItemID
            ZStack(alignment: .top) {
            ScrollView {
                // Restoration uses measured row offsets. Lazy height estimates
                // feed back into scrollTo while prepending variable-height tools.
                VStack(alignment: .leading, spacing: 18) {
                    if revision.rows.isEmpty, revision.pending.isEmpty, !revision.loading, revision.error == nil {
                        VStack(alignment: .leading, spacing: 8) {
                            if model.hasOlder {
                                Text("Earlier messages").font(.title2.weight(.medium))
                                Button("Load earlier messages") { Task { await model.loadOlder() } }
                                    .disabled(model.loadingOlder).accessibilityIdentifier("load-older")
                            } else {
                                Text("Start a conversation").font(.title2.weight(.medium))
                                Text("Send a message to begin.").foregroundStyle(Ink.muted)
                            }
                        }.padding(.top, 24).accessibilityElement(children: .contain).accessibilityIdentifier("conversation-empty")
                    }
                    if let error = revision.error { Text(error).font(.subheadline).foregroundStyle(Ink.muted) }
                    ForEach(revision.items) { item in
                        VStack(alignment: .leading, spacing: 0) {
                        if let row = item.message {
                        ConversationMessageView(row: row, model: model, agentID: model.focused?.id ?? "")
                        } else if let output = item.output {
                            ConversationOutputView(output: output)
                        } else if let content = item.content {
                            ForEach(content.activity.filter { $0.tool?.vaultIntake != nil }) { row in
                                if let intake = row.tool?.vaultIntake {
                                    VaultIntakeCard(model: model, intake: intake)
                                        .id("\(row.id):\(model.vaultIntakeAccount)")
                                        .padding(.bottom, 12)
                                }
                            }
                            let onToggle = {
                                followsLatest = false
                                pendingReadingRestore = rowGeometry.frames[item.id].map {
                                    .init(atLatest: false, rowID: item.id, offsetY: $0.minY)
                                }
                                if historyRequestInFlight { rememberHistoryPosition(in: viewport) }
                            }
                            if content.isCodeModeBatch {
                                ConversationCodeModeBatch(item: content, tools: tools, expanded: tools.binding(content.id, initiallyExpanded: true), showsJavaScript: tools.binding(content.id + ":javascript"), onToggle: onToggle)
                            } else {
                                ForEach(content.activity) { row in
                                    ConversationToolCard(row: row, live: content.isRunning && row.running, expanded: tools.binding(row.id), onToggle: onToggle)
                                }
                            }
                        }
                        }.id(item.id)
                            .background(GeometryReader { geometry in
                                Color.clear.preference(key: ConversationRowFrames.self, value: [item.id: geometry.frame(in: .named("conversation-viewport"))])
                            })
                            .accessibilityElement(children: .contain)
                            .accessibilityIdentifier((item.message?.role == "You" ? "message-user-" : item.message?.role == "Agent" ? "message-assistant-" : "message-") + item.id)
                        if item.id == boundaryItemID {
                            Color.clear.frame(height: 1).accessibilityHidden(true)
                                .background(GeometryReader { geometry in
                                    Color.clear.preference(key: ConversationRowFrames.self,
                                        value: ["history-gap": geometry.frame(in: .named("conversation-viewport"))])
                                })
                        }
                    }

                    if let agentID = model.focused?.id {
                        NanocodexVoiceTranscript(session: model.voice, conversationID: agentID, durableRows: revision.rows, rowContent: { transcript in
                            let row = TranscriptRow(id: "voice-" + transcript.id.uuidString,
                                                    role: transcript.speaker == "user" ? "You" : "Agent", text: transcript.text)
                            return AnyView(ConversationMessageView(row: row, model: model, agentID: agentID)
                                .accessibilityIdentifier("voice-transcript-" + transcript.speaker))
                        }) {
                            followLatest(using: scroll)
                        }
                    }
                    Color.clear.frame(height: 1).id("latest")
                }.padding(.horizontal, 20).padding(.vertical, verticalPadding).frame(maxWidth: 780).frame(maxWidth: .infinity)

            }
            // Clip hit testing as well as drawing: offscreen disclosure buttons
            // must not intercept the header at accessibility text sizes.
            .contentShape(Rectangle())
            .defaultScrollAnchor(.top)
            // Keep layout from snapping to the bottom before animated following runs.
            .defaultScrollAnchor(.top, for: .sizeChanges)
            .defaultScrollAnchor(readingPositions.values[identity]?.atLatest == false ? .top : .bottom, for: .initialOffset)
            .scrollDismissesKeyboard(.interactively)
            .scrollBounceBehavior(.always, axes: .vertical)
            .coordinateSpace(name: "conversation-viewport")
            .onPreferenceChange(ConversationRowFrames.self) { frames in
                rowGeometry.frames = frames
                let users = userMessages
                let targets = UserNavigationTargets(
                    previous: users.last { (frames[$0.id]?.minY ?? .infinity) < -1 }?.id,
                    next: users.first { (frames[$0.id]?.minY ?? -.infinity) > 1 }?.id)
                if userNavigationTargets != targets { userNavigationTargets = targets }
                if !navigationActive, !isInteractingTranscript, let target = pendingReadingRestore, let id = target.rowID, let parent = frames[id] {
                    let frame = target.childID.flatMap { frames[$0] } ?? parent
                    if abs(frame.minY - target.offsetY) < 1 {
                        // Retain the semantic position through keyboard/viewport
                        // changes. The next touch or explicit jump releases it.
                        historyReady = true
                    } else {
                        let available = viewport.size.height - parent.height
                        if abs(available) > 0.5 {
                            // The timeline can grow while loading older steps.
                            // Preserve the step's screen position within its parent.
                            let origin = parent.minY + target.offsetY - frame.minY
                            scroll.scrollTo(id, anchor: UnitPoint(x: 0, y: origin / available))
                        }
                    }
                }
                saveReadingPosition(in: viewport)
                updateHistoryPosition(in: viewport)
                // Continue following the reader during a slow history request,
                // until the insertion changes the coordinate space.
                if historyRequestInFlight, model.historyMutationRevision == historyRequestRevision {
                    rememberHistoryPosition(in: viewport)
                }
            }
            .onScrollGeometryChange(for: ConversationContentPosition.self) { geometry in
                ConversationContentPosition(
                    nearTop: geometry.contentOffset.y + geometry.contentInsets.top <= 240,
                    approachingTop: geometry.contentOffset.y + geometry.contentInsets.top <= max(800, geometry.containerSize.height * 2),
                    atLatest: geometry.contentSize.height - geometry.contentOffset.y
                        - geometry.containerSize.height <= verticalPadding + 1,
                    isMeasured: geometry.containerSize.height > 0)
            } action: { _, position in
                historyContent = position
                updateHistoryPosition(in: viewport)
                saveReadingPosition(in: viewport)
            }
            .onScrollPhaseChange { previous, phase in
                if phase == .tracking { scrollsTowardLatest = false; selectedUserMessage = nil; pendingUserDirection = nil }
                isInteractingTranscript = phase == .interacting
                isScrollGestureActive = phase == .tracking || phase == .interacting || phase == .decelerating
                // Horizontal drawer gestures can enter a scroll phase without
                // moving the transcript. Only vertical input suspends following.
                if phase == .idle, previous == .interacting || previous == .decelerating {
                    if !navigationActive, pendingReadingRestore == nil, historyContent.atLatest, !model.needsLatestHistory {
                        followsLatest = true
                    }
                }
                if phase == .idle, previous == .tracking || previous == .interacting || previous == .decelerating {
                    // A stationary touch may defer the final arrival without
                    // changing reading intent. Catch up when the finger lifts.
                    followLatest(using: scroll)
                }
            }
            .onScrollGeometryChange(for: CGFloat.self) { $0.contentSize.height } action: { _, _ in
                // Rendered height also changes within a streaming row, without
                // adding a new row ID. Follow after that layout has arrived.
                followLatest(using: scroll)
            }
            .onChange(of: revision.projectionRevision) { _, _ in
                continueUserNavigation(using: scroll)
                restoreHistoryPosition(using: scroll)
                // Content-height observation follows after layout; issuing a second
                // scroll here would retarget against the previous geometry.
                updateHistoryPosition(in: viewport)
            }
            .onChange(of: navigationActive) { _, active in
                if !active { followLatest(using: scroll) }
                guard active, !followsLatest, pendingReadingRestore == nil else { return }
                // Capture before keyboard dismissal / drawer animation can resize
                // the viewport. Keep the same row and point offset on close too.
                if let first = rowGeometry.frames.filter({ revision.itemsByID[$0.key] != nil && $0.value.maxY > 0 && $0.value.minY < viewport.size.height })
                    .min(by: { $0.value.minY < $1.value.minY }) {
                    pendingReadingRestore = .init(atLatest: false, rowID: first.key, offsetY: first.value.minY)
                }
            }
            .onChange(of: hasInitialPosition) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: revision.error) { _, error in
                if error != nil { pendingUserDirection = nil }
            }
            .onChange(of: model.hasOlder) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: model.threadLoading) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: model.loadingOlder) { _, loading in
                if !loading {
                    continueUserNavigation(using: scroll)
                    restoreHistoryPosition(using: scroll)
                }
                updateHistoryPosition(in: viewport)
            }
            .onChange(of: model.hasNewer) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: model.loadingNewer) { _, loading in
                if !loading {
                    continueUserNavigation(using: scroll)
                    restoreHistoryPosition(using: scroll)
                }
                updateHistoryPosition(in: viewport)
            }
            .onScrollGeometryChange(for: CGFloat.self) { $0.contentOffset.y } action: { previous, offset in
                // Deceleration, bounce-back and restoration change offsets too.
                // Only direct interaction establishes a new paging direction.
                // A prefetched page can arrive while the finger is still down.
                // Its inserted height is not a reversal of the reader's swipe.
                guard isInteractingTranscript, !navigationActive, abs(previous - offset) > 0.5, !historyRequestInFlight else { return }
                followsLatest = false
                pendingReadingRestore = nil
                scrollsTowardLatest = offset > previous
                historyDirection = scrollsTowardLatest ? .newer : .older
                if hasInitialPosition { historyReady = true }
                updateHistoryPosition(in: viewport)
            }
            .background(Ink.background)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(revision.title)
            .accessibilityIdentifier("conversation")
            // Keep controls as siblings of the native scroll accessibility node.
            // An overlay can replace that node after accessibilityHidden changes
            // during drawer navigation, expanding the button to the whole viewport.
            VStack {
                Spacer(minLength: 0)
                if historyContent.isMeasured, !model.threadLoading, model.needsLatestHistory || (!followsLatest && !historyContent.atLatest) {
                    Button {
                        selectedUserMessage = nil
                        pendingUserDirection = nil
                        historyDirection = nil
                        historyRestore = nil
                        pendingReadingRestore = nil
                        // Record the intent before fetching/projecting the live
                        // tail; every later publication continues following it.
                        followsLatest = true
                        Task {
                            if model.needsLatestHistory { await model.loadNewer(latest: true) }
                            guard model.focusedConversationIdentity == identity, !model.needsLatestHistory else { return }
                            followLatest(using: scroll)
                        }
                    } label: {
                        Label("Latest messages", systemImage: "arrow.down")
                            .labelStyle(.iconOnly)
                            .frame(width: 42, height: 42)
                            .background(.regularMaterial, in: Circle())
                            .overlay(Circle().strokeBorder(Ink.border, lineWidth: 0.5))
                            .contentShape(Circle())
                    }
                    .buttonStyle(.plain)
                    .frame(width: 42, height: 42)
                    .padding(.bottom, 8)
                    .disabled(model.loadingNewer || model.loadingOlder)
                    .accessibilityLabel("Latest messages")
                    .accessibilityHint("Scroll to the latest message and follow new responses")
                    .accessibilityIdentifier("latest-messages")
                }
            }
            .accessibilityElement(children: .contain)
            if revision.loading {
                    ProgressView()
                        .accessibilityLabel("Loading conversation")
                        .accessibilityIdentifier("conversation-loading")
                        .allowsHitTesting(false)
            }
            if model.loadingOlder {
                ProgressView().font(.caption)
                    .padding(10).background(Ink.background, in: Capsule())
                    .allowsHitTesting(false)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel("Loading earlier messages")
                    .accessibilityIdentifier("loading-older")
            }
            }
            }
            threadControls(using: scroll)
            }
            .onChange(of: revision.rows.first?.id, initial: true) { _, _ in
                if !hasInitialPosition, !revision.rows.isEmpty {
                    // Position a newly opened conversation once. Later viewport
                    // changes, including the keyboard, retain its top edge.
                    if let saved = readingPositions.values[identity], !saved.atLatest,
                       let id = saved.rowID,
                       revision.items.contains(where: { $0.id == id }) {
                        followsLatest = false
                        pendingReadingRestore = saved
                        scroll.scrollTo(id, anchor: .top)
                    } else {
                        scroll.scrollTo("latest", anchor: .bottom)
                    }
                    hasInitialPosition = true
                    return
                }
            }
            }
        .foregroundStyle(Ink.text)
    }
}

private struct InboxGeneratedOutputView: View, Equatable {
    private let results: [[String]]
    @State private var outputs: [ChatGeneratedOutput] = []

    init(rows: [TranscriptRow]) {
        results = rows.compactMap { $0.tool?.isInspectionOutput == true ? nil : $0.tool?.generatedResults }
    }
    static func == (lhs: Self, rhs: Self) -> Bool { lhs.results == rhs.results }
    var body: some View {
        Group { if !outputs.isEmpty { ChatGeneratedOutputs(outputs: outputs) } }
            .task(id: results) {
                let captured = results
                let parsed = await Task.detached(priority: .utility) {
                    // This also applies to replayed rows that used to opt exec
                    // output into chat. Tool text belongs inside its disclosure.
                    var seen = Set<String>()
                    return captured.flatMap { ChatGeneratedOutput.parse(results: $0) }
                        .filter { seen.insert($0.id).inserted }
                }.value
                guard !Task.isCancelled else { return }
                outputs = parsed
            }
    }
}

// Geometry is reading-position bookkeeping, not rendered state. Updating each
// pixel must not invalidate the conversation's SwiftUI body.
private final class ConversationRowGeometry {
    var frames: [String: CGRect] = [:]
    var historyRestore: (id: String, offsetY: CGFloat, childID: String?)?
}

private struct ConversationRowFrames: PreferenceKey {
    static var defaultValue: [String: CGRect] { [:] }
    static func reduce(value: inout [String: CGRect], nextValue: () -> [String: CGRect]) {
        value.merge(nextValue()) { _, new in new }
    }
}

private struct ConversationContentPosition: Equatable {
    var nearTop = false
    var approachingTop = false
    var atLatest = false
    var isMeasured = false
}


private struct ConversationCodeModeBatch: View {
    let item: ConversationItem
    let tools: ConversationToolExpansion
    @Binding var expanded: Bool
    @Binding var showsJavaScript: Bool
    var onToggle: () -> Void
    @State private var sourceSheet: ToolSourceDocument?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        if let parent = item.activity.first {
            VStack(alignment: .leading, spacing: 10) {
                Button {
                    onToggle()
                    withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.18)) { expanded.toggle() }
                } label: {
                    HStack(spacing: 8) {
                        Image(systemName: "curlybraces").foregroundStyle(Color.accentColor)
                        Text("Code Mode").font(.subheadline.weight(.medium))
                        Text(item.activity.count == 2 ? "1 tool" : "\(item.activity.count - 1) tools").font(.caption).foregroundStyle(Ink.muted)
                        Spacer(minLength: 4)
                        if item.isRunning {
                            ProgressView().controlSize(.mini).accessibilityLabel("Running")
                        } else if parent.running || parent.tool?.status == "Running" {
                            Text("Interrupted").font(.caption2).foregroundStyle(Ink.muted)
                        } else if let status = parent.tool?.status, status != "Completed" {
                            Text(status).font(.caption2)
                                .foregroundStyle(status == "Failed" ? Color.orange : Ink.muted)
                        } else {
                            Image(systemName: "checkmark").font(.caption2.weight(.semibold))
                                .foregroundStyle(Ink.muted).accessibilityLabel("Completed")
                        }
                        Image(systemName: expanded ? "chevron.up" : "chevron.down")
                            .font(.caption2.weight(.semibold)).foregroundStyle(Ink.muted)
                    }.frame(minHeight: 44).contentShape(Rectangle())
                }.buttonStyle(.plain)
                    .accessibilityIdentifier("code-mode-batch-" + parent.id)
                    .accessibilityValue(expanded ? "Expanded" : "Collapsed")
                if expanded {
                    ForEach(Array(item.activity.dropFirst())) { row in
                        ConversationToolCard(row: row, live: item.isRunning && row.running, expanded: tools.binding(row.id), onToggle: onToggle)
                    }
                    VStack(alignment: .leading, spacing: 8) {
                        Button {
                            onToggle()
                            showsJavaScript.toggle()
                        } label: {
                            HStack {
                                Text("JavaScript and batch output")
                                Spacer()
                                Image(systemName: showsJavaScript ? "chevron.up" : "chevron.down")
                            }.font(.caption).foregroundStyle(Ink.muted)
                                .frame(minHeight: 44).contentShape(Rectangle())
                        }.buttonStyle(.plain)
                            .accessibilityIdentifier("code-mode-javascript-" + parent.id)
                            .accessibilityValue(showsJavaScript ? "Expanded" : "Collapsed")
                        if showsJavaScript {
                            if let source = parent.tool?.input.first(where: { $0.label == "Code" })?.value {
                                HStack {
                                    Spacer()
                                    Button("Copy code", systemImage: "doc.on.doc") { UIPasteboard.general.string = source }
                                        .buttonStyle(.plain).font(.caption).frame(minHeight: 44)
                                        .accessibilityIdentifier("code-mode-copy-" + parent.id)
                                }
                                if ChatCodePreview(source, maximumCharacters: 16_384, maximumLines: 120).isTruncated {
                                    Button("View full code") { sourceSheet = .init(title: "Code", source: source) }
                                        .frame(minHeight: 44)
                                        .accessibilityIdentifier("code-mode-full-source-" + parent.id)
                                } else {
                                    ScrollView(.horizontal) {
                                        ChatCodeText(source: source, language: "javascript")
                                            .font(.system(.footnote, design: .monospaced))
                                            .textSelection(.enabled).fixedSize(horizontal: true, vertical: true)
                                            .accessibilityIdentifier("code-mode-source-" + parent.id)
                                    }
                                }
                            }
                            ToolActivityView(row: parent, hidesCode: true).padding(.vertical, 12)
                                .accessibilityIdentifier("tool-detail-" + parent.id)
                        }
                    }
                }
            }.padding(12)
                .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Ink.border, lineWidth: 0.5))
                .accessibilityElement(children: .contain)
                .sheet(item: $sourceSheet) { document in ToolSourceSheet(document: document) }
        }
    }
}

private struct ConversationToolCard: View {
    let row: TranscriptRow
    let live: Bool
    @Binding var expanded: Bool
    var onToggle: () -> Void
    @State private var sourceSheet: ToolSourceDocument?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    private var failed: Bool { row.tool?.status == "Failed" }
    private var title: String { row.tool?.title ?? row.text }
    private var subject: String { row.tool?.subject ?? "" }
    private var command: String? {
        guard ["Run command", "Start process"].contains(title) else { return nil }
        return row.tool?.input.first(where: { $0.label == "Command" })?.value
    }
    private var codeModeSource: String? {
        guard title == "Run code" else { return nil }
        return row.tool?.input.first(where: { $0.label == "Code" })?.value
    }
    private var hasSource: Bool { command != nil || codeModeSource != nil }
    private var directory: String? { row.tool?.input.first(where: { $0.label == "Folder" })?.value }
    private var shell: String? { row.tool?.input.first(where: { $0.label == "Shell" })?.value }
    private var exitCode: String? { row.tool?.output.first(where: { $0.label == "Exit code" })?.value }
    private var symbol: String {
        let family = title.lowercased()
        if family.contains("vault") { return "lock.shield" }
        if family.contains("agent") || family.contains("delegate") { return "person.2" }
        if family.contains("search") { return "magnifyingglass" }
        if family.contains("browser") || family.contains("page") || family.contains("web") { return "globe" }
        if family.contains("image") || family.contains("capture") { return "photo" }
        if family.contains("file") || family.contains("patch") { return "doc.text" }
        if family.contains("computer") || family.contains("machine") { return "desktopcomputer" }
        if family.contains("account") || family.contains("connect") { return "person.crop.circle" }
        if family.contains("command") || family.contains("code") || family.contains("process") { return "terminal" }
        return "gearshape"
    }
    private var status: String {
        if live { return "Running" }
        if row.running || row.tool?.status == "Running" { return "Interrupted" }
        return row.tool?.status ?? "Completed"
    }
    @ViewBuilder private var statusIndicator: some View {
        if live { ProgressView().controlSize(.mini).accessibilityLabel("Running") }
        else if status != "Completed" {
            Text(exitCode.map { "\(status) · exit \($0)" } ?? status)
                .font(.caption2.weight(.medium)).foregroundStyle(failed ? Color.orange : Ink.muted)
                .fixedSize(horizontal: false, vertical: true)
        } else {
            Image(systemName: "checkmark").font(.caption2.weight(.semibold))
                .foregroundStyle(Ink.muted).accessibilityLabel("Completed")
        }
    }
    private var disclosure: some View {
        Image(systemName: expanded ? "chevron.up" : "chevron.down")
            .font(.caption2.weight(.semibold)).foregroundStyle(Ink.muted).accessibilityHidden(true)
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                onToggle()
                withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.18)) { expanded.toggle() }
            } label: {
                VStack(alignment: .leading, spacing: hasSource ? 10 : 0) {
                    if let command {
                        HStack(spacing: 6) {
                            Image(systemName: "folder").foregroundStyle(Color.accentColor)
                            Text(directory ?? "Default directory")
                                .font(.caption.monospaced()).foregroundStyle(Ink.muted)
                                .fixedSize(horizontal: false, vertical: true)
                                .accessibilityIdentifier("command-directory-" + row.id)
                            Spacer(minLength: 4)
                            statusIndicator
                            disclosure
                        }
                        let preview = ChatCodePreview(command)
                        ChatCodeText(source: preview.text, language: "bash")
                            .font(.system(.footnote, design: .monospaced))
                            .foregroundStyle(Ink.text)
                            .multilineTextAlignment(.leading)
                            .fixedSize(horizontal: false, vertical: true)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .lineLimit(preview.isTruncated ? 3 : nil)
                            .accessibilityIdentifier("command-source-" + row.id)
                        if preview.isTruncated {
                            Text("Show command and results").font(.caption2).foregroundStyle(Ink.muted)
                        }
                        if let shell {
                            Text(shell).font(.caption2.monospaced()).foregroundStyle(Ink.muted)
                        }
                    } else if let source = codeModeSource {
                        HStack(spacing: 6) {
                            Image(systemName: "curlybraces").foregroundStyle(Color.accentColor)
                            Text("Code Mode").font(.caption.weight(.medium)).foregroundStyle(Ink.muted)
                            Text("JavaScript").font(.caption2.monospaced()).foregroundStyle(Ink.muted)
                            Spacer(minLength: 4)
                            statusIndicator
                            disclosure
                        }
                        if !expanded {
                            // The disclosure preview must not highlight an entire
                            // program that is clipped to three visible lines.
                            ChatCodeText(source: ChatCodePreview(source).text, language: "javascript")
                                .font(.system(.caption, design: .monospaced))
                                .foregroundStyle(Ink.text)
                                .multilineTextAlignment(.leading)
                                .lineLimit(3)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .accessibilityIdentifier("code-mode-preview-" + row.id)
                            Text("Show code and results")
                                .font(.caption2).foregroundStyle(Ink.muted)
                        }
                    } else {
                        HStack(spacing: 8) {
                            Image(systemName: symbol).foregroundStyle(Color.accentColor)
                                .frame(width: 18).accessibilityHidden(true)
                            VStack(alignment: .leading, spacing: 3) {
                                Text(subject.isEmpty ? title : subject)
                                    .font(.subheadline).foregroundStyle(Ink.text)
                                    .lineLimit(3).multilineTextAlignment(.leading)
                                if !subject.isEmpty { Text(title).font(.caption2).foregroundStyle(Ink.muted) }
                            }
                            Spacer(minLength: 0)
                            statusIndicator
                            disclosure
                        }
                    }
                }.padding(.vertical, hasSource ? 12 : 6)
                    .frame(minHeight: 44).contentShape(Rectangle())
            }.buttonStyle(.plain)
                .accessibilityIdentifier("tool-disclosure-" + row.id)
                .accessibilityValue(expanded ? "Expanded" : "Collapsed")
                .accessibilityHint(expanded ? "Hide input and results" : "Show input and results")
                .contextMenu {
                    if let command {
                        Button("Copy command", systemImage: "doc.on.doc") { UIPasteboard.general.string = command }
                    }
                    if let source = codeModeSource {
                        Button("Copy code", systemImage: "doc.on.doc") { UIPasteboard.general.string = source }
                    }
                    if let directory {
                        Button("Copy directory", systemImage: "folder") { UIPasteboard.general.string = directory }
                    }
                }
            if expanded {
                if let command, ChatCodePreview(command).isTruncated {
                    Button("View full command") { sourceSheet = .init(title: "Command", source: command) }
                        .frame(minHeight: 44)
                        .accessibilityIdentifier("command-full-source-" + row.id)
                }
                if let source = codeModeSource {
                    Divider()
                    HStack {
                        Text("JavaScript").font(.caption2.monospaced()).foregroundStyle(Ink.muted)
                        Spacer()
                        Button {
                            UIPasteboard.general.string = source
                        } label: {
                            Label("Copy code", systemImage: "doc.on.doc")
                                .font(.caption)
                        }
                        .buttonStyle(.plain)
                        .frame(minHeight: 44)
                        .accessibilityIdentifier("code-mode-copy-" + row.id)
                    }
                    if ChatCodePreview(source, maximumCharacters: 16_384, maximumLines: 120).isTruncated {
                        Button("View full code") { sourceSheet = .init(title: "Code", source: source) }
                            .frame(minHeight: 44)
                            .accessibilityIdentifier("code-mode-full-source-" + row.id)
                    } else {
                        ScrollView(.horizontal) {
                            ChatCodeText(source: source, language: "javascript")
                                .font(.system(.footnote, design: .monospaced))
                                .foregroundStyle(Ink.text)
                                .lineSpacing(4)
                                .textSelection(.enabled)
                                .fixedSize(horizontal: true, vertical: true)
                                .padding(.bottom, 12)
                                .accessibilityIdentifier("code-mode-source-" + row.id)
                        }
                        .accessibilityIdentifier("code-mode-scroll-" + row.id)
                    }
                }
                Divider()
                ToolActivityView(row: row, hidesCommand: command != nil, hidesCode: codeModeSource != nil).padding(.vertical, 12)
                    .accessibilityIdentifier("tool-detail-" + row.id)
            }
        }.padding(.horizontal, 12)
            .sheet(item: $sourceSheet) { document in ToolSourceSheet(document: document) }
            .background(Ink.surface, in: RoundedRectangle(cornerRadius: 12))
            .accessibilityElement(children: .contain)
    }
}

private struct ToolSourceDocument: Identifiable {
    let id = UUID()
    let title: String
    let source: String
}

/// A viewport-sized native text view owns scrolling for large source payloads.
/// Never ask the transcript to measure the full document's intrinsic height.
private struct ToolSourceSheet: View {
    let document: ToolSourceDocument
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            ToolSourceTextView(source: document.source)
                .navigationTitle(document.title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .topBarLeading) {
                        Button("Copy source") { UIPasteboard.general.string = document.source }
                            .accessibilityIdentifier("tool-source-copy")
                    }
                    ToolbarItem(placement: .topBarTrailing) {
                        Button("Done") { dismiss() }.accessibilityIdentifier("tool-source-done")
                    }
                }
        }
    }
}

private struct ToolSourceTextView: UIViewRepresentable {
    let source: String
    func makeUIView(context: Context) -> UITextView {
        let view = UITextView(usingTextLayoutManager: true)
        view.isEditable = false
        view.isSelectable = true
        view.isScrollEnabled = true
        view.alwaysBounceVertical = true
        view.font = UIFontMetrics(forTextStyle: .footnote).scaledFont(for: .monospacedSystemFont(ofSize: 13, weight: .regular))
        view.adjustsFontForContentSizeCategory = true
        view.textColor = .label
        view.backgroundColor = .systemBackground
        view.textContainerInset = UIEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        view.accessibilityIdentifier = "tool-source-text"
        view.text = source
        return view
    }
    func updateUIView(_ view: UITextView, context: Context) {
        if view.text != source { view.text = source }
    }
}

private struct ToolActivityView: View {
    let row: TranscriptRow
    var hidesCommand = false
    var hidesCode = false
    private var tool: ToolPresentation {
        if let tool = row.tool { return tool }
        var fallback = ToolPresentation(name: row.text, arguments: .null)
        if !row.running { fallback.finish(.string(row.detail)) }
        return fallback
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            let input = tool.input.filter {
                (!hidesCommand || !["Command", "Folder", "Shell"].contains($0.label)) && (!hidesCode || $0.label != "Code")
            }
            if !input.isEmpty { fields(input, heading: "Input") }
            if !tool.output.isEmpty { fields(tool.output, heading: "Result") }
        }.foregroundStyle(Ink.muted).accessibilityIdentifier("tool-activity")
    }
    private func fields(_ values: [ToolField], heading: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(heading).font(.caption.weight(.semibold)).foregroundStyle(Ink.muted)
                .accessibilityAddTraits(.isHeader)
            ForEach(Array(values.enumerated()), id: \.offset) { _, field in
                VStack(alignment: .leading, spacing: 3) {
                    if field.label != heading { Text(field.label).font(.caption).foregroundStyle(Ink.muted) }
                    Text(field.value).font(field.code ? .system(.footnote, design: .monospaced) : .subheadline)
                        .foregroundStyle(Ink.text).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                }
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct VaultIntakeCard: View {
    @ObservedObject var model: InboxModel
    let intake: VaultIntake
    @State private var showingForm = false
    @State private var receiptAgentID = ""
    @State private var receipt: VaultIntakeReceipt?
    @State private var verificationSubmitted = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(intake.operation == "browser_takeover" ? "Control browser privately" : intake.operation == "browser_verification" ? (verificationSubmitted ? "Code submitted" : "Verify browser login") : (receipt == nil ? "Add to Vault securely" : "Saved to Vault"), systemImage: "lock.shield")
                .font(.headline)
            if verificationSubmitted { Text("Browser verification is pending.") } else if let receipt {
                Text(receipt.name).font(.subheadline)
            } else {
                if !intake.name.isEmpty { Text(intake.name).font(.subheadline) }
                if let origin = intake.origin { Text(origin).font(.caption).textSelection(.enabled) }
                Text(intake.operation == "browser_takeover" ? "Control the browser privately. The screen and input stay out of chat." : intake.operation == "browser_verification" ? "The code goes directly to this browser session. It stays out of chat and is not saved to Vault." : "Your information goes directly to your encrypted Vault. It stays out of chat.")
                    .font(.subheadline).foregroundStyle(.secondary)
                Button("Open secure form") { receiptAgentID = model.focused?.id ?? ""; showingForm = true }
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("vault-intake-open")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading).padding(16)
        .background(Ink.surface, in: RoundedRectangle(cornerRadius: 16))
        .accessibilityIdentifier("vault-intake-card")
        .task(id: intake.challengeID) {
            if model.claimBrowserRequestPresentation(intake) {
                receiptAgentID = model.focused?.id ?? ""
                showingForm = true
            }
        }
        .fullScreenCover(isPresented: Binding(get: { showingForm && intake.operation == "browser_takeover" }, set: { showingForm = $0 })) {
            BrowserTakeoverSheet(model: model, intake: intake)
        }
        .sheet(isPresented: Binding(get: { showingForm && intake.operation != "browser_takeover" }, set: { showingForm = $0 })) {
            if intake.operation == "browser_verification" {
                BrowserVerificationSheet(model: model, intake: intake, agentID: receiptAgentID) { verificationSubmitted = true }
            } else { VaultLoginSheet(model: model, intake: intake, agentID: receiptAgentID) { receipt = $0 } }
        }
    }
}

private struct VaultLoginSheet: View {
    @ObservedObject var model: InboxModel
    let intake: VaultIntake
    let agentID: String
    let saved: (VaultIntakeReceipt) -> Void
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var name = ""
    @State private var values: [String: String] = [:]
    @State private var account = UUID()
    @State private var submission: Task<Void, Never>?
    @State private var saving = false
    @State private var attempted = false
    @State private var verified = false
    private var authorizing: Bool { intake.operation == "authorize_origin" }
    @State private var failure: String?

    private var fields: [(key: String, label: String, secure: Bool, max: Int)] {
        switch intake.kind {
        case "api_key": return [("api_key", "API key", true, 8192)]
        case "card": return [("card_number", "Card number", true, 32), ("expiry_month", "Expiry month", false, 2), ("expiry_year", "Expiry year", false, 4), ("cvv", "Security code", true, 4), ("billing_zip", "Billing postal code", false, 32)]
        case "address": return [("address_line_1", "Address", false, 256), ("address_line_2", "Address line 2 (optional)", false, 256), ("city", "City", false, 120), ("state", "State", false, 120), ("zip", "Postal code", false, 32), ("country", "Country", false, 120)]
        case "phone": return [("phone_number", "Phone number", false, 64)]
        default: return [("username", "Username", false, 512), ("password", "Password", true, 8192)]
        }
    }
    private var valid: Bool {
        (authorizing ? verified : !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && name.utf8.count <= 120
            && fields.allSatisfy { field in
                let value = values[field.key] ?? ""
                return (field.key == "address_line_2" || !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty) && value.utf8.count <= field.max
            })
    }
    private func clear() { values.removeAll(); name = "" }
    var body: some View {
        NavigationStack {
            Form {
                Section {
                    if authorizing { Text(name.isEmpty ? "Verifying login…" : name) }
                    else {
                    TextField("Name", text: $name).accessibilityIdentifier("vault-intake-name")
                    ForEach(fields, id: \.key) { field in
                        let binding = Binding<String>(get: { values[field.key] ?? "" }, set: { values[field.key] = $0 })
                        Group {
                            if field.secure { SecureField(field.label, text: binding) }
                            else { TextField(field.label, text: binding) }
                        }
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                        .privacySensitive().accessibilityIdentifier("vault-intake-" + field.key)
                    }
                    }
                } footer: {
                    Text("Credentials are sent directly to your encrypted Vault, never as a chat message.")
                }
                if let origin = intake.origin {
                    Section("Website access") { Text(origin).font(.subheadline); Text("Saving allows browser login with this item on this exact website.") }
                }
                if let failure { Section { Text(failure).foregroundStyle(.red) } }
                Section {
                    Button {
                        saving = true; attempted = true
                        submission = Task { @MainActor in
                            defer { clear(); saving = false }
                            do {
                                var payload = values.filter { !$0.value.isEmpty }
                                payload["name"] = name.trimmingCharacters(in: .whitespacesAndNewlines)
                                if let origin = intake.origin { payload["browser_origin"] = origin }
                                let receipt: VaultIntakeReceipt
                                if authorizing, let id = intake.vaultID, let origin = intake.origin {
                                    receipt = try await model.authorizeVaultOrigin(id: id, origin: origin, name: name, account: account)
                                } else {
                                    receipt = try await model.saveVaultItem(kind: intake.kind, values: payload, account: account)
                                }
                                guard !Task.isCancelled, model.vaultIntakeAccount == account else { return }
                                model.publishVaultReceipt(receipt, intake: intake, agentID: agentID, account: account)
                                saved(receipt)
                                dismiss()
                            } catch {
                                // No error body, request, or secret is included in UI/logs/transcripts.
                                failure = "Couldn’t confirm the save. Check your Vault before trying again."
                            }
                        }
                    } label: {
                        HStack { Text(saving ? "Saving…" : authorizing ? "Allow this website" : "Save to Vault"); if saving { ProgressView() } }
                    }
                    .disabled(!valid || saving || attempted)
                    .accessibilityIdentifier("vault-intake-save")
                }
            }
            .disabled(saving)
            .navigationTitle("Add to Vault")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { submission?.cancel(); clear(); dismiss() }
            } }
            .overlay {
                if scenePhase != .active { Color(uiColor: .systemBackground).ignoresSafeArea() }
            }
        }
        .interactiveDismissDisabled(saving)
        .task {
            account = model.vaultIntakeAccount
            if authorizing, let id = intake.vaultID {
                do {
                    let item = try await model.vaultLoginMetadata(id: id, account: account)
                    guard !Task.isCancelled else { return }
                    name = item.name; verified = true
                } catch { failure = "Couldn’t verify this login. Check your Vault." }
            } else { name = intake.name }
        }
        .onDisappear { submission?.cancel(); clear() }
        .onChange(of: model.vaultIntakeAccount) { _, _ in submission?.cancel(); clear(); dismiss() }
        .onChange(of: model.connected) { _, connected in if !connected { submission?.cancel(); clear(); dismiss() } }
    }
}

private struct BrowserVerificationSheet: View {
    @ObservedObject var model: InboxModel
    let intake: VaultIntake
    let agentID: String
    let submitted: () -> Void
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var code = ""
    @State private var account = UUID()
    @State private var attempted = false
    @State private var busy = false
    @State private var failure: String?
    @State private var submission: Task<Void, Never>?
    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text(intake.origin ?? "")
                    SecureField("Verification code", text: $code).textContentType(.oneTimeCode).keyboardType(.numberPad)
                        .textInputAutocapitalization(.never).autocorrectionDisabled().privacySensitive()
                } footer: { Text("The code goes directly to this browser session, outside chat. It is not saved to Vault.") }
                if let failure { Text(failure) }
                Button(busy ? "Submitting…" : "Submit code") {
                    attempted = true; busy = true
                    let value = code; code = ""
                    submission = Task { @MainActor in
                        defer { busy = false }
                        do {
                            try await model.submitBrowserVerification(intake: intake, code: value, account: account)
                            guard !Task.isCancelled, model.vaultIntakeAccount == account else { return }
                            model.publishBrowserVerificationReceipt(intake: intake, agentID: agentID, account: account)
                            submitted(); dismiss()
                        } catch { failure = "Couldn’t confirm submission. Request a new secure form before trying again." }
                    }
                }.disabled(attempted || code.range(of: #"^[0-9]{4,10}$"#, options: .regularExpression) == nil)
            }
            .navigationTitle("Verify browser login")
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { submission?.cancel(); code = ""; dismiss() } } }
            .overlay { if scenePhase != .active { Color(uiColor: .systemBackground).ignoresSafeArea() } }
        }
        .interactiveDismissDisabled(busy)
        .task { account = model.vaultIntakeAccount }
        .onDisappear { submission?.cancel(); code = "" }
        .onChange(of: scenePhase) { _, phase in if phase != .active { code = "" } }
        .onChange(of: model.vaultIntakeAccount) { _, _ in submission?.cancel(); code = ""; dismiss() }
    }
}

private struct BrowserTakeoverSheet: View {
    @ObservedObject var model: InboxModel
    let intake: VaultIntake
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var account = UUID()
    @State private var screen: UIImage?
    @State private var keyboard: BrowserKeyboardHint?
    @State private var inputs: [BrowserInputRegion] = []
    @State private var keyboardVisible = false
    @State private var failure: String?
    @State private var queue: [[String: JSON]] = []
    @State private var submission: Task<Void, Never>?
    @State private var observing: Task<Void, Never>?
    @State private var generation = UUID()
    @State private var viewport = CGSize(width: 390, height: 700)
    @State private var finishing = false
    @State private var touching = false

    private func clear() {
        generation = UUID(); submission?.cancel(); submission = nil
        queue.removeAll(); screen = nil; keyboard = nil; inputs = []; keyboardVisible = false
        finishing = false; touching = false
    }
    private func observe(configureViewport: Bool = false) {
        // Poll pixels without resizing the remote page as the native keyboard opens.
        var action: [String: JSON] = ["action": .string("observe")]
        if configureViewport {
            action["viewport"] = .object([
                "width": .number(Double(min(1920, max(240, viewport.width)).rounded())),
                "height": .number(Double(min(1920, max(240, viewport.height)).rounded())), "mobile": .bool(true)])
        }
        enqueue(action)
    }
    private func enqueue(_ action: [String: JSON]) {
        guard scenePhase == .active, account == model.vaultIntakeAccount, !finishing else { return }
        guard failure == nil || action["action"] == .string("finish") else { return }
        if action["action"] == .string("touch") {
            touching = action["phase"] == .string("start") || action["phase"] == .string("move")
        }
        if action["action"] == .string("finish") { finishing = true; keyboardVisible = false }
        // Only replace adjacent unsent moves. Text, keys and gesture boundaries retain order.
        if action["phase"] == .string("move"), queue.last?["phase"] == .string("move") {
            queue[queue.count - 1] = action
        } else { queue.append(action) }
        drain()
    }
    private func drain() {
        guard submission == nil, !queue.isEmpty else { return }
        let action = queue.removeFirst(), token = generation
        submission = Task { @MainActor in
            do {
                let frame = try await model.browserTakeover(intake: intake, action: action, account: account)
                guard !Task.isCancelled, generation == token, scenePhase == .active,
                      account == model.vaultIntakeAccount else { return }
                switch frame {
                case .finished:
                    guard action["action"] == .string("finish") else { throw APIError.invalidResponse }
                    model.publishBrowserVerificationReceipt(intake: intake, agentID: intake.agentID ?? "", account: account)
                    clear(); dismiss(); return
                case .active(let data, _, _):
                    guard let image = UIImage(data: data) else { throw APIError.invalidResponse }
                    screen = image; keyboard = nil; inputs = []
                case .activeWithInput(let data, _, _, let hint, let regions):
                    guard let image = UIImage(data: data) else { throw APIError.invalidResponse }
                    screen = image; keyboard = hint; inputs = regions
                    if hint != nil { keyboardVisible = true }
                }
                submission = nil; drain()
            } catch {
                guard generation == token, !Task.isCancelled else { return }
                clear(); failure = "Couldn’t confirm the action. Refresh before continuing."
            }
        }
    }
    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                GeometryReader { geometry in
                    PrivateBrowserCanvas(image: screen, keyboard: keyboard, inputs: inputs,
                        keyboardVisible: keyboardVisible && failure == nil && !finishing && scenePhase == .active,
                        enabled: screen != nil && failure == nil && !finishing && scenePhase == .active,
                        send: enqueue, showKeyboard: { hint in keyboard = hint; keyboardVisible = true })
                        .onAppear { viewport = geometry.size }
                        .onChange(of: geometry.size) { _, size in if !keyboardVisible { viewport = size } }
                }
                if let failure { Text(failure).font(.footnote).foregroundStyle(.red).padding(8) }
            }
            .background(Color.black).privacySensitive()
            .overlay {
                if screen == nil && failure == nil {
                    ProgressView("Opening private browser…").tint(.white).foregroundStyle(.white)
                }
            }
            .navigationTitle(intake.origin ?? "Private browser")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { enqueue(["action": .string("finish")]) }
                        .disabled(finishing || scenePhase != .active)
                }
                ToolbarItemGroup(placement: .bottomBar) {
                    Button { guard submission == nil else { return }; failure = nil; observe(configureViewport: true) } label: {
                        Label("Refresh", systemImage: "arrow.clockwise")
                    }.disabled(submission != nil || finishing || touching)
                    Spacer()
                    Button { keyboardVisible.toggle() } label: { Label("Keyboard", systemImage: "keyboard") }
                        .disabled(screen == nil || failure != nil || finishing)
                }
            }
            .overlay { if scenePhase != .active { Color(uiColor: .systemBackground).ignoresSafeArea() } }
        }
        .presentationDetents([.large]).presentationDragIndicator(.hidden)
        .interactiveDismissDisabled()
        .task {
            account = model.vaultIntakeAccount; observe(configureViewport: true)
            observing = Task { @MainActor in
                while !Task.isCancelled {
                    try? await Task.sleep(for: .seconds(1))
                    guard !Task.isCancelled else { return }
                    if submission == nil && queue.isEmpty && failure == nil && !finishing && !touching { observe() }
                }
            }
        }
        .onDisappear { observing?.cancel(); clear() }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { clear(); if failure == nil { failure = "Private view paused. Refresh to continue." } }
        }
        .onChange(of: model.vaultIntakeAccount) { _, _ in clear(); dismiss() }
        .onChange(of: model.connected) { _, connected in if !connected { clear(); dismiss() } }
    }
}

private struct PrivateBrowserCanvas: UIViewRepresentable {
    let image: UIImage?
    let keyboard: BrowserKeyboardHint?
    let inputs: [BrowserInputRegion]
    let keyboardVisible: Bool
    let enabled: Bool
    let send: ([String: JSON]) -> Void
    let showKeyboard: (BrowserKeyboardHint) -> Void
    func makeUIView(context: Context) -> PrivateBrowserTouchView { PrivateBrowserTouchView() }
    func updateUIView(_ view: PrivateBrowserTouchView, context: Context) {
        view.send = send; view.showKeyboard = showKeyboard; view.regions = inputs
        view.imageView.image = image; view.acceptsInput = enabled; view.setNeedsLayout()
        view.bridge.send = send
        view.bridge.configure(type: keyboard?.type ?? "password", multiline: keyboard?.multiline ?? false)
        if keyboardVisible && enabled {
            if !view.bridge.isFirstResponder { view.bridge.becomeFirstResponder() }
        } else { view.bridge.resignFirstResponder() }
        if !enabled { view.resetTouch() }
    }
    static func dismantleUIView(_ view: PrivateBrowserTouchView, coordinator: ()) {
        view.bridge.resignFirstResponder(); view.bridge.send = { _ in }
        view.imageView.image = nil; view.resetTouch(); view.send = { _ in }
    }
}

@MainActor private final class PrivateBrowserKeyboard: UIView, UIKeyInput {
    var send: ([String: JSON]) -> Void = { _ in }
    var multiline = false
    var hasText: Bool { true }
    override var canBecomeFirstResponder: Bool { true }
    var keyboardType: UIKeyboardType = .default
    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var spellCheckingType: UITextSpellCheckingType = .no
    var smartQuotesType: UITextSmartQuotesType = .no
    var smartDashesType: UITextSmartDashesType = .no
    var smartInsertDeleteType: UITextSmartInsertDeleteType = .no
    var isSecureTextEntry = true
    var returnKeyType: UIReturnKeyType = .go
    func configure(type: String, multiline: Bool) {
        let next: UIKeyboardType = switch type {
        case "email": .emailAddress
        case "url": .URL
        case "tel": .phonePad
        case "number": .decimalPad
        default: .default
        }
        let secure = type == "password"
        let changed = keyboardType != next || self.multiline != multiline || isSecureTextEntry != secure
        isSecureTextEntry = secure
        keyboardType = next; self.multiline = multiline; returnKeyType = multiline ? .default : .go
        if changed && isFirstResponder { reloadInputViews() }
    }
    func insertText(_ text: String) {
        if text == "\n" && !multiline { send(["action": .string("key"), "key": .string("Enter")]); return }
        // Bound each edit by UTF-8 bytes, without keeping a local password buffer.
        var chunk = ""
        for scalar in text.unicodeScalars {
            let value = String(scalar)
            if chunk.utf8.count + value.utf8.count > 512 {
                edit(chunk); chunk = ""
            }
            chunk += value
        }
        if !chunk.isEmpty { edit(chunk) }
    }
    private func edit(_ value: String) {
        send(["action": .string("edit"), "delete_backward": .number(0), "text": .string(value)])
    }
    func deleteBackward() { send(["action": .string("edit"), "delete_backward": .number(1), "text": .string("")]) }
    override var keyCommands: [UIKeyCommand]? {
        [UIKeyCommand(input: "\t", modifierFlags: [], action: #selector(tab)),
         UIKeyCommand(input: UIKeyCommand.inputEscape, modifierFlags: [], action: #selector(escape))]
    }
    @objc private func tab() { send(["action": .string("key"), "key": .string("Tab")]) }
    @objc private func escape() { send(["action": .string("key"), "key": .string("Escape")]) }
}

@MainActor private final class PrivateBrowserTouchView: UIView {
    let imageView = UIImageView()
    let bridge = PrivateBrowserKeyboard()
    var send: ([String: JSON]) -> Void = { _ in }
    var showKeyboard: (BrowserKeyboardHint) -> Void = { _ in }
    var regions: [BrowserInputRegion] = []
    var acceptsInput = false
    private var tracked: UITouch?
    private var lastPoint = CGPoint.zero
    private var startPoint = CGPoint.zero
    private let trail = CAShapeLayer()
    private let ripple = CAShapeLayer()
    private var path = UIBezierPath()
    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black; isMultipleTouchEnabled = false
        imageView.contentMode = .scaleAspectFit; imageView.isUserInteractionEnabled = false
        addSubview(imageView); addSubview(bridge)
        trail.strokeColor = UIColor.systemBlue.withAlphaComponent(0.7).cgColor
        trail.fillColor = UIColor.clear.cgColor; trail.lineWidth = 3
        ripple.fillColor = UIColor.systemBlue.withAlphaComponent(0.3).cgColor
        layer.addSublayer(trail); layer.addSublayer(ripple)
        accessibilityLabel = "Private browser screen"
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
    override func layoutSubviews() { super.layoutSubviews(); imageView.frame = bounds; bridge.frame = .zero }
    private var imageRect: CGRect {
        guard let size = imageView.image?.size, size.width > 0, size.height > 0 else { return .zero }
        let scale = min(bounds.width / size.width, bounds.height / size.height)
        let fitted = CGSize(width: size.width * scale, height: size.height * scale)
        return CGRect(x: (bounds.width - fitted.width) / 2, y: (bounds.height - fitted.height) / 2, width: fitted.width, height: fitted.height)
    }
    private func emit(_ phase: String, _ point: CGPoint) {
        let rect = imageRect
        guard rect.width > 0, rect.height > 0 else { return }
        let x = min(1, max(0, (point.x - rect.minX) / rect.width))
        let y = min(1, max(0, (point.y - rect.minY) / rect.height))
        send(["action": .string("touch"), "phase": .string(phase), "x": .number(Double(x)), "y": .number(Double(y))])
        if phase == "end", hypot(point.x - startPoint.x, point.y - startPoint.y) < 12,
           let region = regions.first(where: { Double(x) >= $0.x && Double(x) <= $0.x + $0.width && Double(y) >= $0.y && Double(y) <= $0.y + $0.height }) {
            bridge.configure(type: region.keyboard.type, multiline: region.keyboard.multiline); showKeyboard(region.keyboard)
        }
    }
    private func drawTouch(_ point: CGPoint) {
        CATransaction.begin(); CATransaction.setDisableActions(true)
        if !UIAccessibility.isReduceMotionEnabled { path.addLine(to: point); trail.path = path.cgPath }
        ripple.path = UIBezierPath(ovalIn: CGRect(x: point.x - 16, y: point.y - 16, width: 32, height: 32)).cgPath
        CATransaction.commit()
    }
    func resetTouch() { tracked = nil; path = UIBezierPath(); trail.path = nil; ripple.path = nil }
    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard acceptsInput, tracked == nil, let touch = touches.first else { return }
        let point = touch.location(in: self)
        guard imageRect.contains(point) else { return }
        tracked = touch; startPoint = point; lastPoint = point; path.move(to: point)
        drawTouch(point); emit("start", point)
    }
    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard acceptsInput, let touch = tracked, touches.contains(touch) else { return }
        lastPoint = touch.location(in: self); drawTouch(lastPoint); emit("move", lastPoint)
    }
    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let touch = tracked, touches.contains(touch) else { return }
        if acceptsInput { emit("end", touch.location(in: self)) }; resetTouch()
    }
    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let touch = tracked, touches.contains(touch) else { return }
        if acceptsInput { emit("cancel", touch.location(in: self)) }; resetTouch()
    }
}


// Reject all feed redirects: update discovery only contacts the pinned endpoint.
private final class AppUpdateSessionDelegate: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

@MainActor
private final class NativeAppUpdateModel: ObservableObject {
    @Published var update: AppUpdate?
    @Published var checking = false
    @Published var checked = false
    @Published var installing = false
    @Published var error: String?
    @Published var installRequested = false
    var installedBuild: String { Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "" }
    var installedVersion: String { Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "—" }

    func check() async {
        guard !checking else { return }
        checking = true
        error = nil
        defer { checking = false }
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 20
        configuration.timeoutIntervalForResource = 30
        let session = URLSession(configuration: configuration, delegate: AppUpdateSessionDelegate(), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        do {
            var request = URLRequest(url: AppUpdate.feedURL)
            request.cachePolicy = .reloadIgnoringLocalCacheData
            let (data, response) = try await session.data(for: request)
            try Task.checkCancellation()
            guard let response = response as? HTTPURLResponse, response.statusCode == 200,
                  response.url == AppUpdate.feedURL, data.count <= 128 * 1024 else {
                throw AppUpdate.ValidationError.invalidFeed
            }
            let candidate = try JSONDecoder().decode(AppUpdate.self, from: data)
            try candidate.validate()
            let next = try AppUpdate.isNewer(candidate.build, than: installedBuild) ? candidate : nil
            if next?.build != update?.build { installRequested = false }
            update = next
            checked = true
        } catch is CancellationError {
        } catch {
            self.error = "Couldn’t check for updates. " + error.localizedDescription
        }
    }

    func install(_ candidate: AppUpdate) async {
        error = nil
        installRequested = false
        do {
            guard let url = try candidate.installationURL(installedBuild: installedBuild) else { return }
            installing = true
            defer { installing = false }
            // Open the installer scheme directly, independent of the default web browser.
            let opened = await UIApplication.shared.open(url, options: [:])
            if opened { installRequested = true }
            else { error = "iOS couldn’t open the installer. Try Install update again." }
        } catch { self.error = error.localizedDescription }
    }
}

@MainActor
private struct NativeAppUpdateSection: View {
    @ObservedObject var updater: NativeAppUpdateModel
    var body: some View {
        Section("Nanocodex updates") {
            LabeledContent("Installed", value: "\(updater.installedVersion) (\(updater.installedBuild))")
            if updater.checking { ProgressView("Checking for updates…") }
            if let update = updater.update {
                LabeledContent("Available", value: "\(update.version) (\(update.build))")
                if let notes = update.notes, !notes.isEmpty { Text(notes).font(.caption).foregroundStyle(.secondary) }
                Button(updater.installing ? "Opening installer…" : "Install update") { Task { await updater.install(update) } }
                    .disabled(updater.installing)
                    .accessibilityIdentifier("install-nanocodex-update")
            } else if updater.checked && !updater.checking && updater.error == nil {
                Text("You’re up to date.").foregroundStyle(.secondary)
            }
            if updater.installRequested { Text("Confirm the iOS installation prompt, then return to the Home Screen while the app updates.").font(.caption).foregroundStyle(.secondary) }
            if let error = updater.error { Text(error).font(.caption).foregroundStyle(.red) }
            Button(updater.error == nil ? "Check for updates" : "Retry update check") { Task { await updater.check() } }
                .disabled(updater.checking || updater.installing)
                .accessibilityIdentifier("check-nanocodex-update")
        }
        .task { await updater.check() }
    }
}
