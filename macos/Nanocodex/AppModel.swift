import AppKit
import SwiftUI
import InboxCore
import NanocodexVoice
import NanocodexRemote

@MainActor
final class AppModel: ObservableObject {
    enum Screen { case chat, hands }
    enum WorkspaceFocus { case navigation, writing }
    @Published var state = DesktopState() {
        didSet { updateBackgroundActivity() }
    }
    @Published var keepMacAwake = false {
        didSet {
            backgroundPreferences?.set(keepMacAwake, forKey: "keepMacAwakeWhileHandsRunning")
            updateBackgroundActivity()
        }
    }
    @Published private(set) var runtimeFailed = false
    let backgroundActivity = HandBackgroundActivity()
    let launchAtLogin: LaunchAtLogin
    private let backgroundPreferences: UserDefaults?
    private var backgroundActivityStopped = false
    @Published var tabs: [WorkspaceTab] = [WorkspaceTab()]
    @Published var activeTabID = "" {
        didSet { if oldValue != activeTabID { enterNavigation() } }
    }
    @Published var tabPosition = "left"
    @Published var theme = "system"
    @Published var workspaceMode = "single"
    @Published var tiledTabIDs: [String] = []
    @Published var showingPanePicker = false
    @Published var paneWidth = 0.0
    @Published private(set) var editorFocusRequest = 0
    @Published private(set) var requestedEditorTabID: String?
    @Published private(set) var workspaceFocus: WorkspaceFocus = .navigation
    @Published private(set) var navigationFocusRequest = 1
    @Published var workspaceFilter: WorkspaceFilter = .inbox
    @Published var screen: Screen = .chat
    @Published var snapshots: [String: ThreadSnapshot] = [:]
    @Published private(set) var threadErrors: [String: String] = [:]
    @Published var messages: [String: [MessageEntry]] = [:]
    @Published var pending: [PendingMessage] = []
    @Published private(set) var busyMessages = Set<String>()
    @Published var loading = Set<String>()
    @Published var busyHands = Set<String>()
    @Published var isStarting = true
    @Published var error: String?
    @Published var showingSettings = false
    @Published var showingSearch = false
    @Published var showingHandSetup = false
    @Published var showingRemoteSetup = false
    @Published var showingScreens = false
    @Published private(set) var remoteService: RemoteService?
    private(set) var remoteMacHost = RemoteMacHost()
    private(set) var remotePhoneHost = RemoteMacHost()
    @Published var editingHand: Hand?
    @Published var settings = AgentSettings()
    @Published var selectedHandForLogs: Hand?
    @Published private(set) var phoneSignInActive = false
    @Published private(set) var phoneSignInStartedConnected = false
    @Published private(set) var phoneSignInChallenge: SignInChallenge?
    let runtime: RuntimeClient
    let voice = VoiceSession()
    @Published private var preparingVoiceTabID: String?
    private var closedTabs: [WorkspaceTab] = []
    private var generation = 0
    private var restoredLayout = false
    private var defaultHandConnection: Task<Void, Never>?
    private var accountHandDiscovery: Task<Void, Never>?
    private var persistence: Task<Void, Never>?
    private var observation = Set<String>()
    private var didStart = false
    private let isolatedSession: Bool
    private var currentCredential: AccountKeychain.Credential? {
        didSet {
            resetRemoteSharing()
            if let credential = currentCredential, let origin = URL(string: credential.baseUrl) {
                remoteService = try? RemoteService(origin: origin) { request in
                    request.setValue("Bearer " + credential.apiKey, forHTTPHeaderField: "Authorization")
                }
                configureAutomaticScreenSharing()
            }
        }
    }
    private var signInPreviousCredential: AccountKeychain.Credential?
    private var signInPreviousSavedCredential: AccountKeychain.Credential?
    private var signInChangedAccount = false
    private var signInCommitted = false
    private var signInSavedInKeychain = false
    private var accountTransition = false
    private var inboxOrder: [String] = []
    private var reviewEvents: [String: [ManagedEvent]] = [:]
    private var timelineProjections: [String: TimelineProjection] = [:]
    private var pinnedPaneID: String?
    struct ReadingPosition { var anchor: String?; var followsOutput: Bool; var offset: CGFloat? = nil }
    var readingPositions: [String: ReadingPosition] = [:]
    @Published var expandedMessages: [String: Set<String>] = [:]

    private func requestEditorFocus(_ id: String) { workspaceFocus = .writing; requestedEditorTabID = id; editorFocusRequest += 1 }
    func enterNavigation() {
        requestedEditorTabID = nil; workspaceFocus = .navigation; navigationFocusRequest += 1
    }
    func focusComposer() {
        guard tab(activeTabID) != nil, !canvasTabs.isEmpty else { return }
        requestEditorFocus(activeTabID)
    }
    func composerFocused(_ id: String) { select(id); workspaceFocus = .writing }
    func tab(_ id: String? = nil) -> WorkspaceTab? { tabs.first { $0.id == (id ?? activeTabID) } }
    func snapshot(_ id: String? = nil) -> ThreadSnapshot? { tab(id)?.threadId.flatMap { snapshots[$0] } }
    func threadError(_ id: String? = nil) -> String? { tab(id)?.threadId.flatMap { threadErrors[$0] } }
    func transcript(_ id: String? = nil) -> [MessageEntry] { tab(id)?.threadId.flatMap { messages[$0] } ?? [] }
    func pendingMessages(_ id: String? = nil) -> [PendingMessage] {
        guard let tab = tab(id) else { return [] }
        return pending.filter { $0.agentID != nil ? $0.agentID == tab.threadId : $0.tabID == tab.id }
    }
    func controllableTurns(_ id: String? = nil) -> [String] {
        let queued = Set(pendingMessages(id).map(\.id))
        return (snapshot(id)?.activeTurns ?? []).filter { !queued.contains($0) }
    }
    func isBusy(_ id: String? = nil) -> Bool { pendingMessages(id).contains { busyMessages.contains($0.id) } }
    func canSend(_ id: String? = nil) -> Bool {
        let ready = tab(id)?.threadId.map { snapshots[$0] != nil && threadErrors[$0] == nil } ?? true
        return state.connected && !accountTransition && ready && preparingVoiceTabID != (id ?? activeTabID) && !isBusy(id) && !pendingMessages(id).contains { $0.phase == .failed }
    }
    func displayedTranscript(_ id: String? = nil) -> [MessageEntry] {
        let queued = pendingMessages(id)
        let waiting = Set(queued.filter { !$0.predecessor.isEmpty }.map(\.id))
        let rows = transcript(id).filter { !waiting.contains($0.turnId) }.flatMap { entry -> [MessageEntry] in
            guard entry.kind == .user || entry.kind == .assistant,
                  let spoken = RealtimeTranscript.project(entry.text) else { return [entry] }
            return spoken.enumerated().map { index, turn in
                MessageEntry(id: entry.id + ":voice:\(index)", turnId: entry.turnId,
                             kind: turn.speaker == "user" ? .user : .assistant, text: turn.text,
                             streaming: entry.streaming, cursor: entry.cursor)
            }
        }
        let known = Set(rows.map(\.id))
        return rows + queued.filter { $0.predecessor.isEmpty && !known.contains(MessageEntry.userID($0.id)) }.map {
            .init(id: MessageEntry.userID($0.id), turnId: $0.id, kind: .user, text: $0.text)
        }
    }
    func voiceTranscriptRows(_ id: String? = nil) -> [TranscriptRow] {
        displayedTranscript(id).map { entry in
            var row = TranscriptRow(id: entry.id, role: entry.kind == .user ? "You" : "Agent", text: entry.text)
            row.cursor = entry.cursor.flatMap { Cursor(rawValue: $0) }
            return row
        }
    }
    func running(_ id: String? = nil) -> Bool { !(snapshot(id)?.activeTurns.isEmpty ?? true) }
    func working(_ id: String? = nil) -> Bool { running(id) || pendingMessages(id).contains { $0.phase != .failed } }
    func hasDraft(_ id: String? = nil) -> Bool { !(tab(id)?.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true) }
    func handsForTab(_ id: String? = nil) -> [Hand] { connectedHands.filter { $0.agentId == nil || $0.agentId == tab(id)?.threadId } }
    func settingsForTab(_ id: String) -> AgentSettings { snapshot(id)?.settings ?? tab(id)?.draftSettings ?? AgentSettings() }
    func update(for tab: WorkspaceTab) -> WorkspaceUpdate {
        let snapshot = tab.threadId.flatMap { snapshots[$0] }
        let events = tab.threadId.flatMap { reviewEvents[$0] } ?? snapshot?.events ?? []
        let terminal = events.last { ["turn_completed", "turn_failed", "turn_cancelled"].contains($0.data["type"].string) }
        let cursor = events.last?.cursor ?? "0"
        return WorkspaceUpdate(cursor: cursor, running: working(tab.id),
                               checked: snapshot != nil, failed: terminal?.data["type"].string == "turn_failed" || pendingMessages(tab.id).contains { $0.phase == .failed },
                               completed: terminal?.data["type"].string == "turn_completed")
    }
    func matchesFilter(_ tab: WorkspaceTab) -> Bool {
        switch workspaceFilter {
        case .all: return true
        case .running: return update(for: tab).running
        case .inbox: return update(for: tab).isInInbox(tab)
        }
    }
    var visibleTabs: [WorkspaceTab] {
        let visible = tabs.filter { matchesFilter($0) || $0.id == pinnedPaneID }
        guard workspaceFilter == .inbox else { return visible }
        return visible.sorted {
            (inboxOrder.firstIndex(of: $0.id) ?? tabs.firstIndex(of: $0) ?? 0) <
            (inboxOrder.firstIndex(of: $1.id) ?? tabs.firstIndex(of: $1) ?? 0)
        }
    }
    var isTiled: Bool { workspaceMode == "tiles" && tiledTabIDs.count > 1 }
    var canvasTabs: [WorkspaceTab] {
        if isTiled { return tiledTabIDs.compactMap { tab($0) } }
        return (visibleTabs.first { $0.id == activeTabID } ?? visibleTabs.first).map { [$0] } ?? []
    }
    func openBeside(_ id: String) {
        showingPanePicker = false
        guard tab(id) != nil else { return }
        guard let current = canvasTabs.first(where: { $0.id == activeTabID }) ?? canvasTabs.first, current.id != id else { select(id); return }
        if !isTiled { tiledTabIDs = [current.id] }
        if !tiledTabIDs.contains(id) {
            tiledTabIDs.insert(id, at: min((tiledTabIDs.firstIndex(of: activeTabID) ?? 0) + 1, tiledTabIDs.count))
        }
        workspaceMode = "tiles"; showingPanePicker = false
        select(id); requestEditorFocus(id); persistLayout()
    }
    func removePane(_ id: String) {
        guard let index = tiledTabIDs.firstIndex(of: id) else { return }
        tiledTabIDs.remove(at: index)
        if activeTabID == id { activeTabID = tiledTabIDs.isEmpty ? visibleTabs.first?.id ?? "" : tiledTabIDs[min(index, tiledTabIDs.count - 1)] }
        if tiledTabIDs.count < 2 { workspaceMode = "single" }
        pinnedPaneID = activeTabID.isEmpty ? nil : activeTabID
        persistLayout()
    }
    func focusOnly(_ id: String) { select(id); workspaceMode = "single"; persistLayout() }
    var attentionCount: Int { tabs.filter { update(for: $0).needsAttention($0) }.count }
    var runningCount: Int { tabs.filter { update(for: $0).running }.count }
    func setFilter(_ filter: WorkspaceFilter) {
        enterNavigation()
        workspaceFilter = filter; screen = .chat; pinnedPaneID = nil; workspaceMode = "single"
        // Rank only on explicit navigation, so arriving output cannot move tiles.
        inboxOrder = tabs.filter { update(for: $0).needsAttention($0) }.map(\.id) + tabs.filter { !update(for: $0).needsAttention($0) }.map(\.id)
        if !tabs.contains(where: { $0.id == activeTabID && matchesFilter($0) }) {
            activeTabID = visibleTabs.first(where: matchesFilter)?.id ?? ""
        }
        pinnedPaneID = activeTabID.isEmpty ? nil : activeTabID
        persistLayout()
    }
    func review(_ id: String, seen: Bool) {
        guard let tab = tab(id) else { return }
        let order = visibleTabs.map(\.id), index = order.firstIndex(of: id) ?? 0
        let cursor = update(for: tab).cursor
        updateTab(tabID: id) { if seen { $0.seenCursor = cursor }; $0.deferredCursor = cursor }
        let wasActive = activeTabID == id
        if workspaceFilter == .inbox, isTiled { removePane(id) }
        if pinnedPaneID == id { pinnedPaneID = nil }
        if workspaceFilter == .inbox, wasActive {
            let remaining = order.filter { candidate in candidate != id && self.tab(candidate).map(matchesFilter) == true }
            activeTabID = remaining.isEmpty ? "" : remaining[min(index, remaining.count - 1)]
            pinnedPaneID = activeTabID.isEmpty ? nil : activeTabID
        }
        persistLayout()
    }
    func cyclePane(_ offset: Int, focusEditor: Bool? = nil) {
        let writing = focusEditor ?? (workspaceFocus == .writing)
        let ids = (isTiled ? canvasTabs : visibleTabs).map(\.id)
        guard !ids.isEmpty else { return }
        let index = ids.firstIndex(of: activeTabID) ?? 0
        let next = min(ids.count - 1, max(0, index + offset))
        guard next != index else { return }
        select(ids[next])
        if writing { requestEditorFocus(ids[next]) }
    }
    func movePane(_ offset: Int) {
        if isTiled {
            guard let index = tiledTabIDs.firstIndex(of: activeTabID), tiledTabIDs.indices.contains(index + offset) else { return }
            tiledTabIDs.swapAt(index, index + offset); persistLayout(); return
        }
        guard let index = tabs.firstIndex(where: { $0.id == activeTabID }) else { return }
        let destination = index + offset
        guard tabs.indices.contains(destination) else { return }
        tabs.swapAt(index, destination); inboxOrder = tabs.map(\.id); persistLayout()
    }
    func resizePanes(_ width: Double) { paneWidth = min(880, max(420, width)); persistLayout() }
    func toggleFocusMode() {
        if isTiled { workspaceMode = "single" }
        else if tiledTabIDs.count > 1 {
            if tab(activeTabID) == nil { activeTabID = tiledTabIDs[0] }
            workspaceMode = "tiles"; if !tiledTabIDs.contains(activeTabID) { tiledTabIDs[0] = activeTabID }
        }
        else { showingPanePicker = true }
        persistLayout()
    }

    var activeTab: WorkspaceTab? { tabs.first { $0.id == activeTabID } }
    var activeSnapshot: ThreadSnapshot? { activeTab?.threadId.flatMap { snapshots[$0] } }
    var activeMessages: [MessageEntry] { activeTab?.threadId.flatMap { messages[$0] } ?? [] }
    var isRunning: Bool { !(activeSnapshot?.activeTurns.isEmpty ?? true) }
    var hasUnsentMessage: Bool { !(activeTab?.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true) }
    var preferredColorScheme: ColorScheme? { theme == "dark" ? .dark : theme == "light" ? .light : nil }
    var connectedHands: [Hand] { state.hands.filter { $0.status == "connected" } }
    var otherAccountHands: [AccountHand] {
        guard state.connected else { return [] }
        let local = Set(state.hands.map(\.id))
        return (state.accountHands ?? []).filter { !local.contains($0.id) }
    }
    var connectedHandCount: Int { connectedHands.count + otherAccountHands.filter(\.isConnected).count }
    var selectableHands: [Hand] { connectedHands.filter { $0.agentId == nil || $0.agentId == activeTab?.threadId } }
    var showsOnboarding: Bool { !state.connected || (phoneSignInActive && !phoneSignInStartedConnected) }

    init(runtimeDirectory: String? = nil, backgroundPreferences: UserDefaults? = nil) {
        runtime = RuntimeClient(dataDirectory: runtimeDirectory)
        isolatedSession = runtimeDirectory != nil || ProcessInfo.processInfo.environment["NANOCODEX_DESKTOP_DATA"] != nil || ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
        self.backgroundPreferences = backgroundPreferences ?? (isolatedSession ? nil : .standard)
        launchAtLogin = LaunchAtLogin.installed(isolatedSession: isolatedSession, preferences: self.backgroundPreferences)
        keepMacAwake = self.backgroundPreferences?.object(forKey: "keepMacAwakeWhileHandsRunning") as? Bool ?? true
        activeTabID = tabs[0].id
        runtime.onEvent = { [weak self] event in self?.receive(event) }
        runtime.onFailure = { [weak self] message in
            self?.error = message; self?.isStarting = false; self?.runtimeFailed = true
            self?.updateBackgroundActivity()
        }
    }
    var backgroundHandStatus: String {
        if runtimeFailed { return "Runtime stopped · Quit and reopen Nanocodex" }
        if isStarting { return "Connecting…" }
        if !state.connected { return "Sign in to connect Hands" }
        if state.defaultHandEnabled == false && !state.hands.contains(where: \.isRunning) { return "This Mac’s Hand is disabled" }
        let connected = state.hands.filter { $0.status == "connected" }.count
        if connected > 0 { return connected == 1 ? "1 Hand connected" : "\(connected) Hands connected" }
        return state.hands.contains(where: \.isRunning) ? "Connecting Hands…" : "No Hands running"
    }
    private func updateBackgroundActivity() {
        backgroundActivity.update(running: !backgroundActivityStopped && !runtimeFailed && state.connected && state.hands.contains(where: \.isRunning), keepAwake: keepMacAwake)
    }
    func start() async {
        guard !didStart else { return }; didStart = true
        launchAtLogin.start()
        do {
            let imported = AccountKeychain.environmentCredential()
            let initial = imported ?? (isolatedSession ? nil : AccountKeychain.read())
            currentCredential = initial
            try runtime.start(credential: initial)
            apply(try await runtime.call("state", as: DesktopState.self))
            apply(try await runtime.call("refresh", as: DesktopState.self))
            if state.connected {
                if !isolatedSession, let imported { try AccountKeychain.save(imported) }
                await restoreObservers()
            }
        } catch { self.error = error.localizedDescription }
        isStarting = false
    }
    private func receive(_ event: RuntimeEvent) {
        switch event {
        case .state(let next): apply(next)
        case .thread(let thread): apply(thread)
        case .ignored: break
        }
    }
    private func apply(_ next: DesktopState) {
        guard next != state else { return }
        let accountChanged = state.accountScope != nil && state.accountScope != next.accountScope
        if accountChanged { resetAccount() }
        let wasConnected = state.connected
        state = next
        if !restoredLayout, let layout = next.layout, !layout.tabs.isEmpty {
            tabs = layout.tabs; activeTabID = tabs.contains(where: { $0.id == layout.activeTabId }) ? layout.activeTabId : tabs[0].id
            tabPosition = layout.tabPosition; theme = layout.theme
            tiledTabIDs = (layout.tiledTabIDs ?? []).filter { id in tabs.contains { $0.id == id } }
            workspaceMode = layout.workspaceMode == "tiles" && tiledTabIDs.count > 1 ? "tiles" : "single"; paneWidth = layout.paneWidth ?? 0
            pending = (layout.pendingMessages ?? []).map { value in var restored = value; restored.restore(); return restored }
            restoredLayout = true
        }
        if (!wasConnected || accountChanged), next.connected {
            connectDefaultHand()
            configureAutomaticScreenSharing()
            observeAccountHands()
            Task { await restoreObservers() }
        }
        if !next.connected {
            defaultHandConnection?.cancel(); defaultHandConnection = nil
            accountHandDiscovery?.cancel(); accountHandDiscovery = nil
        }
    }
    private func apply(_ thread: ThreadSnapshot) {
        let previous = snapshots[thread.id]
        let eventsChanged = previous?.events != thread.events
        let needsReconciliation = pending.contains { $0.agentID == thread.id }
        if !eventsChanged, !needsReconciliation, previous?.hasMore == thread.hasMore, previous?.connected == thread.connected,
           previous?.activeTurns == thread.activeTurns, previous?.settings == thread.settings,
           previous?.error == thread.error, previous?.acceptedTurns == thread.acceptedTurns, previous?.cursor == thread.cursor { return }
        snapshots[thread.id] = thread
        if thread.connected { threadErrors.removeValue(forKey: thread.id) }
        if eventsChanged {
            let events = conversationEvents(thread.events)
            reviewEvents[thread.id] = events
            messages[thread.id] = timelineProjections[thread.id, default: TimelineProjection()].project(events)
        }
        reconcilePending(thread)
        if activeTab?.threadId == thread.id, settings != thread.settings { settings = thread.settings }
    }
    private func restoreObservers() async {
        for id in Set(tabs.compactMap(\.threadId) + pending.compactMap(\.agentID)) { await observe(id) }
        if isTiled {
            if !tiledTabIDs.contains(activeTabID) { activeTabID = tiledTabIDs[0] }
        } else if pinnedPaneID == nil, !visibleTabs.contains(where: { $0.id == activeTabID }) {
            activeTabID = visibleTabs.first?.id ?? ""
        }
    }
    func observe(_ id: String) async {
        guard !observation.contains(id), state.connected else { return }
        let epoch = generation
        observation.insert(id); loading.insert(id); threadErrors.removeValue(forKey: id)
        defer { if epoch == generation { loading.remove(id) } }
        do {
            let thread = try await runtime.call("openThread", [.string(id)], as: ThreadSnapshot.self)
            if current(epoch) { apply(thread) }
        }
        catch { if current(epoch) { observation.remove(id); threadErrors[id] = error.localizedDescription } }
    }
    func title(_ tab: WorkspaceTab) -> String {
        if let title = tab.title, !title.isEmpty { return title }
        if let id = tab.threadId, let thread = state.threads.first(where: { $0.id == id }), thread.title != "New thread" { return thread.title }
        return displayedTranscript(tab.id).first(where: { $0.kind == .user }).map { String($0.displayText.prefix(72)) } ?? "New thread"
    }
    func select(_ id: String) {
        guard tabs.contains(where: { $0.id == id }), id != activeTabID || screen != .chat || pinnedPaneID != id else { return }
        if isTiled, !tiledTabIDs.contains(id), let slot = tiledTabIDs.firstIndex(of: activeTabID) { tiledTabIDs[slot] = id }
        activeTabID = id; pinnedPaneID = id; screen = .chat
        if let threadID = activeTab?.threadId { Task { await observe(threadID) } }; settings = settingsForTab(id)
        persistLayout()
    }
    func newTab(target: String = "", beside: Bool = false) {
        let tab = WorkspaceTab(target: target)
        tabs.append(tab)
        if beside { openBeside(tab.id) } else { select(tab.id); requestEditorFocus(tab.id) }
        showingPanePicker = false; persistLayout()
    }
    func open(_ thread: AgentThread) {
        if let tab = tabs.first(where: { $0.threadId == thread.id }) { select(tab.id) }
        else { let tab = WorkspaceTab(threadId: thread.id); tabs.append(tab); select(tab.id) }
        showingSearch = false
    }
    func closeTab(_ id: String) {
        guard let index = tabs.firstIndex(where: { $0.id == id }) else { return }
        if preparingVoiceTabID == id || (tabs[index].threadId != nil && voice.conversationID == tabs[index].threadId) { voice.stop() }
        if tiledTabIDs.contains(id) { removePane(id) }
        let tab = tabs.remove(at: index); closedTabs.append(tab)
        if closedTabs.count > 20 { closedTabs.removeFirst() }
        if tabs.isEmpty { tabs = [WorkspaceTab()] }
        if activeTabID == id { activeTabID = tabs[min(index, tabs.count - 1)].id; pinnedPaneID = activeTabID; requestEditorFocus(activeTabID) }
        if let threadID = tab.threadId, !tabs.contains(where: { $0.threadId == threadID }), !pending.contains(where: { $0.agentID == threadID }) {
            observation.remove(threadID)
            timelineProjections.removeValue(forKey: threadID)
            Task { try? await runtime.request("closeThread", [.string(threadID)]) }
        }
        persistLayout()
    }
    func renameTab(_ tab: WorkspaceTab) {
        let alert = NSAlert(); alert.messageText = "Rename Tab"; alert.informativeText = "Give this tab a name. Clear it to use the thread’s title."
        alert.addButton(withTitle: "Save"); alert.addButton(withTitle: "Cancel")
        let field = NSTextField(string: tab.title ?? ""); field.frame = NSRect(x: 0, y: 0, width: 300, height: 24)
        field.placeholderString = title(tab); alert.accessoryView = field
        alert.window.initialFirstResponder = field
        if alert.runModal() == .alertFirstButtonReturn, let index = tabs.firstIndex(where: { $0.id == tab.id }) {
            let name = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            tabs[index].title = name.isEmpty ? nil : String(name.prefix(128)); persistLayout()
        }
    }
    func reopenTab() { guard let tab = closedTabs.popLast() else { return }; tabs.append(tab); select(tab.id); requestEditorFocus(tab.id) }
    func cycleTab(_ offset: Int) { guard let index = tabs.firstIndex(where: { $0.id == activeTabID }) else { return }; select(tabs[(index + offset + tabs.count) % tabs.count].id) }
    func moveTab(_ source: String, before destination: String) {
        guard source != destination, let from = tabs.firstIndex(where: { $0.id == source }), let to = tabs.firstIndex(where: { $0.id == destination }) else { return }
        let value = tabs.remove(at: from); tabs.insert(value, at: min(to, tabs.count)); inboxOrder = tabs.map(\.id); persistLayout()
    }
    func updateDraft(_ draft: String, tabID: String? = nil) { updateTab(tabID: tabID) { $0.draft = draft } }
    func updateTarget(_ target: String, tabID: String? = nil) {
        let selectableHands = handsForTab(tabID)
        let remote = otherAccountHands.first(where: { $0.id == target && $0.isConnected })
        guard target.isEmpty || selectableHands.contains(where: { $0.id == target }) || remote != nil else { return }
        let hand = selectableHands.first(where: { $0.id == target })
        updateTab(tabID: tabID) {
            $0.target = target
            if let hand { $0.folder = hand.kind == "local" ? hand.workspace : "" }
            else if remote != nil { $0.folder = "" }
        }
    }
    func updateTab(tabID: String? = nil, _ action: (inout WorkspaceTab) -> Void) { guard let index = tabs.firstIndex(where: { $0.id == (tabID ?? activeTabID) }) else { return }; action(&tabs[index]); persistLayout() }
    func persistLayout() {
        persistence?.cancel()
        guard !accountTransition else { return }
        let layout = TabLayout(tabs: tabs, activeTabId: activeTabID, tabPosition: tabPosition, theme: theme, workspaceMode: workspaceMode, paneWidth: paneWidth, tiledTabIDs: tiledTabIDs, pendingMessages: pending)
        let scope = state.accountScope
        persistence = Task {
            try? await Task.sleep(for: .milliseconds(350))
            guard !Task.isCancelled else { return }
            do { try await runtime.request("saveLayout", [try Self.layoutPayload(layout, scope: scope)]) }
            catch { self.error = error.localizedDescription }
        }
    }
    private static func layoutPayload(_ layout: TabLayout, scope: String?) throws -> JSONValue {
        let payload = try JSONValue.encoded(layout)
        guard case .object(var fields) = payload else { return payload }
        if let scope { fields["accountScope"] = .string(scope) }
        return .object(fields)
    }
    func chooseFolder(tabID: String? = nil) {
        let tabID = tabID ?? activeTabID
        let panel = NSOpenPanel(); panel.title = "Choose a folder for this tab"; panel.prompt = "Use folder"
        panel.canChooseDirectories = true; panel.canChooseFiles = false; panel.canCreateDirectories = true
        panel.message = "This folder is shared with this thread when you send. Commands run as your macOS user."
        if panel.runModal() == .OK, let path = panel.url?.path { updateTab(tabID: tabID) { $0.folder = path; $0.target = "" } }
    }
    private func current(_ epoch: Int) -> Bool { epoch == generation && state.connected && !accountTransition }
    private func saveQueue() async throws {
        persistence?.cancel()
        let layout = TabLayout(tabs: tabs, activeTabId: activeTabID, tabPosition: tabPosition, theme: theme, workspaceMode: workspaceMode, paneWidth: paneWidth, tiledTabIDs: tiledTabIDs, pendingMessages: pending)
        try await runtime.request("saveLayout", [try Self.layoutPayload(layout, scope: state.accountScope)])
    }
    private func changePending(_ id: String, _ change: (inout PendingMessage) -> Void) {
        guard let index = pending.firstIndex(where: { $0.id == id }) else { return }
        change(&pending[index]); persistLayout()
    }
    private func removeCancelledPending(_ id: String) {
        guard let message = pending.first(where: { $0.id == id }) else { return }
        for index in pending.indices where pending[index].agentID == message.agentID && pending[index].predecessor == id {
            pending[index].predecessor = message.predecessor
        }
        pending.removeAll { $0.id == id }
    }
    private func reconcilePending(_ thread: ThreadSnapshot) {
        let before = pending
        for event in thread.events where event.data["type"].string == "turn_cancelled" {
            removeCancelledPending(event.turnId ?? event.data["id"].string)
        }
        for index in pending.indices where pending[index].agentID == thread.id {
            if let accepted = thread.events.first(where: { $0.data["type"].string == "turn_accepted" && ($0.turnId ?? $0.data["id"].string) == pending[index].id }) {
                pending[index].acceptedCursor = accepted.cursor
                if pending[index].phase == .submitting || pending[index].phase == .failed {
                    pending[index].phase = .queued; pending[index].error = nil
                }
            }
        }
        pending.removeAll { message in
            guard message.agentID == thread.id else { return false }
            if message.phase == .cancelling {
                return message.hasFinished(in: thread) || thread.events.contains {
                    ($0.turnId ?? $0.data["id"].string) == message.id && ["turn_completed", "turn_failed", "turn_cancelled"].contains($0.data["type"].string)
                }
            }
            return message.hasStarted(in: thread.events) || message.hasFinished(in: thread)
        }
        if before != pending { persistLayout() }
    }
    func send(_ text: String? = nil, targetOverride: String? = nil, tabID: String? = nil) async {
        guard state.connected else { showingSettings = true; return }
        guard let tab = tab(tabID), canSend(tab.id) else { return }
        let input = (text ?? tab.draft).trimmingCharacters(in: .whitespacesAndNewlines)
        guard !input.isEmpty else { return }
        let message = PendingMessage(tabID: tab.id, agentID: tab.threadId, text: input,
                                     predecessor: pendingMessages(tab.id).last?.id ?? controllableTurns(tab.id).first ?? "",
                                     target: targetOverride ?? tab.target, folder: targetOverride == nil ? tab.folder : "", settings: settingsForTab(tab.id))
        pending.append(message); busyMessages.insert(message.id)
        updateDraft("", tabID: tab.id); error = nil
        await submit(message.id, epoch: generation)
    }

    func voiceConfiguration(tabID: String) async throws -> VoiceConfiguration {
        guard current(generation), let credential = currentCredential, let requested = tab(tabID),
              let url = URL(string: credential.baseUrl) else {
            throw RuntimeFailure(message: "Connect your account to start voice.")
        }
        let epoch = generation
        preparingVoiceTabID = tabID
        defer { if preparingVoiceTabID == tabID { preparingVoiceTabID = nil } }
        var agentID = requested.threadId
        if agentID == nil {
            guard !isBusy(tabID) else { throw RuntimeFailure(message: "This conversation is still being created.") }
            let thread: AgentThread = try await runtime.call("createThread", [try .encoded(settingsForTab(tabID))])
            try Task.checkCancellation()
            guard current(epoch), let index = tabs.firstIndex(where: { $0.id == tabID }) else { throw CancellationError() }
            tabs[index].threadId = thread.id; agentID = thread.id
            persistLayout()
        }
        guard let agentID, current(epoch) else { throw CancellationError() }
        let boundary = (snapshots[agentID]?.cursor ?? snapshots[agentID]?.events.last?.cursor).flatMap { Cursor(rawValue: $0) }
        voice.transcriptFeed.begin(conversationID: agentID, durableRows: voiceTranscriptRows(tabID), after: boundary)
        Task { await observe(agentID) }
        return VoiceConfiguration(baseURL: url, apiKey: credential.apiKey, agentID: agentID, conversationTitle: title(requested))
    }
    func retryPending(_ id: String) async {
        guard let message = pending.first(where: { $0.id == id }), message.phase == .failed,
              !busyMessages.contains(id), current(generation), !isBusy(message.tabID) else { return }
        busyMessages.insert(id)
        changePending(id) { $0.phase = .submitting; $0.error = nil }
        await submit(id, epoch: generation)
    }
    private func submit(_ requestID: String, epoch: Int) async {
        defer { if epoch == generation { busyMessages.remove(requestID) } }
        guard var message = pending.first(where: { $0.id == requestID }) else { return }
        var preparingHand = false
        var submitting = false
        do {
            try await saveQueue()
            guard current(epoch) else { return }
            if message.agentID == nil {
                let thread: AgentThread = try await runtime.call("createThread", [try .encoded(message.settings ?? AgentSettings())])
                guard current(epoch) else { return }
                message.agentID = thread.id
                changePending(requestID) { $0.agentID = thread.id }
                if let index = tabs.firstIndex(where: { $0.id == message.tabID }) { tabs[index].threadId = thread.id }
                try await saveQueue()
            }
            guard current(epoch), let agentID = message.agentID else { return }
            await observe(agentID)
            guard current(epoch) else { return }
            if message.prompt == nil {
                if message.target.isEmpty, !message.folder.isEmpty {
                    preparingHand = true
                    let hand: Hand = try await runtime.call("prepareFolderHand", [.object(["agentId": .string(agentID), "workspace": .string(message.folder)])])
                    guard current(epoch) else { return }
                    message.target = hand.id
                    changePending(requestID) { $0.target = hand.id }
                    if let index = tabs.firstIndex(where: { $0.id == message.tabID }) { tabs[index].target = hand.id }
                }
                if let hand = state.hands.first(where: { $0.id == message.target }), hand.status != "connected" {
                    preparingHand = true
                    if state.defaultHandEnabled == false, state.hands.first(where: { $0.kind == "local" && $0.agentId == nil })?.id == hand.id {
                        throw RuntimeFailure(message: "This Mac’s Hand is disabled. Enable it in Settings before sending to it.")
                    }
                    await startHand(hand.id)
                    guard current(epoch) else { return }
                    guard state.hands.first(where: { $0.id == hand.id })?.status == "connected" else { throw RuntimeFailure(message: "This Hand could not connect. Open Hands to retry.") }
                }
                var prompt = message.text
                preparingHand = false
                if !message.target.isEmpty {
                    let handName = state.hands.first(where: { $0.id == message.target })?.name
                        ?? otherAccountHands.first(where: { $0.id == message.target })?.name ?? message.target
                    prompt += "\n\n[Selected Hand: \(handName) (\(message.target)). Call accountInfo to resolve its exact mounted workspace. Execute commands and file operations on this Hand only. If unavailable, report that instead of choosing another machine.]"
                }
                message.prompt = prompt
                changePending(requestID) { $0.prompt = prompt }
                try await saveQueue()
            }
            guard current(epoch) else { return }
            submitting = true
            let accepted = try await runtime.request("queuePrompt", [.object(["agentId": .string(agentID), "input": .string(message.prompt ?? message.text), "requestId": .string(requestID)])])
            guard current(epoch) else { return }
            guard accepted["turn_id"].string == requestID else { throw RuntimeFailure(message: "The message acknowledgement did not match.") }
            changePending(requestID) {
                if $0.phase == .submitting { $0.phase = .queued }; $0.error = nil
                let cursor = accepted["cursor"].string.isEmpty ? accepted["accepted_cursor"].string : accepted["cursor"].string
                if !cursor.isEmpty { $0.acceptedCursor = cursor }
            }
            if ["completed", "failed", "cancelled"].contains(accepted["state"].string) {
                if accepted["state"].string == "cancelled" { removeCancelledPending(requestID) }
                else { pending.removeAll { $0.id == requestID } }
                persistLayout()
            }
            if let snapshot = snapshots[agentID] { reconcilePending(snapshot) }
        } catch {
            guard current(epoch) else { return }
            changePending(requestID) {
                $0.phase = .failed
                $0.error = submitting ? "Delivery unconfirmed. Retry checks the same message; it won’t create another one."
                    : preparingHand ? "The Hand couldn’t connect. Retry to reconnect and send this message."
                    : "Couldn’t prepare this message. Retry to send it."
            }
        }
    }
    func steerNow(_ id: String) async {
        guard let message = pending.first(where: { $0.id == id }),
              pending.first(where: { $0.agentID == message.agentID })?.id == id,
              let predecessor = message.interruption else { return }
        await controlPending(message, turnID: predecessor, phase: .starting)
    }
    func cancelPending(_ id: String) async {
        guard let message = pending.first(where: { $0.id == id }) else { return }
        if message.agentID == nil || (message.phase == .failed && message.prompt == nil && message.acceptedCursor == nil) {
            guard !busyMessages.contains(id) else { return }
            removeCancelledPending(id); persistLayout(); return
        }
        await controlPending(message, turnID: id, phase: .cancelling)
    }
    private func controlPending(_ message: PendingMessage, turnID: String, phase: PendingMessage.Phase) async {
        guard let agentID = message.agentID, current(generation),
              !pending.contains(where: { $0.agentID == agentID && busyMessages.contains($0.id) }),
              message.phase != .starting, message.phase != .cancelling else { return }
        let epoch = generation
        busyMessages.insert(message.id)
        changePending(message.id) { $0.phase = phase; $0.error = nil }
        defer { if epoch == generation { busyMessages.remove(message.id) } }
        do {
            try await saveQueue()
            guard current(epoch) else { return }
            let receipt = try await runtime.request("cancel", [.object(["agentId": .string(agentID), "turnId": .string(turnID)])])
            guard current(epoch) else { return }
            if phase == .cancelling, ["completed", "cancelled", "failed"].contains(receipt["state"].string) {
                removeCancelledPending(message.id); persistLayout()
            }
        } catch {
            guard current(epoch) else { return }
            changePending(message.id) { $0.phase = message.phase; $0.error = "Cancellation unconfirmed. The queued message is retained; try again." }
        }
    }
    func cancel(tabID: String? = nil) async {
        guard current(generation), let id = tab(tabID)?.threadId, let turn = controllableTurns(tabID).first else { return }
        let epoch = generation
        do { try await runtime.request("cancel", [.object(["agentId": .string(id), "turnId": .string(turn)])]) }
        catch { if current(epoch) { self.error = error.localizedDescription } }
    }
    func loadOlder(tabID: String? = nil) async {
        guard current(generation), let id = tab(tabID)?.threadId else { return }
        let epoch = generation
        do {
            let snapshot = try await runtime.call("older", [.string(id)], as: ThreadSnapshot.self)
            if current(epoch), tabs.contains(where: { $0.threadId == id }) { apply(snapshot) }
        } catch { if current(epoch) { self.error = error.localizedDescription } }
    }
    func changeSettings(tabID: String?, _ change: (inout AgentSettings) -> Void) {
        let id = tabID ?? activeTabID
        var copy = settingsForTab(id); change(&copy)
        updateTab(tabID: id) { $0.draftSettings = copy }
        if let threadID = tab(id)?.threadId { snapshots[threadID]?.settings = copy }
        if id == activeTabID { settings = copy }
        guard let threadID = tab(id)?.threadId else { return }
        Task {
            do { let _: AgentSettings = try await runtime.call("settings", [.object(["agentId": .string(threadID), "settings": try .encoded(copy)])]) }
            catch { self.error = error.localizedDescription }
        }
    }
    func updateSettings() {
        guard let id = activeTab?.threadId else { return }
        let copy = settings
        Task {
            do { let _: AgentSettings = try await runtime.call("settings", [.object(["agentId": .string(id), "settings": try .encoded(copy)])]) }
            catch { self.error = error.localizedDescription }
        }
    }
    func refresh() async {
        let id = activeTab?.threadId
        do {
            apply(try await runtime.call("refresh", as: DesktopState.self)); error = nil
            if let id, threadErrors[id] != nil { await observe(id) }
        }
        catch { self.error = error.localizedDescription }
    }
    func connect(baseUrl: String, key: String, remember: Bool, dismissSettings: Bool = true) async throws {
        voice.stop()
        persistence?.cancel(); accountTransition = true
        defer { accountTransition = false }
        let next: DesktopState = try await runtime.call("connect", [.object(["baseUrl": .string(baseUrl), "apiKey": .string(key), "remember": .bool(false)])])
        resetAccount(); apply(next)
        currentCredential = .init(baseUrl: next.baseUrl, apiKey: key)
        if !isolatedSession {
            if remember { try AccountKeychain.save(.init(baseUrl: next.baseUrl, apiKey: key)) } else { AccountKeychain.remove() }
        }
        error = nil
        if dismissSettings { showingSettings = false }
    }
    func startPhoneSignIn(phone: String, baseUrl: String) async throws -> SignInChallenge {
        if !phoneSignInActive {
            phoneSignInStartedConnected = state.connected
            signInPreviousCredential = currentCredential
            signInPreviousSavedCredential = isolatedSession ? nil : AccountKeychain.read()
            phoneSignInActive = true
        }
        let challenge: SignInChallenge = try await runtime.call("startSignIn", [.object(["phone": .string(phone), "baseUrl": .string(baseUrl)])])
        phoneSignInChallenge = challenge
        return challenge
    }
    func finishPhoneSignIn(code: String) async throws {
        persistence?.cancel(); accountTransition = true
        defer { accountTransition = false }
        if !signInCommitted {
            // Only this private response contains the credential. It never enters observable state.
            let credential: AccountKeychain.Credential = try await runtime.call("verifySignIn", [.object(["code": .string(code)])])
            // Save before switching, so a Keychain failure leaves the current account and Hands intact.
            if !isolatedSession { try AccountKeychain.save(credential); signInSavedInKeychain = true }
            let next: DesktopState
            do {
                next = try await runtime.call("connect", [.object(["baseUrl": .string(credential.baseUrl), "apiKey": .string(credential.apiKey), "remember": .bool(false)])])
            } catch {
                if !isolatedSession {
                    do {
                        if let previous = signInPreviousSavedCredential { try AccountKeychain.save(previous) }
                        else { try AccountKeychain.removeChecked() }
                        signInSavedInKeychain = false
                    } catch {
                        throw RuntimeFailure(message: "macOS could not restore your previous saved account. Your new sign-in remains securely saved. Retry to finish switching accounts.")
                    }
                }
                throw error
            }
            signInChangedAccount = true
            resetAccount(); apply(next)
            currentCredential = credential
            signInCommitted = true
        }
        try await runtime.request("completeSignIn")
        clearPhoneSignIn()
        error = nil; showingSettings = false
    }
    func cancelPhoneSignIn() async throws {
        guard phoneSignInActive else { return }
        if signInCommitted || signInSavedInKeychain {
            // This credential is already in Keychain. Never revoke it when closing the form.
            try await runtime.request("completeSignIn")
        } else {
            if signInChangedAccount {
                try await restoreSignInPreviousAccount()
            }
            try await runtime.request("cancelSignIn")
        }
        clearPhoneSignIn()
    }
    private func restoreSignInPreviousAccount() async throws {
        if let previous = signInPreviousCredential {
            let next: DesktopState = try await runtime.call("connect", [.object(["baseUrl": .string(previous.baseUrl), "apiKey": .string(previous.apiKey), "remember": .bool(false)])])
            resetAccount(); apply(next); currentCredential = previous
        } else {
            let next: DesktopState = try await runtime.call("disconnect")
            resetAccount(); apply(next); currentCredential = nil
        }
        signInChangedAccount = false
    }
    private func clearPhoneSignIn() {
        phoneSignInActive = false; phoneSignInStartedConnected = false
        phoneSignInChallenge = nil
        signInPreviousCredential = nil; signInPreviousSavedCredential = nil; signInChangedAccount = false; signInCommitted = false; signInSavedInKeychain = false
    }
    func disconnect() async {
        voice.stop()
        do { try await cancelPhoneSignIn(); apply(try await runtime.call("disconnect", as: DesktopState.self)); if !isolatedSession { AccountKeychain.remove() }; currentCredential = nil; resetAccount() }
        catch { self.error = error.localizedDescription }
    }
    private func resetAccount() {
        voice.stop(); voice.transcriptFeed.clear(); preparingVoiceTabID = nil
        accountHandDiscovery?.cancel(); accountHandDiscovery = nil
        defaultHandConnection?.cancel(); defaultHandConnection = nil
        resetRemoteSharing(); showingScreens = false
        persistence?.cancel()
        timelineProjections.removeAll()
        requestedEditorTabID = nil; readingPositions = [:]; expandedMessages = [:]; inboxOrder = []; pinnedPaneID = nil; workspaceFilter = .inbox; workspaceMode = "single"; tiledTabIDs = []; showingPanePicker = false; paneWidth = 0
        generation += 1; busyMessages = []; snapshots = [:]; reviewEvents = [:]; threadErrors = [:]; messages = [:]; pending = []; observation = []; closedTabs = []; tabs = [WorkspaceTab()]; activeTabID = tabs[0].id; restoredLayout = false
    }
    func useThisMac() async {
        do {
            apply(try await runtime.call("setDefaultHandEnabled", [.bool(true)], as: DesktopState.self))
            let _: Hand? = try await runtime.call("prepareDefaultHand")
        }
        catch { self.error = error.localizedDescription }
    }
    func setDeviceHandEnabled(_ enabled: Bool) async {
        defaultHandConnection?.cancel(); defaultHandConnection = nil
        do {
            apply(try await runtime.call("setDefaultHandEnabled", [.bool(enabled)], as: DesktopState.self))
            if enabled { connectDefaultHand() }
        } catch { self.error = error.localizedDescription }
    }
    func saveHand(_ hand: Hand, start: Bool) async {
        do { apply(try await runtime.call("saveHand", [try .encoded(hand)], as: DesktopState.self)); showingHandSetup = false; if start { await startHand(hand.id) } }
        catch { self.error = error.localizedDescription }
    }
    func startHand(_ id: String) async { await handAction("startHand", id) }
    func stopHand(_ id: String) async {
        if state.hands.contains(where: { $0.id == id && $0.kind == "local" && $0.agentId == nil }) { defaultHandConnection?.cancel(); defaultHandConnection = nil }
        await handAction("stopHand", id)
    }
    func removeHand(_ id: String) async { await stopHand(id); await handAction("removeHand", id) }
    private func handAction(_ action: String, _ id: String) async {
        busyHands.insert(id); defer { busyHands.remove(id) }
        do { apply(try await runtime.call(action, [.string(id)], as: DesktopState.self)) }
        catch { self.error = error.localizedDescription }
    }
    func useHand(_ hand: Hand) async {
        if hand.status != "connected" { await startHand(hand.id) }
        guard let connected = state.hands.first(where: { $0.id == hand.id }), connected.status == "connected" else { return }
        if let owner = connected.agentId {
            if let tab = tabs.first(where: { $0.threadId == owner }) { select(tab.id) }
            else {
                let tab = WorkspaceTab(threadId: owner)
                tabs.append(tab); select(tab.id)
            }
            updateTarget(connected.id)
        } else {
            newTab(target: connected.id)
            updateTarget(connected.id)
        }
    }
    func createCloudHand() { newTab(); Task { await send("Create a cloud Hand using mount with provider cloudflare and a useful name. Then call accountInfo and tell me its exact mounted workspace and available capabilities.", targetOverride: "") } }
    func useAccountHand(_ hand: AccountHand) {
        guard otherAccountHands.contains(where: { $0.id == hand.id && $0.isConnected }) else { return }
        newTab(target: hand.id); screen = .chat
    }
    func refreshAccountHands() async {
        guard state.connected else { return }
        do { apply(try await runtime.call("refreshAccountHands", as: DesktopState.self)) }
        catch { /* The runtime retains the last known devices with an offline status. */ }
    }
    private func observeAccountHands() {
        accountHandDiscovery?.cancel()
        let epoch = generation
        accountHandDiscovery = Task { [weak self] in
            while !Task.isCancelled {
                guard let self, self.generation == epoch, self.state.connected else { return }
                await self.refreshAccountHands()
                do { try await Task.sleep(for: .seconds(5)) } catch { return }
            }
        }
    }
    func discoverHands() { if activeTab?.threadId == nil { newTab() }; screen = .chat; Task { await send("Call accountInfo and show my available Hands with their names, exact workspace mounts, and capabilities.", targetOverride: "") } }
    func openAccount() { guard let url = URL(string: state.baseUrl + "/connect") else { return }; NSWorkspace.shared.open(url) }
    func prepareToQuit() async {
        accountHandDiscovery?.cancel(); accountHandDiscovery = nil
        backgroundActivityStopped = true; backgroundActivity.stop()
        voice.stop(); await voice.finishStopping()
        defaultHandConnection?.cancel(); defaultHandConnection = nil
        await remoteMacHost.stop(); await remotePhoneHost.stop(); remoteService?.close()
        persistence?.cancel()
        try? await cancelPhoneSignIn()
        _ = try? await runtime.request("saveLayout", [try Self.layoutPayload(TabLayout(tabs: tabs, activeTabId: activeTabID, tabPosition: tabPosition, theme: theme, workspaceMode: workspaceMode, paneWidth: paneWidth, tiledTabIDs: tiledTabIDs, pendingMessages: pending), scope: state.accountScope)])
        runtime.stop()
    }
    func shutdown() { resetRemoteSharing(); backgroundActivityStopped = true; backgroundActivity.stop(); voice.stop(); defaultHandConnection?.cancel(); accountHandDiscovery?.cancel(); persistence?.cancel(); runtime.stop() }

    private func connectDefaultHand() {
        defaultHandConnection?.cancel()
        let epoch = generation
        defaultHandConnection = Task { [weak self] in
            var delay = 1
            while !Task.isCancelled {
                guard let self, self.generation == epoch, self.state.connected, self.state.defaultHandEnabled != false else { return }
                do { let _: Hand? = try await self.runtime.call("prepareDefaultHand"); return }
                catch {
                    do { try await Task.sleep(for: .seconds(delay)) } catch { return }
                    delay = min(30, delay * 2)
                }
            }
        }
    }
    private func resetRemoteSharing() {
        remoteService?.close(); remoteService = nil
        let mac = remoteMacHost, phone = remotePhoneHost
        mac.revokeControl(); phone.revokeControl()
        remoteMacHost = RemoteMacHost(); remotePhoneHost = RemoteMacHost()
        Task { await mac.stop(); await phone.stop() }
    }

    private func configureAutomaticScreenSharing() {
        guard !isolatedSession, state.connected, !backgroundActivityStopped,
              let remoteService else { return }
        remoteMacHost.configureAutomaticSharing(service: remoteService, defaults: backgroundPreferences ?? .standard)
    }
}
