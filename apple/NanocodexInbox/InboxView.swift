import SwiftUI
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

private enum Ink {
    static let background = ChatPalette.background
    static let card = Color(uiColor: .secondarySystemGroupedBackground)
    static let surface = ChatPalette.userBubble
    static let border = Color(uiColor: .separator)
    static let text = Color.primary
    static let muted = Color.secondary
    static let accent = Color.primary
    static let amber = Color.secondary
}

struct InboxView: View {
    @ObservedObject var model: InboxModel
    @State private var showOverview = false
    @State private var readingPositions = ConversationReadingPositions()
    @State private var showScheduledJobs = false
    @State private var showSettings = false
    @State private var showScreens = false
    @State private var screenFraction = 0.46
    @GestureState private var screenResize: CGFloat = 0
    @State private var tabScrub: TabScrub?
    @State private var tabScrubEdge = 0
    @State private var toolbarBounds = CGRect.zero
    @GestureState private var draggingTabs = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.scenePhase) private var scenePhase
    @ScaledMetric(relativeTo: .subheadline) private var tabWidth = 154.0
    @ScaledMetric(relativeTo: .subheadline) private var tabHeight = 44.0
    @FocusState private var composerFocused: Bool

    private struct TabScrub {
        let ids: [String]
        var index: Int
        var translation: CGFloat = 0
        var remainder: CGFloat = 0
        var selectedID: String { ids[index] }
    }

    private var highlightedTabID: String? { tabScrub?.selectedID ?? model.focused?.id }

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
                .navigationDestination(isPresented: $showSettings) {
                    settings
                        #if os(iOS)
                        .toolbar(.visible, for: .navigationBar)
                        .navigationBarTitleDisplayMode(.inline)
                        #endif
                }
        }
        .foregroundStyle(Ink.text)
        .tint(Ink.accent)
        .sheet(isPresented: $showOverview) {
            ConversationOverview(model: model) { id in
                selectConversation(id)
                showOverview = false
            }
            .tint(Ink.accent)
        }
        .sheet(isPresented: $model.showContext) { ContextInboxView(model: model).tint(Ink.accent) }
        .onChange(of: model.focused?.activeTurns ?? []) { _, turns in
            if !turns.contains(model.selectedTurn) { model.selectedTurn = turns.first ?? "" }
        }
        .onChange(of: model.focusedConversationIdentity, initial: true) { _, _ in
            composerFocused = false
            if model.focused != nil { model.openThread() }
        }
        .onChange(of: model.connected) { _, connected in
            if !connected { showScreens = false; showScheduledJobs = false; showSettings = false; showOverview = false; readingPositions.values.removeAll() }
        }
        .onChange(of: draggingTabs) { _, dragging in
            if !dragging { tabScrub = nil; tabScrubEdge = 0 }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { tabScrub = nil; tabScrubEdge = 0 }
        }
        .task(id: tabScrubEdge) {
            guard tabScrubEdge != 0 else { return }
            let direction = tabScrubEdge
            while !Task.isCancelled {
                do { try await Task.sleep(for: .milliseconds(90)) } catch { return }
                guard let scrub = tabScrub else { return }
                moveTabScrub(to: scrub.index + direction)
            }
        }
    }

    private var inbox: some View {
        ZStack {
            Ink.background.ignoresSafeArea()
            if model.restoringAccount {
                accountRestoration
            } else if model.connected {
                inboxContent
            } else { ConnectView(model: model) }
        }
    }
    private var inboxContent: some View {
        VStack(spacing: 0) {
            browserTabs.padding(.vertical, 4)
                .modifier(InboxHeaderGlass())
                .padding(.horizontal, 12).padding(.top, 6).padding(.bottom, 6)
            ConnectionStatusView(status: model.threadLoading ? "" : model.connection, retry: { model.retryConnection() }, signIn: { showSettings = true })
                .frame(maxWidth: .infinity, alignment: .trailing).padding(.horizontal, 16)
            GeometryReader { geometry in
                VStack(spacing: 0) {
                    if showScreens, let service = model.remoteService {
                        let available = geometry.size.height
                        let height = screenHeight(available: available, translation: screenResize)
                        RemoteDashboard(service: service, onClose: { showScreens = false })
                            .id(ObjectIdentifier(service))
                            .frame(height: height).clipped()
                        screenDivider(available: available)
                    }
                    if let identity = model.focusedConversationIdentity {
                        ConversationView(model: model, composerFocused: $composerFocused,
                                         identity: identity, readingPositions: readingPositions)
                            .id(identity)
                    } else {
                        emptyState.frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                }
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
                browserToolbar
            }.background(Ink.background)
        }
    }

    private func screenHeight(available: CGFloat, translation: CGFloat = 0) -> CGFloat {
        // Give the preview more of the remaining space when the composer opens
        // the keyboard, while keeping history and the resize handle reachable.
        let fraction = screenFraction + (composerFocused ? 0.2 : 0)
        let maximum = max(0, available - (composerFocused ? 44 : 100) - 24)
        let base = min(max(100, available * fraction), maximum)
        return min(max(100, base + translation), maximum)
    }

    private func screenDivider(available: CGFloat) -> some View {
        Capsule().fill(Ink.muted.opacity(0.45)).frame(width: 36, height: 4)
            .frame(maxWidth: .infinity).frame(height: 24)
            .background(Ink.background).contentShape(Rectangle())
            .gesture(DragGesture().updating($screenResize) { value, state, _ in
                state = value.translation.height
            }.onEnded { value in
                let height = screenHeight(available: available, translation: value.translation.height)
                screenFraction = min(0.75, max(0.25, height / max(1, available) - (composerFocused ? 0.2 : 0)))
            })
            .accessibilityElement().accessibilityLabel("Screen height")
            .accessibilityValue("\(Int(screenHeight(available: available) / max(1, available) * 100)) percent")
            .accessibilityHint("Drag to resize the screen and conversation")
            .accessibilityAdjustableAction { direction in
                screenFraction = min(0.75, max(0.25, screenFraction + (direction == .increment ? 0.1 : -0.1)))
            }
            .accessibilityIdentifier("screen-pane-divider")
    }

    private var browserTabs: some View {
        GeometryReader { geometry in
            let width = min(tabWidth, geometry.size.width)
            ScrollViewReader { scroll in
                ScrollView(.horizontal) {
                    LazyHStack(spacing: 6) {
                        ForEach(model.tabCards) { card in
                            Button { selectConversation(card.id) } label: {
                                HStack(spacing: 6) {
                                    Text(card.title).font(.subheadline.weight(highlightedTabID == card.id ? .semibold : .regular))
                                        .lineLimit(1).frame(maxWidth: width - 24)
                                }
                                .padding(.horizontal, 12).frame(height: tabHeight - 8)
                                .background(highlightedTabID == card.id ? Ink.surface : Color.clear, in: RoundedRectangle(cornerRadius: 12))
                                .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(highlightedTabID == card.id ? Ink.border : Color.clear))
                            }
                            // Stable widths keep distant selections accurate in a lazy strip.
                            .frame(width: width, height: tabHeight).id(card.id)
                            .accessibilityLabel(card.title)
                            .accessibilityValue(card.status)
                            .accessibilityAddTraits(model.focused?.id == card.id ? [.isSelected] : [])
                            .accessibilityIdentifier("browser-tab:" + card.id)
                        }
                    }
                }
                .scrollIndicators(.hidden)
                .onChange(of: highlightedTabID, initial: true) { _, id in
                    guard let id else { return }
                    withAnimation(tabScrub == nil || reduceMotion ? nil : .interactiveSpring(response: 0.18, dampingFraction: 0.92)) {
                        scroll.scrollTo(id, anchor: .center)
                    }
                }
            }
            .accessibilityIdentifier("browser-tabs")
        }
        .buttonStyle(.plain).padding(.horizontal, 8)
        .frame(height: tabHeight)
    }

    private var browserToolbar: some View {
        HStack(spacing: 0) {
            Button { composerFocused = false; model.back() } label: {
                Image(systemName: "chevron.left").frame(width: 44, height: 44)
            }
            .accessibilityLabel("Back").accessibilityIdentifier("conversation-back")
            .disabled(!model.canGoBack)
            Spacer(minLength: 0)
            Button { composerFocused = false; showScreens.toggle() } label: {
                Image(systemName: "display").frame(width: 44, height: 44)
                    .background(showScreens ? Ink.surface : Color.clear, in: RoundedRectangle(cornerRadius: 10))
            }
            .accessibilityLabel("Remote screens").disabled(model.remoteService == nil)
            .accessibilityValue(showScreens ? "Visible" : "Hidden")
            .accessibilityIdentifier("conversation-remote-screens")
            Spacer(minLength: 0)
            Button(action: createAgent) {
                Image(systemName: "plus").frame(width: 44, height: 44)
            }
            .accessibilityLabel("New conversation").accessibilityIdentifier("new-conversation")
            .keyboardShortcut("n", modifiers: .command)
            Spacer(minLength: 0)
            Button {
                guard !draggingTabs else { return }
                composerFocused = false; showOverview = true
            } label: {
                Text(String(model.cards.count)).font(.system(size: 13, weight: .semibold)).monospacedDigit()
                    .frame(minWidth: 23, minHeight: 25)
                    .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(Ink.text, lineWidth: 1.5))
                    .frame(width: 44, height: 44)
            }
            .accessibilityLabel("Conversation overview").accessibilityValue("\(model.cards.count) conversations")
            .accessibilityHint("Tap to show windows. Drag left or right to move through tabs; hold at an edge to keep scrolling.")
            .accessibilityIdentifier("tab-overview")
            .highPriorityGesture(tabScrubGesture)
            .accessibilityAdjustableAction { direction in
                let ids = model.tabCards.map(\.id)
                guard let current = ids.firstIndex(of: model.focused?.id ?? "") else { return }
                let next = direction == .increment ? current + 1 : current - 1
                if ids.indices.contains(next) { selectConversation(ids[next]) }
            }
            Spacer(minLength: 0)
            Menu {
                Button { composerFocused = false; showScheduledJobs = true } label: {
                    Label("Scheduled jobs", systemImage: "clock")
                }.accessibilityIdentifier("inbox-scheduled-jobs")
                Button { composerFocused = false; showSettings = true } label: {
                    Label("Account settings", systemImage: "gearshape")
                }
            } label: {
                Image(systemName: "ellipsis").frame(width: 44, height: 44)
            }.accessibilityLabel("App menu").accessibilityIdentifier("app-menu")
        }
        .font(.system(size: 20, weight: .medium)).buttonStyle(.plain)
        .padding(.horizontal, 16).padding(.bottom, 2).frame(maxWidth: 620)
        .frame(maxWidth: .infinity).background(Ink.background)
        .overlay(alignment: .top) { Rectangle().fill(Ink.border.opacity(0.35)).frame(height: 0.5) }
        .background(GeometryReader { geometry in
            Color.clear.onAppear { toolbarBounds = geometry.frame(in: .global) }
                .onChange(of: geometry.frame(in: .global)) { _, frame in toolbarBounds = frame }
        })
        .overlay(alignment: .top) {
            if let scrub = tabScrub {
                HStack(spacing: 12) {
                    Text(model.cards.first(where: { $0.id == scrub.selectedID })?.title ?? "Conversation")
                        .lineLimit(1).frame(maxWidth: .infinity, alignment: .leading)
                    Text("\(scrub.index + 1) / \(scrub.ids.count)").monospacedDigit().foregroundStyle(.secondary)
                }
                .font(.system(size: 14, weight: .medium)).padding(.horizontal, 16).padding(.vertical, 12)
                .modifier(InboxHeaderGlass()).frame(maxWidth: 320).padding(.horizontal, 16)
                .offset(y: -56).allowsHitTesting(false).accessibilityIdentifier("tab-scrub-preview")
            }
        }
    }

    private var tabScrubGesture: some Gesture {
        DragGesture(minimumDistance: 8, coordinateSpace: .global)
            .updating($draggingTabs) { _, dragging, _ in dragging = true }
            .onChanged { value in
                if tabScrub == nil {
                    guard abs(value.translation.width) > abs(value.translation.height) else { return }
                    let ids = model.tabCards.map(\.id)
                    guard ids.count > 1, let origin = ids.firstIndex(of: model.focused?.id ?? "") else { return }
                    tabScrub = TabScrub(ids: ids, index: origin)
                }
                guard var scrub = tabScrub else { return }
                scrub.remainder += value.translation.width - scrub.translation
                scrub.translation = value.translation.width
                let steps = Int(scrub.remainder / 28)
                scrub.remainder -= CGFloat(steps) * 28
                tabScrub = scrub
                moveTabScrub(to: scrub.index - steps)
                tabScrubEdge = value.location.x < toolbarBounds.minX + 24 ? 1
                    : value.location.x > toolbarBounds.maxX - 24 ? -1 : 0
            }
            .onEnded { _ in
                let selected = tabScrub?.selectedID
                tabScrub = nil; tabScrubEdge = 0
                // Scrubbing only moves the lightweight strip. Load history once,
                // after release, instead of opening every conversation passed.
                if let selected { selectConversation(selected) }
            }
    }

    private func moveTabScrub(to index: Int) {
        guard var scrub = tabScrub else { return }
        let next = min(max(index, 0), scrub.ids.count - 1)
        guard next != scrub.index else { return }
        scrub.index = next; tabScrub = scrub
        UISelectionFeedbackGenerator().selectionChanged()
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
            Text(model.filter == .running ? "No running conversations" : "No conversations").font(.title2.weight(.medium))
            Text("Start a conversation with +.").font(.subheadline).foregroundStyle(Ink.muted).multilineTextAlignment(.center)
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
                Section("This device") {
                    Toggle("Make this device available as a Hand", isOn: $model.deviceHandEnabled)
                        .accessibilityIdentifier("device-hand-enabled")
                    Label(model.deviceHandStatus, systemImage: "hand.raised")
                        .accessibilityIdentifier("device-hand-status")
                    Text("Connects automatically to your account unless disabled. Agents can work with workspace files and query captured messages when capture is enabled.").font(.caption).foregroundStyle(.secondary)
                    Text("Tasks you start can keep this Hand connected in the background on iOS 26 or later. iOS shows progress and lets you stop the task. When idle, this phone connects only during brief background windows or while Nanocodex is open. Force-quitting ends background work.").font(.caption).foregroundStyle(.secondary)
                    if let error = model.handBackgroundError { Text(error).font(.caption).foregroundStyle(.secondary) }
                }
            }
            Section("Controls") {
                Text("Tap a tab at the top to switch conversations. The bottom bar has Back, Screens, + for a new conversation, the tab selector, and the app menu. Tap the tab selector to see all windows, or drag it to switch tabs.")
                Text("The overview shows the latest conversation history. A green border identifies running agents. Drafts and reading positions stay with each conversation.").font(.caption)
                Text("Scroll up to read earlier messages. Send queues a message; Steer now stops the current turn so the queued message can start. ⌘Return sends your message.").font(.caption)
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
        showOverview = false
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

private struct ConversationOverview: View {
    @ObservedObject var model: InboxModel
    var select: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @State private var query = ""
    @State private var runningOnly = false
    @State private var order: [String]

    init(model: InboxModel, select: @escaping (String) -> Void) {
        self.model = model
        self.select = select
        _order = State(initialValue: model.cards.sorted(by: AgentCard.mostRecentFirst).map(\.id))
    }

    private var visibleCards: [AgentCard] {
        let text = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let cards = Dictionary(uniqueKeysWithValues: model.cards.map { ($0.id, $0) })
        return order.compactMap { cards[$0] }.filter { card in
            (!runningOnly || card.isRunning) && (text.isEmpty || card.title.localizedCaseInsensitiveContains(text)
                || card.id.localizedCaseInsensitiveContains(text) || card.preview.localizedCaseInsensitiveContains(text))
        }
    }

    private func overviewDescription(_ card: AgentCard) -> String {
        let latest = model.overviewRows(for: card.id).last {
            ($0.role == "You" || ($0.role == "Agent" && $0.agentID == nil && $0.phase != "commentary")) && !$0.text.isEmpty
        }
        let text = latest.map { String((ContextPrompt.separate($0.text)?.request ?? $0.text).suffix(600)) } ?? ""
        return card.status + (text.isEmpty ? "" : ". " + text)
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                Picker("Conversations", selection: $runningOnly) {
                    Text("All").tag(false)
                    Text("Running").tag(true)
                }.pickerStyle(.segmented).padding(.horizontal, 16).padding(.top, 8)
                    .accessibilityIdentifier("overview-filter")
            ScrollView {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: dynamicTypeSize.isAccessibilitySize ? 280 : 155), spacing: 14)], spacing: 18) {
                    ForEach(visibleCards) { card in
                        Button { select(card.id) } label: {
                            VStack(alignment: .leading, spacing: 9) {
                                Text(card.title).font(.system(size: 15, weight: .semibold)).lineLimit(2, reservesSpace: true)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                ConversationMiniature(model: model, card: card, rows: model.overviewRows(for: card.id)).equatable()
                                    .frame(height: 230)
                                    .background(Ink.background)
                                    .clipShape(RoundedRectangle(cornerRadius: 16))
                                    .overlay {
                                        RoundedRectangle(cornerRadius: 16)
                                            .strokeBorder(card.isRunning ? Color.green : model.focused?.id == card.id ? Ink.text : Ink.border,
                                                          lineWidth: card.isRunning || model.focused?.id == card.id ? 2 : 0.75)
                                    }
                                    .accessibilityIdentifier("overview-preview:" + card.id)
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityRepresentation {
                            Button(card.title) { select(card.id) }
                                .accessibilityValue(overviewDescription(card))
                                .accessibilityHint("Open conversation")
                                .accessibilityAddTraits(model.focused?.id == card.id ? [.isSelected] : [])
                                .accessibilityIdentifier("overview-card:" + card.id)
                        }
                        .onAppear { model.setOverviewVisible(card.id, visible: true) }
                        .onDisappear { model.setOverviewVisible(card.id, visible: false) }
                    }
                }.padding(16)
                if visibleCards.isEmpty {
                    ContentUnavailableView(model.cards.isEmpty ? "No conversations" : "No matching conversations", systemImage: "bubble.left.and.bubble.right",
                                           description: Text(model.cards.isEmpty ? "Use + to start a conversation." : "Try another search or show all conversations."))
                }
            }.scrollDismissesKeyboard(.interactively).accessibilityIdentifier("conversation-overview")
            }
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search conversations")
            .background(Ink.background)
            .navigationTitle("Conversations (\(model.cards.count))")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItemGroup(placement: .bottomBar) {
                    Button {
                        model.newAgent()
                        dismiss()
                    } label: { Image(systemName: "plus") }
                    .accessibilityLabel("New conversation").accessibilityIdentifier("new-conversation-overview")
                    Spacer()
                    Button("Done") { dismiss() }
                }
            }
        }
        .presentationDetents([.large]).presentationDragIndicator(.visible)
        .onChange(of: model.cards.map(\.id)) { _, ids in
            // Live history updates must not move another window under a tap.
            let available = Set(ids)
            order.removeAll { !available.contains($0) }
            let known = Set(order)
            order.append(contentsOf: ids.filter { !known.contains($0) })
        }
        .onDisappear { model.stopOverview() }
    }
}

private struct ConversationMiniature: View, Equatable {
    let model: InboxModel
    let card: AgentCard
    let rows: [TranscriptRow]
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.card.id == rhs.card.id && lhs.card.activeTurns == rhs.card.activeTurns
            && lhs.card.error == rhs.card.error && lhs.card.checked == rhs.card.checked
            && lhs.card.turnCount == rhs.card.turnCount && lhs.rows == rhs.rows
    }
    private let scale: CGFloat = 0.48

    var body: some View {
        GeometryReader { viewport in
            let items = Array(ConversationItem.group(rows, activeTurns: Set(card.activeTurns)).suffix(4))
            VStack(alignment: .leading, spacing: 18) {
                if items.isEmpty {
                    if let error = card.error {
                        Text(error).font(.system(size: 17)).foregroundStyle(Ink.muted)
                    } else if card.checked, card.turnCount == 0, card.previewRows.isEmpty {
                        Text("Send a message to begin.").font(.system(size: 17)).foregroundStyle(Ink.muted)
                    } else {
                        ProgressView().frame(maxWidth: .infinity)
                    }
                }
                ForEach(items) { item in
                    if let row = item.message {
                        ConversationMessageView(row: row, model: model, agentID: card.id)
                    } else {
                        VStack(alignment: .leading, spacing: 14) {
                            HStack(spacing: 10) {
                                Image(systemName: item.isRunning ? "circle.fill" : "checkmark")
                                    .font(.system(size: 12)).foregroundStyle(item.isRunning ? Color.green : Ink.muted)
                                Text(item.isRunning ? "Working" : "Activity").font(.system(size: 13, weight: .medium))
                                Spacer(minLength: 4)
                                Text("\(item.activity.count) steps").font(.caption)
                            }.foregroundStyle(Ink.muted).padding(13)
                                .background(Ink.surface, in: RoundedRectangle(cornerRadius: 14))
                            InboxGeneratedOutputView(rows: item.activity).equatable()
                        }
                    }
                }
            }
            .padding(20)
            .frame(width: viewport.size.width / scale, alignment: .leading)
            .fixedSize(horizontal: false, vertical: true)
            .scaleEffect(scale, anchor: .bottomLeading)
            .frame(width: viewport.size.width, height: viewport.size.height, alignment: .bottomLeading)
        }
        .clipped().allowsHitTesting(false).accessibilityHidden(true)
        // Preview updates stay steady even while a running transcript grows.
        .transaction { $0.animation = nil; $0.disablesAnimations = true }
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
    @FocusState.Binding var focused: Bool
    var onVoiceChat: @MainActor () -> Void = {}
    @State private var showPhotos = false
    @State private var showFiles = false
    @State private var selectedPhotos: [PhotosPickerItem] = []
    @State private var photoTarget: InboxModel.AttachmentTarget?
    @State private var fileTarget: InboxModel.AttachmentTarget?
    @State private var pickerError: String?
    #if os(iOS)
    @State private var showCamera = false
    @State private var cameraTarget: InboxModel.AttachmentTarget?
    @State private var cameraPermissionDenied = false
    #endif

    private var visiblePending: [PendingMessage] {
        model.focusedPending.filter { !$0.predecessor.isEmpty || $0.phase == .failed }
    }
    private var sendShowsStop: Bool {
        model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && model.focusedAttachments.isEmpty && !model.preparingAttachments
            && !model.stopTarget.isEmpty
    }
    private var stopRequest: PendingTurnCancellation? {
        model.focused.flatMap { model.cancellation(agentID: $0.id, turnID: model.stopTarget) }
    }
    private var pendingHeight: CGFloat {
        let messages = visiblePending
        if messages.count > 1 { return messages.contains { $0.attachments?.isEmpty == false } ? 136 : 104 }
        return messages.first?.error == nil && (messages.first?.attachments?.isEmpty ?? true) ? 48 : 82
    }

    var body: some View {
        VStack(spacing: 0) {
            if let error = model.creationError {
                HStack {
                    Text(error).font(.caption).foregroundStyle(Ink.muted)
                    Spacer()
                    Button("Retry") { model.retryCreation() }.accessibilityIdentifier("retry-creation")
                }.padding(12)
            }
            if let agentID = model.focused?.id, !model.contextForAgent(agentID).isEmpty {
                Button { focused = false; model.showContext = true } label: {
                    Label("Context for your next message (\(model.contextForAgent(agentID).count))", systemImage: "tray.full")
                        .font(.caption).padding(.vertical, 10)
                }.accessibilityIdentifier("composer-context")
            }
            if model.controllableTurns.count > 1 {
                Picker("Active turn", selection: $model.selectedTurn) {
                    ForEach(model.controllableTurns, id: \.self) { id in Text("Turn \(model.controllableTurns.firstIndex(of: id).map { $0 + 1 } ?? 1)").tag(id) }
                }.pickerStyle(.menu)
            }
            if !visiblePending.isEmpty {
                ScrollView {
                    VStack(spacing: 0) {
                        ForEach(visiblePending) { message in
                            HStack(alignment: .center, spacing: 8) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(message.phase == .submitting ? "Sending…" : message.phase == .starting ? "Stopping current turn…" : message.phase == .cancelling ? "Cancelling…" : message.phase == .failed ? "Not confirmed" : "Queued")
                                        .font(.system(size: 11)).foregroundStyle(Ink.muted)
                                    Text(ContextPrompt.separate(message.input)?.request ?? message.input).font(.system(size: 13)).lineLimit(1)
                                        .accessibilityIdentifier("pending-message")
                                    if let attachments = message.attachments, !attachments.isEmpty {
                                        Label(attachments.map(\.name).joined(separator: ", "), systemImage: "photo")
                                            .font(.caption2).lineLimit(1).accessibilityIdentifier("pending-attachments")
                                    }
                                    if let error = message.error { Text(error).font(.caption2).foregroundStyle(Ink.muted) }
                                }.frame(maxWidth: .infinity, alignment: .leading)
                                if message.phase == .failed {
                                    Button { model.retryPending(message.id) } label: { Text("Retry").frame(minHeight: 44) }.accessibilityIdentifier("retry-pending")
                                        .disabled(model.busy.contains(message.agentID))
                                } else if model.steeringTarget(message) != nil {
                                    Button { model.steerNow(message.id) } label: { Text("Steer now").frame(minHeight: 44) }.accessibilityIdentifier("steer-now")
                                }
                                Button { model.cancelPending(message.id) } label: {
                                    Image(systemName: "xmark").frame(width: 44, height: 44).contentShape(Rectangle())
                                }.accessibilityLabel("Cancel queued message")
                                    .disabled(model.cancellation(agentID: message.agentID, turnID: message.id).map { $0.error == nil } ?? false)
                            }.font(.system(size: 13, weight: .medium)).buttonStyle(.plain)
                                .padding(.leading, 16)
                        }
                    }
                }.frame(maxHeight: pendingHeight)
                    .padding(.top, 4)
                    .accessibilityIdentifier("pending-messages")
                Rectangle().fill(Ink.border).frame(height: 0.5).padding(.horizontal, 16)
            }
            if !model.focusedAttachments.isEmpty {
                ScrollView(.horizontal) {
                    HStack(spacing: 10) {
                        ForEach(model.focusedAttachments) { attachment in
                            VStack(alignment: .leading, spacing: 4) {
                                Group {
                                    if attachment.isVideo {
                                        AttachmentMovieThumbnail(attachment: attachment, poster: model.attachmentURL(attachment), movie: model.attachmentMovieURL(attachment))
                                    } else {
                                        AttachmentImageView(source: model.attachmentURL(attachment).map(AttachmentImageSource.file))
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
                if model.focusedAttachments.contains(where: \.isVideo) {
                    Text("Original video").font(.caption).foregroundStyle(Ink.muted)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 16).padding(.bottom, 6)
                        .accessibilityIdentifier("video-analysis-description")
                }
            }
            if model.preparingAttachments {
                ProgressView().accessibilityLabel("Preparing attachments").font(.caption).frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16).padding(.vertical, 8).accessibilityIdentifier("preparing-attachments")
            }
            if let error = pickerError ?? model.attachmentError {
                Text(error).font(.caption).foregroundStyle(Ink.muted).frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16).padding(.vertical, 8).accessibilityIdentifier("attachment-error")
            }
            HStack(alignment: .bottom, spacing: 8) {
                Menu {
                    #if os(iOS)
                    Button { Task { await openCamera() } } label: { Label("Camera", systemImage: "camera") }
                        .disabled(model.preparingAttachments).accessibilityIdentifier("choose-camera")
                    #endif
                    Button {
                        guard let target = model.captureAttachmentTarget() else { return }
                        photoTarget = target; selectedPhotos = []; pickerError = nil; focused = false; showPhotos = true
                    } label: { Label("Photos & Videos", systemImage: "photo.on.rectangle") }
                        .disabled(model.preparingAttachments).accessibilityIdentifier("choose-photos")
                    Button {
                        guard let target = model.captureAttachmentTarget() else { return }
                        fileTarget = target; pickerError = nil; focused = false; showFiles = true
                    } label: { Label("Files", systemImage: "folder") }
                        .disabled(model.preparingAttachments).accessibilityIdentifier("choose-files")
                    Button { focused = false; model.showContext = true } label: {
                        Label("Context from other apps", systemImage: "tray.full")
                    }
                } label: {
                    Image(systemName: "plus").frame(width: 44, height: 44).contentShape(Rectangle())
                }.menuStyle(.borderlessButton).accessibilityLabel("Add attachments").accessibilityIdentifier("add-attachments")
                TextField("Ask Nanocodex", text: $model.draft, axis: .vertical)
                    .lineLimit(1...4).textFieldStyle(.plain).font(.body).focused($focused)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.vertical, 8).accessibilityIdentifier("composer")
                if let agentID = model.focused?.id {
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
                            Image(systemName: sendShowsStop ? "stop.fill" : model.busy.contains(model.focused?.id ?? "") ? "ellipsis" : "arrow.up")
                        }
                    }.font(.system(size: 18, weight: .semibold)).frame(width: 44, height: 44)
                        .background(Ink.accent.opacity(sendShowsStop || model.canSend ? 1 : 0.22), in: Circle()).foregroundStyle(Ink.background)
                }.buttonStyle(.plain)
                    .disabled(sendShowsStop ? stopRequest.map { $0.error == nil } ?? false : !model.canSend)
                    .accessibilityLabel(sendShowsStop ? stopRequest.map { $0.error == nil ? "Stopping turn" : "Retry stop" } ?? "Stop turn" : model.focused?.isRunning == true ? "Queue message" : "Send message")
                    .accessibilityIdentifier("send")
                    .keyboardShortcut(sendShowsStop ? nil : KeyboardShortcut(.return, modifiers: .command))
            }.padding(.horizontal, 8).padding(.bottom, 8).padding(.top, visiblePending.isEmpty ? 8 : 0)
                .padding(.leading, 6).accessibilityElement(children: .contain).accessibilityIdentifier("composer-input")

        }.background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 28))
            .overlay(RoundedRectangle(cornerRadius: 28).strokeBorder(Color.primary.opacity(focused ? 0.18 : 0.1)))
            .shadow(color: .black.opacity(0.035), radius: 8, y: 2)
            .padding(.horizontal, 12).padding(.top, 6).padding(.bottom, 8).background(Ink.background)
            .onChange(of: model.focusedConversationIdentity) { _, _ in focused = false }
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
            .photosPicker(isPresented: $showPhotos, selection: $selectedPhotos, maxSelectionCount: 4, matching: .any(of: [.images, .videos]))
            .onChange(of: selectedPhotos) { _, items in
                guard !items.isEmpty, let target = photoTarget else { return }
                model.importAttachmentPhotos(items, target: target)
                selectedPhotos = []; photoTarget = nil
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

/// Animate only the status glyphs, leaving the surrounding transcript layout still.
private struct PulsingText: View {
    let text: String
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.scenePhase) private var scenePhase
    var body: some View {
        Text(text)
            .phaseAnimator(reduceMotion || scenePhase != .active ? [false] : [false, true]) { content, dimmed in
                content.opacity(dimmed ? 0.4 : 1)
            } animation: { _ in .easeInOut(duration: 1.1) }
    }
}

private enum AttachmentImageSource: Hashable, Sendable {
    case file(URL)
    case inline(String)
}

private func videoTime(_ seconds: Double) -> String {
    let value = max(0, Int(seconds.rounded(.down)))
    return String(format: "%d:%02d", value / 60, value % 60)
}

private struct AttachmentMovieThumbnail: View {
    let attachment: MessageAttachment
    let poster: URL?
    let movie: URL?
    @State private var showingPreview = false
    var body: some View {
        Button { showingPreview = true } label: {
            AttachmentImageView(source: poster.map(AttachmentImageSource.file))
                .overlay { Image(systemName: "play.circle.fill").font(.title).foregroundStyle(.white).shadow(radius: 3) }
                .overlay(alignment: .bottomLeading) {
                    Text(videoTime(attachment.video?.duration ?? 0)).font(.caption2.monospacedDigit()).foregroundStyle(.white)
                        .padding(.horizontal, 5).padding(.vertical, 2).background(.black.opacity(0.65), in: Capsule()).padding(5)
                }
        }.buttonStyle(.plain).disabled(movie == nil)
            .accessibilityLabel("Preview " + attachment.name).accessibilityIdentifier("preview-video-" + attachment.id)
            .sheet(isPresented: $showingPreview) {
                if let movie { MoviePreview(url: movie, name: attachment.name) }
            }
    }
}

private struct MoviePreview: View {
    let url: URL
    let name: String
    @Environment(\.dismiss) private var dismiss
    @State private var player: AVPlayer?
    var body: some View {
        NavigationStack {
            VStack(spacing: 12) {
                VideoPlayer(player: player).accessibilityIdentifier("video-player")
            }.navigationTitle(name)
                .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }.frame(minWidth: 320, minHeight: 360)
            .onAppear { player = AVPlayer(url: url); player?.play() }
            .onDisappear { player?.pause(); player = nil }
    }
}

private struct VideoAttachmentView: View {
    let video: TranscriptVideo
    let model: InboxModel
    let agentID: String
    @State private var movie: URL?
    @State private var loading = false
    @State private var error: String?
    @State private var showingPreview = false
    @State private var selected = 0.0
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(video.name + " · " + videoTime(video.duration), systemImage: "film").font(.caption.weight(.medium)).lineLimit(2)
            if video.path != nil {
                Button {
                    loading = true; error = nil
                } label: {
                    if loading { ProgressView().accessibilityLabel("Loading video") }
                    else { Label("Play video", systemImage: "play.circle.fill") }
                }.disabled(loading).accessibilityIdentifier("play-original-video")
                if let error { Text(error).font(.caption).foregroundStyle(Ink.muted) }
            } else if !video.images.isEmpty {
                let index = min(video.images.count - 1, max(0, Int(selected)))
                AttachmentImageView(source: .inline(video.images[index]), contentMode: .fit).frame(height: 150)
                if video.images.count > 1 {
                    Slider(value: $selected, in: 0...Double(video.images.count - 1), step: 1)
                        .accessibilityLabel("Video frame").accessibilityIdentifier("video-frame-slider")
                }
                Text(videoTime(video.timestamps[index]) + " · \(video.images.count) frames · No audio")
                    .font(.caption2).foregroundStyle(Ink.muted)
            }
        }.frame(maxWidth: 260).accessibilityElement(children: .contain).accessibilityIdentifier("message-video")
            .task(id: loading) {
                guard loading else { return }
                do { movie = try await model.downloadVideo(video, agentID: agentID); showingPreview = true }
                catch is CancellationError { }
                catch { self.error = error.localizedDescription }
                loading = false
            }
            .sheet(isPresented: $showingPreview, onDismiss: {
                if let movie { try? FileManager.default.removeItem(at: movie) }
                movie = nil
            }) { if let movie { MoviePreview(url: movie, name: video.name) } }
    }
}

private struct AttachmentImageView: View {
    let source: AttachmentImageSource?
    var contentMode: ContentMode = .fill
    @State private var thumbnail: CGImage?

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
        .task(id: source) {
            thumbnail = nil
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
    var pending: PendingMessage? = nil
    var body: some View {
        ConversationMessageContent(row: row, model: model, agentID: agentID, pending: pending,
                                   attachmentURLs: (pending?.attachments ?? []).map { model.attachmentURL($0) },
                                   movieURLs: (pending?.attachments ?? []).map { model.attachmentMovieURL($0) }).equatable()
    }
}

private struct ConversationMessageContent: View, Equatable {
    let row: TranscriptRow
    let model: InboxModel
    let agentID: String
    let pending: PendingMessage?
    let attachmentURLs: [URL?]
    let movieURLs: [URL?]
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.row == rhs.row && lhs.agentID == rhs.agentID && lhs.pending == rhs.pending
            && lhs.attachmentURLs == rhs.attachmentURLs && lhs.movieURLs == rhs.movieURLs
            && lhs.model === rhs.model
    }
    var body: some View {
        HStack(alignment: .top, spacing: 0) {
            if row.role == "You" { Spacer(minLength: 44) }
            VStack(alignment: .leading, spacing: 10) {
                if row.role == "Thinking" {
                    DisclosureGroup {
                        ChatMarkdown(text: row.text)
                    } label: {
                        if row.running { PulsingText(text: "Thinking…") }
                        else { Text("Thought process") }
                    }.font(.system(size: 13)).foregroundStyle(Ink.muted)
                } else if row.role == "You", let content = ContextPrompt.separate(row.text) {
                    Text(content.request).font(.system(size: 17)).lineSpacing(5).textSelection(.enabled)
                    DisclosureGroup("Captured context (\(content.captures.count))") {
                        ForEach(Array(content.captures.enumerated()), id: \.offset) { _, capture in
                            VStack(alignment: .leading, spacing: 4) {
                                Text(capture.source + (capture.sender.isEmpty ? "" : " · " + capture.sender)).font(.caption.weight(.semibold))
                                Text(capture.text.isEmpty ? capture.url : capture.text).font(.subheadline).textSelection(.enabled)
                            }.padding(.vertical, 4)
                        }
                    }.font(.caption).foregroundStyle(Ink.muted)
                } else if row.role == "Agent", !row.text.isEmpty {
                    ChatMarkdown(text: row.text)
                    if !row.running { ChatCopyButton(text: row.text).padding(.leading, -8) }
                } else if !row.text.isEmpty {
                    Text(row.text).font(.system(size: row.role == "Status" ? 14 : 17))
                        .lineSpacing(5).textSelection(.enabled)
                        .foregroundStyle(row.role == "Status" ? Ink.muted : Ink.text)
                }
                if let images = row.images {
                    ForEach(Array(images.filter { $0.hasPrefix("data:image/") }.enumerated()), id: \.offset) { _, image in
                        AttachmentImageView(source: .inline(image), contentMode: .fit)
                            .frame(maxWidth: 240).frame(height: 180)
                            .accessibilityIdentifier("message-image")
                    }
                }
                ForEach(row.videos ?? []) { VideoAttachmentView(video: $0, model: model, agentID: agentID) }
                if let pending {
                    ForEach(pending.attachments ?? []) { attachment in
                        if attachment.isVideo {
                            AttachmentMovieThumbnail(attachment: attachment, poster: model.attachmentURL(attachment), movie: model.attachmentMovieURL(attachment))
                                .frame(width: 200, height: 130)
                        } else if let url = model.attachmentURL(attachment) {
                            AttachmentImageView(source: .file(url), contentMode: .fit)
                                .frame(maxWidth: 240).frame(height: 180).accessibilityIdentifier("message-image")
                        }
                    }
                    Text(pending.phase == .cancelling ? "Cancelling…" : "Sending…").font(.caption).foregroundStyle(Ink.muted)
                }
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel(row.role == "You" ? "Your message" : row.role == "Agent" ? "Assistant message" : row.role)
            .padding(row.role == "You" ? 16 : 0)
            .background(row.role == "You" ? Ink.surface : Color.clear, in: RoundedRectangle(cornerRadius: 24))
            if row.role != "You" { Spacer(minLength: 0) }
        }.frame(maxWidth: .infinity, alignment: row.role == "You" ? .trailing : .leading)
    }
}

private final class ConversationReadingPositions {
    struct Position {
        var atLatest: Bool
        var rowID: String? = nil
        var offsetY: CGFloat = 0
    }
    var values: [String: Position] = [:]
}

private struct ConversationView: View {
    @ObservedObject var model: InboxModel
    @FocusState.Binding var composerFocused: Bool
    let identity: String
    let readingPositions: ConversationReadingPositions

    var body: some View {
        ConversationContentView(model: model, composerFocused: $composerFocused,
                                identity: identity, readingPositions: readingPositions,
                                revision: .init(rows: model.rows, pending: model.focusedPending,
                                                title: model.focused?.title ?? "Conversation",
                                                activeTurns: model.focused?.activeTurns ?? [],
                                                loading: model.threadLoading, error: model.threadError,
                                                hasOlder: model.hasOlder, loadingOlder: model.loadingOlder,
                                                draft: model.draft, composerFocused: composerFocused))
            .equatable()
    }
}

private struct ConversationContentView: View, Equatable {
    struct Revision: Equatable {
        var rows: [TranscriptRow]
        var pending: [PendingMessage]
        var title: String
        var activeTurns: [String]
        var loading: Bool
        var error: String?
        var hasOlder: Bool
        var loadingOlder: Bool
        var draft: String
        var composerFocused: Bool
    }
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.identity == rhs.identity && lhs.revision == rhs.revision && lhs.model === rhs.model
    }
    private let verticalPadding: CGFloat = 24
    let model: InboxModel
    @FocusState.Binding var composerFocused: Bool
    let identity: String
    let readingPositions: ConversationReadingPositions
    let revision: Revision
    @State private var hasInitialPosition = false
    @State private var pendingReadingRestore: ConversationReadingPositions.Position?
    @State private var rowFrames: [String: CGRect] = [:]
    @State private var historyRestore: (id: String, anchor: UnitPoint)?
    @State private var historyContent = ConversationContentPosition()
    @State private var historyReady = false
    @State private var historyRequestAllowed = true
    @State private var historyRequestInFlight = false
    @State private var historyRequestFirstID: String?
    @State private var resizeRestore: [(id: String, y: CGFloat, height: CGFloat)]?
    private var pendingSubmissions: [PendingMessage] {
        model.focusedPending.filter { message in
            message.predecessor.isEmpty && message.phase != .failed
                && !model.rows.contains { $0.role == "You" && ($0.id == message.id || $0.turnID == message.id) }
        }
    }
    private func rememberHistoryPosition(in viewport: GeometryProxy) {
        guard let first = rowFrames.filter({ $0.value.maxY > 0 && $0.value.minY < viewport.size.height })
            .min(by: { $0.value.minY < $1.value.minY }) else { return }
        let available = viewport.size.height - first.value.height
        historyRestore = (first.key, UnitPoint(x: 0, y: abs(available) > 0.5 ? first.value.minY / available : 0))
    }
    private func loadEarlierIfNeeded(in viewport: GeometryProxy) {
        guard model.focusedConversationIdentity == identity, pendingReadingRestore == nil,
              historyReady, historyContent.nearTop, historyRequestAllowed, historyContent.firstID == model.rows.first?.id,
              model.hasOlder, !model.threadLoading, !model.loadingOlder, !historyRequestInFlight else { return }
        historyRequestAllowed = false
        historyRequestInFlight = true
        historyRequestFirstID = model.rows.first?.id
        rememberHistoryPosition(in: viewport)
        Task {
            await model.loadOlder()
            guard model.focusedConversationIdentity == identity else { return }
            let inserted = model.rows.first?.id != historyRequestFirstID
            if !inserted { historyRestore = nil }
            historyRequestInFlight = false
            // Successful pages can prefetch again when their new beginning
            // enters view. A failed page needs a new approach to the top.
            if inserted { historyRequestAllowed = true }
            updateHistoryPosition(in: viewport)
        }
    }
    private func updateHistoryPosition(in viewport: GeometryProxy) {
        guard model.focusedConversationIdentity == identity, hasInitialPosition,
              pendingReadingRestore == nil, historyContent.isMeasured else { return }
        // Ignore the transient top layout before a newly opened conversation
        // reaches its initial position at the bottom.
        if !historyReady {
            guard historyContent.atLatest || readingPositions.values[identity]?.atLatest == false else { return }
            historyReady = true
        }
        if !historyContent.nearTop { historyRequestAllowed = true }
        loadEarlierIfNeeded(in: viewport)
    }
    private func rememberReadingPosition(in viewport: GeometryProxy) {
        let frame = viewport.frame(in: .global)
        resizeRestore = rowFrames.filter { $0.value.maxY > 0 && $0.value.minY < frame.height }
            .sorted {
                let leftVisible = $0.value.minY >= 0 && $0.value.maxY <= frame.height
                let rightVisible = $1.value.minY >= 0 && $1.value.maxY <= frame.height
                return leftVisible == rightVisible ? $0.value.minY < $1.value.minY : leftVisible
            }
            .map { ($0.key, $0.value.minY + frame.minY, $0.value.height) }
    }
    private func restoreReadingPosition(using scroll: ScrollViewProxy, in viewport: GeometryProxy) {
        guard let targets = resizeRestore else { return }
        let viewportFrame = viewport.frame(in: .global)
        for target in targets {
            guard let current = rowFrames[target.id] else { continue }
            // Native scrolling often already preserves the point. Correct only
            // when keyboard resizing or asynchronous Markdown moved that row.
            if abs(current.minY + viewportFrame.minY - target.y) < 1 { return }
            let available = viewportFrame.height - current.height
            guard abs(available) > 0.5 else { continue }
            let anchorY = (target.y - viewportFrame.minY) / available
            var transaction = Transaction()
            transaction.disablesAnimations = true
            withTransaction(transaction) { scroll.scrollTo(target.id, anchor: UnitPoint(x: 0, y: anchorY)) }
            break
        }
    }
    private func saveReadingPosition(in viewport: GeometryProxy) {
        guard model.focusedConversationIdentity == identity, hasInitialPosition,
              pendingReadingRestore == nil, historyReady, historyContent.isMeasured,
              !historyRequestInFlight else { return }
        if historyContent.atLatest {
            readingPositions.values[identity] = .init(atLatest: true)
        } else if let first = rowFrames.filter({ $0.value.maxY > 0 && $0.value.minY < viewport.size.height })
            .sorted(by: {
                let leftFull = $0.value.minY >= 0 && $0.value.maxY <= viewport.size.height
                let rightFull = $1.value.minY >= 0 && $1.value.maxY <= viewport.size.height
                return leftFull == rightFull ? $0.value.minY < $1.value.minY : leftFull
            }).first {
            // Store a point offset, not a fraction of the current viewport.
            // The keyboard may still be changing its height when switching tabs.
            readingPositions.values[identity] = .init(atLatest: false, rowID: first.key, offsetY: first.value.minY)
        }
    }
    var body: some View {
        ScrollViewReader { scroll in
            GeometryReader { viewport in
            ZStack(alignment: .top) {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                LazyVStack(alignment: .leading, spacing: 18) {
                    if model.rows.isEmpty, pendingSubmissions.isEmpty, !model.threadLoading, model.threadError == nil {
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
                    if let error = model.threadError { Text(error).font(.subheadline).foregroundStyle(Ink.muted) }
                    ForEach(ConversationItem.group(model.rows, activeTurns: Set(model.focused?.activeTurns ?? []))) { item in
                        Group {
                        if let row = item.message {
                        ConversationMessageView(row: row, model: model, agentID: model.focused?.id ?? "")
                        } else {
                            VStack(alignment: .leading, spacing: 14) {
                                ConversationActivityView(item: item)
                                InboxGeneratedOutputView(rows: item.activity).equatable()
                            }
                        }
                        }.id(item.id)
                            .background(GeometryReader { geometry in
                                Color.clear.preference(key: ConversationRowFrames.self, value: [item.id: geometry.frame(in: .named("conversation-viewport"))])
                            })
                            .accessibilityElement(children: .contain)
                            .accessibilityIdentifier((item.message?.role == "You" ? "message-user-" : item.message?.role == "Agent" ? "message-assistant-" : "message-") + item.id)
                    }
                    ForEach(pendingSubmissions) { message in
                        ConversationMessageView(row: .init(id: message.id, role: "You", text: message.input),
                                                model: model, agentID: message.agentID, pending: message)
                            .accessibilityElement(children: .contain)
                            .accessibilityIdentifier("message-user-pending-" + message.id)
                    }
                }.scrollTargetLayout()
                    // Keep the bottom target outside the lazy rows so it has a
                    // stable position even when the last reply fills many screens.
                    if let agentID = model.focused?.id {
                        NanocodexVoiceTranscript(session: model.voice, conversationID: agentID, durableRows: model.rows) {
                            if historyContent.atLatest { scroll.scrollTo("latest", anchor: .bottom) }
                        }
                    }
                    Color.clear.frame(height: 1).id("latest")
                }.padding(.horizontal, 20).padding(.vertical, verticalPadding).frame(maxWidth: 780).frame(minHeight: viewport.size.height, alignment: .top).frame(maxWidth: .infinity)
                    .background(GeometryReader { geometry in
                        let frame = geometry.frame(in: .named("conversation-viewport"))
                        // Publish threshold changes, not every fractional offset
                        // or lazy height estimate, to avoid driving another layout.
                        Color.clear.preference(key: ConversationContentFrame.self, value: ConversationContentPosition(
                            firstID: model.rows.first?.id, nearTop: frame.minY >= -240,
                            // The latest marker precedes the bottom padding.
                            atLatest: frame.maxY <= viewport.size.height + verticalPadding + 1,
                            isMeasured: frame.height > 0))
                    })
            }
            .modifier(ChatScrollAnchors(initialAnchor: readingPositions.values[identity]?.atLatest == false ? .top : .bottom))
            .scrollDismissesKeyboard(.interactively)
            .scrollBounceBehavior(.always, axes: .vertical)
            .coordinateSpace(name: "conversation-viewport")
            .onPreferenceChange(ConversationRowFrames.self) {
                rowFrames = $0
                restoreReadingPosition(using: scroll, in: viewport)
                if let target = pendingReadingRestore, let id = target.rowID, let frame = $0[id] {
                    if abs(frame.minY - target.offsetY) < 1 {
                        pendingReadingRestore = nil
                        historyReady = true
                    } else {
                        let available = viewport.size.height - frame.height
                        if abs(available) > 0.5 {
                            // Reapply after the lazy stack measures the target row.
                            scroll.scrollTo(id, anchor: UnitPoint(x: 0, y: target.offsetY / available))
                        }
                    }
                }
                saveReadingPosition(in: viewport)
                // Continue following the reader during a slow history request,
                // until the insertion changes the coordinate space.
                if historyRequestInFlight, model.rows.first?.id == historyRequestFirstID {
                    rememberHistoryPosition(in: viewport)
                }
            }
            .onPreferenceChange(ConversationContentFrame.self) { content in
                historyContent = content
                updateHistoryPosition(in: viewport)
                saveReadingPosition(in: viewport)
            }
            .onChange(of: hasInitialPosition) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: model.hasOlder) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: model.threadLoading) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: model.loadingOlder) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: composerFocused) { _, _ in rememberReadingPosition(in: viewport) }
            .onChange(of: model.draft) { _, _ in
                if resizeRestore == nil { rememberReadingPosition(in: viewport) }
            }
            .onChange(of: viewport.frame(in: .global)) { _, _ in
                restoreReadingPosition(using: scroll, in: viewport)
            }
            .simultaneousGesture(DragGesture(minimumDistance: 1).onChanged { _ in
                resizeRestore = nil
                pendingReadingRestore = nil
                if hasInitialPosition { historyReady = true }
            })
            .background(Ink.background)
            .accessibilityLabel(revision.title)
            .accessibilityIdentifier("conversation")
            .overlay {
                if model.threadLoading {
                    ProgressView()
                        .accessibilityLabel("Loading conversation")
                        .accessibilityIdentifier("conversation-loading")
                        .allowsHitTesting(false)
                }
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
            .onChange(of: model.rows.first?.id, initial: true) { _, _ in
                if !hasInitialPosition, !model.rows.isEmpty {
                    // Position a newly opened conversation once. Later viewport
                    // changes, including the keyboard, retain its top edge.
                    if let saved = readingPositions.values[identity], !saved.atLatest,
                       let id = saved.rowID,
                       ConversationItem.group(model.rows).contains(where: { $0.id == id }) {
                        pendingReadingRestore = saved
                        scroll.scrollTo(id, anchor: .top)
                    } else {
                        scroll.scrollTo("latest", anchor: .bottom)
                    }
                    hasInitialPosition = true
                    return
                }
                guard let target = historyRestore else { return }
                var transaction = Transaction()
                transaction.disablesAnimations = true
                withTransaction(transaction) {
                    scroll.scrollTo(target.id, anchor: target.anchor)
                    historyRestore = nil
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
        results = rows.compactMap { $0.tool?.generatedResults }
    }
    static func == (lhs: Self, rhs: Self) -> Bool { lhs.results == rhs.results }
    var body: some View {
        Group { if !outputs.isEmpty { ChatGeneratedOutputs(outputs: outputs) } }
            .task(id: results) {
                let captured = results
                let parsed = await Task.detached(priority: .utility) {
                    // This also applies to replayed rows that used to opt exec
                    // output into chat. Tool text belongs inside Activity.
                    var seen = Set<String>()
                    return captured.flatMap { ChatGeneratedOutput.parse(results: $0) }
                        .filter { seen.insert($0.id).inserted }
                }.value
                guard !Task.isCancelled else { return }
                outputs = parsed
            }
    }
}

private struct ChatScrollAnchors: ViewModifier {
    var initialAnchor: UnitPoint = .bottom
    func body(content: Content) -> some View {
        if #available(iOS 18.0, macOS 15.0, *) {
            // Lazy Markdown rows can acquire their height after ScrollViewReader's
            // first scrollTo. Let the scroll view place new chats itself,
            // while keyboard and history resizing continue to retain the top edge.
            content.defaultScrollAnchor(.top).defaultScrollAnchor(initialAnchor, for: .initialOffset)
        } else {
            content.defaultScrollAnchor(initialAnchor)
        }
    }
}

private struct ConversationRowFrames: PreferenceKey {
    static var defaultValue: [String: CGRect] { [:] }
    static func reduce(value: inout [String: CGRect], nextValue: () -> [String: CGRect]) {
        value.merge(nextValue()) { _, new in new }
    }
}

private struct ConversationContentPosition: Equatable {
    var firstID: String?
    var nearTop = false
    var atLatest = false
    var isMeasured = false
}

private struct ConversationContentFrame: PreferenceKey {
    static var defaultValue: ConversationContentPosition { ConversationContentPosition() }
    static func reduce(value: inout ConversationContentPosition, nextValue: () -> ConversationContentPosition) {
        let next = nextValue()
        if next.isMeasured { value = next }
    }
}

private struct ConversationActivityView: View {
    let item: ConversationItem
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var expanded = false
    private var failures: Int { item.activity.filter { $0.tool?.status == "Failed" }.count }
    private var title: String {
        guard item.isRunning else { return "Activity" }
        if let last = item.activity.last, last.role == "Tool", last.running { return last.tool?.title ?? "Working" }
        return item.activity.last?.role == "Thinking" ? "Thinking" : "Working"
    }
    var body: some View {
        VStack(spacing: 0) {
            Button {
                withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.16)) { expanded.toggle() }
            } label: {
                HStack(spacing: 10) {
                    Group {
                        if item.isRunning { ProgressView().controlSize(.mini) }
                        else { Image(systemName: failures > 0 ? "exclamationmark.circle" : "checkmark").font(.system(size: 12, weight: .medium)) }
                    }.frame(width: 18, height: 18)
                    Text(title).font(.system(size: 13, weight: .medium)).lineLimit(1)
                    Spacer(minLength: 4)
                    if failures > 0 { Text("\(failures) issue\(failures == 1 ? "" : "s")").foregroundStyle(Ink.amber).font(.caption) }
                    if !item.activity.isEmpty { Text("\(item.activity.count) step\(item.activity.count == 1 ? "" : "s")").font(.caption).monospacedDigit() }
                    Image(systemName: "chevron.right").font(.system(size: 10, weight: .semibold)).rotationEffect(.degrees(expanded ? 90 : 0))
                }.foregroundStyle(Ink.muted).padding(.horizontal, 13).frame(minHeight: 44).contentShape(Rectangle())
            }.buttonStyle(.plain).accessibilityIdentifier("activity-disclosure")
                .accessibilityLabel(title).accessibilityValue("\(item.activity.count) step\(item.activity.count == 1 ? "" : "s"), \(failures) issue\(failures == 1 ? "" : "s"), " + (expanded ? "Expanded" : "Collapsed"))
            if expanded {
                Rectangle().fill(Ink.border).frame(height: 0.5).padding(.horizontal, 13)
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        ForEach(item.activity) { row in
                            ConversationActivityStep(row: row, live: item.isRunning && row.running)
                        }
                        if item.activity.isEmpty { Text("Waiting for the first update…").font(.caption).foregroundStyle(Ink.muted).padding(12) }
                    }.padding(6)
                }.frame(maxHeight: 300).fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("activity-timeline")
            }
        }.background(Ink.surface.opacity(0.55), in: RoundedRectangle(cornerRadius: 14))
            .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(Ink.border.opacity(0.7), lineWidth: 0.5))
            .accessibilityElement(children: .contain).accessibilityIdentifier("activity-group")
    }
}

private struct ConversationActivityStep: View {
    let row: TranscriptRow
    let live: Bool
    @State private var expanded = false
    private var failed: Bool { row.tool?.status == "Failed" }
    private var title: String { row.tool?.title ?? (row.role == "Thinking" ? "Thinking" : "Progress update") }
    private var subject: String {
        if let subject = row.tool?.subject { return subject }
        let firstLine = String(row.text.split(whereSeparator: \.isNewline).first ?? "")
        let parsed = try? AttributedString(markdown: firstLine, options: .init(failurePolicy: .returnPartiallyParsedIfPossible))
        return parsed.map { String($0.characters) } ?? firstLine
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button { expanded.toggle() } label: {
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: failed ? "exclamationmark.circle" : row.role == "Tool" ? "terminal" : "text.alignleft")
                        .font(.system(size: 12)).frame(width: 18, height: 18).foregroundStyle(failed ? Ink.amber : Ink.muted)
                    VStack(alignment: .leading, spacing: 3) {
                        HStack(spacing: 8) {
                            Text(title).font(.system(size: 13, weight: .medium)).foregroundStyle(Ink.text).lineLimit(1)
                            if let status = row.tool?.status {
                                Text(status).font(.system(size: 11)).foregroundStyle(failed ? Ink.amber : Ink.muted)
                            }
                        }
                        if !subject.isEmpty { Text(subject).font(.system(size: 12)).foregroundStyle(Ink.muted).lineLimit(1) }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                    if live { ProgressView().controlSize(.mini) }
                    Image(systemName: "chevron.right").font(.system(size: 9, weight: .semibold))
                        .rotationEffect(.degrees(expanded ? 90 : 0)).foregroundStyle(Ink.muted).padding(.top, 3)
                }.padding(9).frame(minHeight: 44).contentShape(Rectangle())
            }.buttonStyle(.plain).accessibilityIdentifier("activity-step-" + row.id)
                .accessibilityValue(expanded ? "Expanded" : "Collapsed")
            if expanded {
                ScrollView {
                    Group {
                        if row.tool != nil { ToolActivityView(row: row) }
                        else if row.role == "Thinking" { ChatMarkdown(text: row.text) }
                        else { Text(row.text).font(.system(size: 14)).lineSpacing(4).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading) }
                    }.padding(12)
                }.frame(maxHeight: 240).fixedSize(horizontal: false, vertical: true)
                    .background(Ink.background.opacity(0.8), in: RoundedRectangle(cornerRadius: 9))
                    .padding(.leading, 28).padding(.horizontal, 6).padding(.bottom, 8)
                    .accessibilityIdentifier("activity-detail-" + row.id)
            }
        }
    }
}

private struct ToolActivityView: View {
    let row: TranscriptRow
    private var tool: ToolPresentation {
        if let tool = row.tool { return tool }
        var fallback = ToolPresentation(name: row.text, arguments: .null)
        if !row.running { fallback.finish(.string(row.detail)) }
        return fallback
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if !tool.input.isEmpty { fields(tool.input, heading: "Input") }
            if !tool.output.isEmpty { fields(tool.output, heading: "Result") }
        }.foregroundStyle(Ink.muted).accessibilityIdentifier("tool-activity")
    }
    private func fields(_ values: [ToolField], heading: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(heading).font(.system(size: 12, weight: .semibold)).foregroundStyle(Ink.muted)
            ForEach(Array(values.enumerated()), id: \.offset) { _, field in
                VStack(alignment: .leading, spacing: 3) {
                    if field.label != heading { Text(field.label).font(.system(size: 12)).foregroundStyle(Ink.muted) }
                    Text(field.value).font(.system(size: 14, design: field.code ? .monospaced : .default))
                        .foregroundStyle(Ink.text).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                }
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
}
