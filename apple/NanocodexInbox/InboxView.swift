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
    @State private var composerFocused = false

    private struct TabScrub {
        let ids: [String]
        var index: Int
        var translation: CGFloat = 0
        var remainder: CGFloat = 0
        var selectedID: String { ids[index] }
    }

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
        .fullScreenCover(isPresented: $showOverview) {
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
            GeometryReader { geometry in
                VStack(spacing: 0) {
                    if showScreens, let service = model.remoteService {
                        let available = geometry.size.height
                        let height = screenHeight(available: available, translation: screenResize)
                        RemoteDashboard(service: service, onClose: { showScreens = false })
                            .id(ObjectIdentifier(service)).frame(height: height).clipped()
                        screenDivider(available: available)
                    }
                    if let identity = model.focusedConversationIdentity {
                        ConversationView(model: model, identity: identity, readingPositions: readingPositions).id(identity)
                    } else { emptyState.frame(maxWidth: .infinity, maxHeight: .infinity) }
                }
            }
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
                browserToolbar
            }.background(Ink.background)
        }
    }

    private var browserTabs: some View {
        // Capture selection once. A lazy strip may evaluate distant tabs during
        // layout; resolving the focused card inside each label makes that work
        // quadratic in the number of conversations.
        let selectedID = model.deck.focusedID
        let highlightedID = tabScrub?.selectedID ?? selectedID
        let cards = model.tabCards
        return GeometryReader { geometry in
            let width = min(tabWidth, geometry.size.width)
            ScrollViewReader { scroll in
                ScrollView(.horizontal) {
                    LazyHStack(spacing: 6) {
                        ForEach(cards) { card in
                            Button { selectConversation(card.id) } label: {
                                HStack(spacing: 6) {
                                    Text(card.title).font(.subheadline.weight(highlightedID == card.id ? .semibold : .regular))
                                        .lineLimit(1).frame(maxWidth: width - 24)
                                }
                                .padding(.horizontal, 12).frame(height: tabHeight - 8)
                                .contentShape(Rectangle())
                            }
                            // Stable widths keep distant selections accurate in a lazy strip.
                            .frame(width: width, height: tabHeight).id(card.id)
                            .accessibilityLabel(card.title)
                            .accessibilityValue(card.status)
                            .accessibilityAddTraits(selectedID == card.id ? [.isSelected] : [])
                            .accessibilityIdentifier("browser-tab:" + card.id)
                        }
                    }
                }
                .scrollIndicators(.hidden)
                .onChange(of: highlightedID, initial: true) { _, id in
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

    private func screenHeight(available: CGFloat, translation: CGFloat = 0) -> CGFloat {
        let maximum = max(0, available - 100 - 24)
        return min(max(100, available * screenFraction + translation), maximum)
    }

    private func screenDivider(available: CGFloat) -> some View {
        Capsule().fill(Ink.muted.opacity(0.45)).frame(width: 36, height: 4)
            .frame(maxWidth: .infinity).frame(height: 24).contentShape(Rectangle())
            .gesture(DragGesture().updating($screenResize) { value, state, _ in state = value.translation.height }
                .onEnded { value in
                    let height = screenHeight(available: available, translation: value.translation.height)
                    screenFraction = min(0.75, max(0.25, height / max(1, available)))
                })
            .accessibilityElement().accessibilityLabel("Screen height")
            .accessibilityIdentifier("screen-pane-divider")
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
                Text(String(model.tabCards.count)).font(.system(size: 13, weight: .semibold)).monospacedDigit()
                    .frame(minWidth: 23, minHeight: 25)
                    .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(Ink.text, lineWidth: 1.5))
                    .frame(width: 44, height: 44)
            }
            .accessibilityLabel("Conversation overview").accessibilityValue("\(model.tabCards.count) active conversations")
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
                moveTabScrub(to: scrub.index + steps)
                tabScrubEdge = value.location.x < toolbarBounds.minX + 24 ? -1
                    : value.location.x > toolbarBounds.maxX - 24 ? 1 : 0
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
                Text("Scroll up to read earlier messages. Send queues a message; Steer now updates the current turn without stopping it. ⌘Return sends your message.").font(.caption)
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
    @State private var showingClosed = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var order: [String]

    init(model: InboxModel, select: @escaping (String) -> Void) {
        self.model = model
        self.select = select
        _order = State(initialValue: model.overviewCards.sorted(by: AgentCard.mostRecentFirst).map(\.id))
    }

    private var visibleCards: [AgentCard] {
        let text = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let source = showingClosed ? model.closedConversationCards : model.overviewCards
        let cards = Dictionary(uniqueKeysWithValues: source.map { ($0.id, $0) })
        let ordered = showingClosed ? source : order.compactMap { cards[$0] }
        return ordered.filter { card in
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
                        ConversationOverviewCard(model: model, card: card,
                            description: overviewDescription(card), canClose: !showingClosed,
                            select: { select(card.id) }, close: {
                                withAnimation(reduceMotion ? nil : .spring(response: 0.32, dampingFraction: 0.86)) {
                                    model.closeConversationTab(card.id)
                                }
                            })
                            .transition(reduceMotion ? .opacity : .scale(scale: 0.9).combined(with: .opacity))
                        .onAppear { model.setOverviewVisible(card.id, visible: true) }
                        .onDisappear { model.setOverviewVisible(card.id, visible: false) }
                    }
                }.padding(16)
                    .animation(reduceMotion ? nil : .spring(response: 0.32, dampingFraction: 0.86), value: visibleCards.map(\.id))
                if visibleCards.isEmpty {
                    ContentUnavailableView(model.cards.isEmpty ? "No conversations" : "No matching conversations", systemImage: "bubble.left.and.bubble.right",
                                           description: Text(model.cards.isEmpty ? "Use + to start a conversation." : "Try another search or load older conversations below."))
                }
                if !showingClosed && model.hasOlderConversations {
                    Button("Load older conversations") { model.loadOlderConversations() }
                        .buttonStyle(.bordered)
                        .padding(.bottom, 20)
                        .accessibilityIdentifier("load-older-conversations")
                }
            }.scrollDismissesKeyboard(.interactively).accessibilityIdentifier("conversation-overview")
            }
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search conversations")
            .background(Ink.background)
            .navigationTitle(showingClosed ? "Closed tabs" : "Conversations")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItemGroup(placement: .bottomBar) {
                    Menu {
                        Button(showingClosed ? "Show open tabs" : "Closed tabs", systemImage: "clock") {
                            showingClosed.toggle()
                        }
                    } label: { Text("More") }
                    .accessibilityIdentifier("overview-more")
                    Spacer()
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
        .onChange(of: model.overviewCards.map(\.id)) { _, ids in
            // Live history updates must not move another window under a tap.
            let available = Set(ids)
            order.removeAll { !available.contains($0) }
            let known = Set(order)
            order.append(contentsOf: ids.filter { !known.contains($0) })
        }
        .onDisappear { model.stopOverview() }
    }
}

private struct ConversationOverviewCard: View {
    @ObservedObject var model: InboxModel
    let card: AgentCard
    let description: String
    let canClose: Bool
    var select: () -> Void
    var close: () -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @GestureState private var swipe: CGFloat = 0

    private var selected: Bool { model.deck.focusedID == card.id }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: "bubble.left.fill")
                    .foregroundStyle(card.isRunning ? Color.green : Color.secondary)
                    .accessibilityHidden(true)
                Text(card.title).font(.subheadline.weight(.medium)).lineLimit(1)
                    .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                    .contentShape(Rectangle())
                    .onTapGesture(perform: select)
                    .accessibilityRepresentation { Button("Open " + card.title, action: select) }
                if canClose {
                    Button(action: close) {
                        Image(systemName: "xmark").font(.system(size: 12, weight: .medium))
                            .frame(width: 44, height: 44).contentShape(Rectangle())
                    }
                    .accessibilityLabel("Close " + card.title)
                    .accessibilityHint("Keeps the conversation and any running work")
                    .accessibilityIdentifier("overview-close:" + card.id)
                }
            }.padding(.leading, 12)
            let queue = model.queuePresentation(agentID: card.id, rows: model.overviewRows(for: card.id))
            ConversationMiniature(model: model, card: card, rows: queue.rows, pendingCount: queue.queuedMessages.count).equatable()
                    .frame(height: 230)
                    .frame(maxWidth: .infinity)
                    .background(Ink.background)
                    .contentShape(Rectangle())
                    // A tap recognizer fails when dragging; a plain Button can
                    // activate on release after a simultaneous swipe gesture.
                    .onTapGesture(perform: select)
                    .accessibilityRepresentation {
                        Button(card.title, action: select)
                            .accessibilityValue(description)
                            .accessibilityHint("Open conversation")
                            .accessibilityAddTraits(selected ? [.isSelected] : [])
                            .accessibilityIdentifier("overview-card:" + card.id)
                    }
        }
        .buttonStyle(.plain)
        .background(Ink.card)
        .clipShape(RoundedRectangle(cornerRadius: 18))
        .overlay {
            RoundedRectangle(cornerRadius: 18)
                .strokeBorder(selected ? Color.accentColor : Ink.border, lineWidth: selected ? 2 : 0.5)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("overview-preview:" + card.id)
        .offset(x: reduceMotion ? 0 : swipe)
        .opacity(1 - min(abs(swipe) / 300, 0.65))
        .simultaneousGesture(DragGesture(minimumDistance: 24)
            .updating($swipe) { value, offset, _ in
                guard canClose, abs(value.translation.width) > abs(value.translation.height) * 1.5 else { return }
                offset = value.translation.width
            }
            .onEnded { value in
                guard canClose, abs(value.translation.width) > 90,
                      abs(value.translation.width) > abs(value.translation.height) * 1.5 else { return }
                close()
            })
    }
}

private struct ConversationMiniature: View, Equatable {
    let model: InboxModel
    let card: AgentCard
    let rows: [TranscriptRow]
    let pendingCount: Int
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.card.id == rhs.card.id && lhs.card.activeTurns == rhs.card.activeTurns
            && lhs.card.error == rhs.card.error && lhs.card.checked == rhs.card.checked
            && lhs.card.turnCount == rhs.card.turnCount && lhs.rows == rhs.rows && lhs.pendingCount == rhs.pendingCount
    }
    private let scale: CGFloat = 0.48

    var body: some View {
        GeometryReader { viewport in
            let items = Array(ConversationItem.group(rows, activeTurns: card.activeTurns).suffix(4))
            VStack(alignment: .leading, spacing: 18) {
                if items.isEmpty && pendingCount == 0 {
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
                                if item.isRunning { ProgressView().controlSize(.mini) }
                                else {
                                    Image(systemName: "checkmark").font(.system(size: 12))
                                    Text("Activity").font(.system(size: 13, weight: .medium))
                                }
                                Spacer(minLength: 4)
                            }.foregroundStyle(Ink.muted).padding(13)
                                .background(Ink.surface, in: RoundedRectangle(cornerRadius: 14))
                            InboxGeneratedOutputView(rows: item.activity).equatable()
                        }
                    }
                }
                if pendingCount > 0 {
                    Label("\(pendingCount) pending message\(pendingCount == 1 ? "" : "s")", systemImage: "clock")
                        .font(.system(size: 14)).foregroundStyle(Ink.muted)
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
    @Binding var focused: Bool
    var onVoiceChat: @MainActor () -> Void = {}
    @State private var showExpandedEditor = false
    @State private var composerOverflows = false
    @State private var showPhotos = false
    @State private var showFiles = false
    @State private var selectedPhotos: [PhotosPickerItem] = []
    @State private var photoTarget: InboxModel.AttachmentTarget?
    @State private var fileTarget: InboxModel.AttachmentTarget?
    @State private var pickerError: String?
    @State private var queueContentHeight: CGFloat = 64
    #if os(iOS)
    @State private var showCamera = false
    @State private var cameraTarget: InboxModel.AttachmentTarget?
    @State private var cameraPermissionDenied = false
    #endif

    private var sendShowsStop: Bool {
        model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && model.focusedAttachments.isEmpty && !model.preparingAttachments
            && !model.stopTarget.isEmpty
    }
    private var stopRequest: PendingTurnCancellation? {
        model.focused.flatMap { model.cancellation(agentID: $0.id, turnID: model.stopTarget) }
    }

    var body: some View {
        let queue = model.focusedQueue
        let visiblePending = queue.queuedMessages
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
                                } else if model.steeringTarget(message) != nil {
                                    Button { model.steerNow(message.id) } label: { Text("Steer now").frame(minHeight: 44) }.accessibilityIdentifier("steer-now")
                                        .accessibilityHint("Sends this message into the current turn without stopping it")
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
            if !model.focusedAttachments.isEmpty {
                ScrollView(.horizontal) {
                    HStack(spacing: 10) {
                        ForEach(model.focusedAttachments) { attachment in
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
                ChatComposerEditor(text: $model.draft, focused: $focused, overflowing: $composerOverflows)
                    .accessibilityIdentifier("composer")
                    .overlay(alignment: .topLeading) {
                        if model.draft.isEmpty {
                            Text("Ask Nanocodex").font(.body).foregroundStyle(.tertiary)
                                .padding(.top, 8).allowsHitTesting(false).accessibilityHidden(true)
                        }
                    }
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
                .padding(.horizontal, 8).padding(.bottom, 8).padding(.top, visiblePending.isEmpty ? 8 : 0)
                .padding(.leading, 6).accessibilityElement(children: .contain).accessibilityIdentifier("composer-input")

        }.background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 28))
            .overlay(RoundedRectangle(cornerRadius: 28).strokeBorder(Color.primary.opacity(focused ? 0.18 : 0.1)))
            .shadow(color: .black.opacity(0.035), radius: 8, y: 2)
            .padding(.horizontal, 12).padding(.top, 6).padding(.bottom, 8).background(Ink.background)
            .onChange(of: model.focusedConversationIdentity) { _, _ in
                showExpandedEditor = false
                focused = false
            }
            .sheet(isPresented: $showExpandedEditor) {
                ExpandedAgentComposer(
                    draft: $model.draft,
                    canSend: model.canSend,
                    attachmentCount: model.focusedAttachments.count,
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

private struct ExpandedAgentComposer: View {
    @Binding var draft: String
    let canSend: Bool
    let attachmentCount: Int
    let onCollapse: () -> Void
    let onSend: () -> Void
    @FocusState private var editorFocused: Bool

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 8) {
                TextEditor(text: $draft)
                    .font(.body)
                    .scrollContentBackground(.hidden)
                    .focused($editorFocused)
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
    @State private var error: String?
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ChatMediaPreview(title: attachment.name, load: { [try await model.downloadAttachment(attachment, agentID: agentID)] }) {
                AttachmentImageView(source: preview.map(AttachmentImageSource.data), contentMode: .fit)
                    .frame(maxWidth: 240).frame(height: 180)
                    .accessibilityLabel("Open " + attachment.name).accessibilityIdentifier("message-image")
            }
            if let error { Text(error).font(.caption).foregroundStyle(.secondary) }
        }.task(id: attachment.id) {
            do { preview = try await model.attachmentPreview(attachment, agentID: agentID) }
            catch is CancellationError { }
            catch { self.error = error.localizedDescription }
        }
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
                    DisclosureGroup {
                        ChatMarkdown(text: row.text)
                    } label: {
                        if row.running { ProgressView().controlSize(.mini).accessibilityLabel("Thinking") }
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
                if !row.detail.isEmpty { Text(row.detail).font(.caption).foregroundStyle(Ink.muted) }
                if let delivery, delivery.phase == .failed {
                    Label("Couldn’t confirm delivery", systemImage: "exclamationmark.circle").font(.caption).foregroundStyle(.secondary)
                    HStack {
                        Button("Retry") { model.retryPending(delivery.id) }.accessibilityIdentifier("retry-pending")
                        Button("Cancel") { model.cancelPending(delivery.id) }.accessibilityLabel("Cancel message")
                    }.font(.caption).disabled(!canRetry)
                }
                if let transfer = steering, canWithdraw, (transfer.wasAccepted || transfer.phase == .unconfirmed), transfer.phase != .withdrawn {
                    Button(transfer.phase == .withdrawing ? "Withdrawing…" : "Withdraw steering") { model.withdrawSteering(transfer.id) }
                        .font(.caption).accessibilityIdentifier("withdraw-steering")
                        .disabled(!model.connected || (transfer.phase == .withdrawing && transfer.error == nil))
                }
                if let images = row.images {
                    ForEach(Array(images.enumerated()), id: \.offset) { _, image in
                        ChatImageAttachment(source: image).frame(maxWidth: 240)
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
        var childID: String? = nil
    }
    var values: [String: Position] = [:]
}

private struct ConversationRenderedItem: Identifiable, Equatable {
    var id: String
    var content: ConversationItem?
    var output: ChatGeneratedOutput?
    var sourceRowID: String?
    var message: TranscriptRow? { content?.message }
    static func project(_ groups: [ConversationItem], outputs: [String: [ChatGeneratedOutput]]) -> [Self] {
        var result: [Self] = []
        for group in groups {
            result.append(Self(id: group.id, content: group))
            var seen = Set<String>()
            for row in group.activity {
                for output in outputs[row.id] ?? [] where seen.insert(output.id).inserted {
                    result.append(Self(id: group.id + ":output:" + output.id, output: output, sourceRowID: row.id))
                }
            }
        }
        return result
    }
}

// Use measured row heights; decoding follows native viewport visibility.
private struct ConversationOutputView: View {
    let output: ChatGeneratedOutput
    @State private var visible = false
    var body: some View {
        ChatGeneratedOutputView(output: output, loadsThumbnail: visible)
            .onScrollVisibilityChange(threshold: 0.01) { visible = $0 }
    }
}

private struct ConversationView: View {
    @ObservedObject var model: InboxModel
    let identity: String
    let readingPositions: ConversationReadingPositions

    var body: some View {
        let queue = model.focusedQueue
        let turns = model.focused?.activeTurns ?? []
        let items = ConversationRenderedItem.project(ConversationItem.group(queue.rows, activeTurns: turns), outputs: model.generatedOutputsByRow)
        ConversationContentView(model: model,
                                identity: identity, readingPositions: readingPositions,
                                revision: .init(rows: queue.rows, items: items, itemsByID: Dictionary(uniqueKeysWithValues: items.map { ($0.id, $0) }), pending: queue.messages,
                                                title: model.focused?.title ?? "Conversation",
                                                activeTurns: model.focused?.activeTurns ?? [],
                                                loading: model.threadLoading, error: model.threadError,
                                                hasOlder: model.hasOlder, loadingOlder: model.loadingOlder,
                                                hasNewer: model.hasNewer, loadingNewer: model.loadingNewer))
    }
}

// Scroll and reading-position state must invalidate this view independently
// of transcript revisions. Keep equality boundaries on rendered messages only.
private struct ConversationContentView: View {
    struct Revision: Equatable {
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
    private let verticalPadding: CGFloat = 24
    let model: InboxModel
    let identity: String
    let readingPositions: ConversationReadingPositions
    let revision: Revision
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var followsLatest = true
    @State private var isInteractingTranscript = false
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
    @State private var historyRequestFirstID: String?
    private func rememberHistoryPosition(in viewport: GeometryProxy) {
        let visible = rowGeometry.frames.filter { revision.itemsByID[$0.key] != nil && $0.value.maxY > 0 && $0.value.minY < viewport.size.height }
        let sourceRows = visible.keys.compactMap { revision.itemsByID[$0]?.sourceRowID }
        model.protectHistoryRows(Set(visible.keys).union(sourceRows))
        // Collapsed Activity labels are not media reading anchors. Expanded
        // timelines retain the visible step when earlier work arrives.
        let stable = visible.filter {
            guard let item = revision.itemsByID[$0.key] else { return false }
            return item.output != nil || item.message != nil || rowGeometry.expandedActivity.contains(item.id)
        }
        guard let first = (stable.isEmpty ? visible : stable).min(by: { $0.value.minY < $1.value.minY }) else { return }
        if rowGeometry.expandedActivity.contains(first.key),
           let group = revision.itemsByID[first.key]?.content,
           let timeline = rowGeometry.frames[first.key + ":timeline"],
           let child = group.activity.compactMap({ row -> (String, CGRect)? in
               guard let frame = rowGeometry.frames[row.id], frame.intersects(timeline),
                     frame.maxY > 0, frame.minY < viewport.size.height else { return nil }
               return (row.id, frame)
           }).min(by: { $0.1.minY < $1.1.minY }) {
            historyRestore = (first.key, child.1.minY, child.0)
        } else { historyRestore = (first.key, first.value.minY, nil) }
    }
    private func loadHistory(_ direction: HistoryDirection, in viewport: GeometryProxy) {
        guard model.focusedConversationIdentity == identity, pendingReadingRestore == nil,
              historyReady, !model.threadLoading, !model.loadingOlder, !model.loadingNewer,
              !historyRequestInFlight, direction == .older ? model.hasOlder : model.hasNewer else { return }
        historyRequestInFlight = true
        historyDirection = nil
        historyRequestFirstID = model.rows.first?.id
        rememberHistoryPosition(in: viewport)
        Task {
            if direction == .older { await model.loadOlder() }
            else { await model.loadNewer() }
            guard model.focusedConversationIdentity == identity else { return }
            if model.rows.first?.id == historyRequestFirstID { historyRestore = nil }
            historyRequestInFlight = false
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
        model.setHistoryAtLatest(historyContent.atLatest)
        if historyDirection == .older, historyContent.approachingTop { model.prefetchOlder() }
        // Layout changes also cross these thresholds. Only the reader's chosen
        // direction may load a page, so trimming cannot undo their navigation.
        if historyDirection == .older, historyContent.nearTop { loadHistory(.older, in: viewport) }
        else if historyDirection == .newer, historyContent.atLatest { loadHistory(.newer, in: viewport) }
    }
    private func saveReadingPosition(in viewport: GeometryProxy) {
        guard model.focusedConversationIdentity == identity, hasInitialPosition,
              pendingReadingRestore == nil, historyReady, historyContent.isMeasured,
              !historyRequestInFlight else { return }
        if followsLatest && !model.hasNewer {
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
    var body: some View {
        ScrollViewReader { scroll in
            GeometryReader { viewport in
            ZStack(alignment: .top) {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    if revision.rows.isEmpty, revision.pending.isEmpty, !model.threadLoading, model.threadError == nil {
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
                    ForEach(revision.items) { item in
                        VStack(alignment: .leading, spacing: 0) {
                        if let row = item.message {
                        ConversationMessageView(row: row, model: model, agentID: model.focused?.id ?? "")
                        } else if let output = item.output {
                            ConversationOutputView(output: output)
                        } else if let content = item.content {
                            ConversationActivityView(item: content, onExpansion: { expanded in
                                if expanded { rowGeometry.expandedActivity.insert(content.id) }
                                else {
                                    rowGeometry.expandedActivity.remove(content.id)
                                    if pendingReadingRestore?.rowID == content.id { pendingReadingRestore = nil }
                                }
                            }, onInteraction: { pendingReadingRestore = nil })
                        }
                        }.id(item.id)
                            .background(GeometryReader { geometry in
                                Color.clear.preference(key: ConversationRowFrames.self, value: [item.id: geometry.frame(in: .named("conversation-viewport"))])
                            })
                            .accessibilityElement(children: .contain)
                            .accessibilityIdentifier((item.message?.role == "You" ? "message-user-" : item.message?.role == "Agent" ? "message-assistant-" : "message-") + item.id)
                    }

                    if let agentID = model.focused?.id {
                        NanocodexVoiceTranscript(session: model.voice, conversationID: agentID, durableRows: model.rows) {
                            if followsLatest { scroll.scrollTo("latest", anchor: .bottom) }
                        }
                    }
                    if model.hasNewer {
                        Button("Load newer messages") {
                            historyDirection = .newer
                            loadHistory(.newer, in: viewport)
                        }.disabled(model.loadingNewer).accessibilityIdentifier("load-newer")
                    }
                    Color.clear.frame(height: 1).id("latest")
                }.padding(.horizontal, 20).padding(.vertical, verticalPadding).frame(maxWidth: 780).frame(maxWidth: .infinity)

            }
            .defaultScrollAnchor(.top)
            .defaultScrollAnchor(followsLatest ? .bottom : .top, for: .sizeChanges)
            .defaultScrollAnchor(readingPositions.values[identity]?.atLatest == false ? .top : .bottom, for: .initialOffset)
            .scrollDismissesKeyboard(.interactively)
            .scrollBounceBehavior(.always, axes: .vertical)
            .coordinateSpace(name: "conversation-viewport")
            .onPreferenceChange(ConversationRowFrames.self) { frames in
                rowGeometry.frames = frames
                if let target = pendingReadingRestore, let id = target.rowID, let parent = frames[id] {
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
                // Continue following the reader during a slow history request,
                // until the insertion changes the coordinate space.
                if historyRequestInFlight, model.rows.first?.id == historyRequestFirstID {
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
                if phase == .tracking {
                    pendingReadingRestore = nil
                    scrollsTowardLatest = false
                }
                isInteractingTranscript = phase == .interacting
                if phase == .interacting || phase == .decelerating { followsLatest = false }
                else if phase == .idle, previous == .interacting || previous == .decelerating {
                    followsLatest = scrollsTowardLatest && historyContent.atLatest && !model.hasNewer
                }
            }
            .onChange(of: hasInitialPosition) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: model.hasOlder) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: model.threadLoading) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: model.loadingOlder) { _, _ in updateHistoryPosition(in: viewport) }
            .onScrollGeometryChange(for: CGFloat.self) { $0.contentOffset.y } action: { previous, offset in
                // Deceleration, bounce-back and restoration change offsets too.
                // Only direct interaction establishes a new paging direction.
                // A prefetched page can arrive while the finger is still down.
                // Its inserted height is not a reversal of the reader's swipe.
                guard isInteractingTranscript, previous != offset, !historyRequestInFlight else { return }
                pendingReadingRestore = nil
                scrollsTowardLatest = offset > previous
                historyDirection = scrollsTowardLatest ? .newer : .older
                if hasInitialPosition { historyReady = true }
                updateHistoryPosition(in: viewport)
            }
            .background(Ink.background)
            .accessibilityLabel(revision.title)
            .accessibilityIdentifier("conversation")
            .overlay(alignment: .bottom) {
                if historyContent.isMeasured, !model.threadLoading, model.hasNewer || !historyContent.atLatest {
                    Button {
                        historyDirection = nil
                        historyRestore = nil
                        pendingReadingRestore = nil
                        Task {
                            if model.hasNewer { await model.loadNewer(latest: true) }
                            guard model.focusedConversationIdentity == identity, !model.hasNewer else { return }
                            followsLatest = true
                            withAnimation(reduceMotion ? nil : .easeOut(duration: 0.2)) {
                                scroll.scrollTo("latest", anchor: .bottom)
                            }
                        }
                    } label: {
                        Label("Latest messages", systemImage: "arrow.down")
                            .labelStyle(.iconOnly)
                            .frame(minWidth: 28, minHeight: 28)
                    }
                    .buttonStyle(.bordered)
                    .buttonBorderShape(.circle)
                    .tint(.primary)
                    .padding(.bottom, 8)
                    .disabled(model.loadingNewer || model.loadingOlder)
                    .accessibilityLabel("Latest messages")
                    .accessibilityHint("Scroll to the latest message and follow new responses")
                    .accessibilityIdentifier("latest-messages")
                }
            }
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
                guard let target = historyRestore else { return }
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

// Geometry is reading-position bookkeeping, not rendered state. Updating each
// pixel must not invalidate the conversation's SwiftUI body.
private final class ConversationRowGeometry {
    var frames: [String: CGRect] = [:]
    var expandedActivity = Set<String>()
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

private struct ConversationActivityView: View {
    let item: ConversationItem
    var onExpansion: (Bool) -> Void = { _ in }
    var onInteraction: () -> Void = {}
    @State private var expanded = false
    @State private var visibleStep: String?
    private var failures: Int { item.activity.filter { $0.tool?.status == "Failed" }.count }
    var body: some View {
        Group {
            if item.activity.isEmpty {
                ProgressView().accessibilityLabel("Activity")
            } else {
                DisclosureGroup(isExpanded: $expanded) {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 8) {
                            ForEach(item.activity) { row in
                                ConversationActivityStep(row: row, live: item.isRunning && row.running)
                                    .id(row.id)
                                    .background(GeometryReader { geometry in
                                        Color.clear.preference(key: ConversationRowFrames.self,
                                            value: [row.id: geometry.frame(in: .named("conversation-viewport"))])
                                    })
                            }
                        }.scrollTargetLayout().padding(.vertical, 8)
                    }.scrollPosition(id: $visibleStep, anchor: .top)
                        .frame(maxHeight: 300).fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier("activity-timeline")
                        .background(GeometryReader { geometry in
                            Color.clear.preference(key: ConversationRowFrames.self,
                                value: [item.id + ":timeline": geometry.frame(in: .named("conversation-viewport"))])
                        })
                        .onScrollPhaseChange { _, phase in
                            if phase == .tracking || phase == .interacting { onInteraction() }
                        }
                } label: {
                    HStack(spacing: 10) {
                        if item.isRunning { ProgressView().controlSize(.mini) }
                        Text("Activity").font(.subheadline)
                    }.frame(minHeight: 44).foregroundStyle(Ink.muted)
                }.tint(Ink.muted)
                    .accessibilityIdentifier("activity-disclosure")
                    .accessibilityLabel("Activity")
                    .accessibilityHint("\(item.activity.count) steps, \(failures) issues")
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .contain).accessibilityIdentifier("activity-group")
            .onChange(of: expanded) { _, value in onExpansion(value) }
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
        let summary = subject
        DisclosureGroup(isExpanded: $expanded) {
            ScrollView {
                Group {
                    if row.tool != nil { ToolActivityView(row: row) }
                    else if row.role == "Thinking" { ChatMarkdown(text: row.text) }
                    else { Text(row.text).font(.body).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading) }
                }.padding(.vertical, 8)
            }.frame(maxHeight: 240).fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("activity-detail-" + row.id)
        } label: {
            HStack(spacing: 10) {
                Image(systemName: failed ? "exclamationmark.circle" : row.role == "Tool" ? "terminal" : "text.alignleft")
                    .foregroundStyle(failed ? Ink.amber : Ink.muted)
                VStack(alignment: .leading, spacing: 3) {
                    Text(title).font(.subheadline).foregroundStyle(Ink.text).lineLimit(1)
                    if !summary.isEmpty { Text(summary).font(.caption).foregroundStyle(Ink.muted).lineLimit(1) }
                }.frame(maxWidth: .infinity, alignment: .leading)
                if live { ProgressView().controlSize(.mini) }
                else if failed { Text("Failed").font(.caption).foregroundStyle(Ink.amber) }
            }.frame(minHeight: 44)
        }.tint(Ink.muted).accessibilityIdentifier("activity-step-" + row.id)
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
