import AppKit
import SwiftUI

@MainActor
final class AppModel: ObservableObject {
    enum Screen { case chat, hands }
    struct PendingMessage { var id: String; var text: String }
    private struct RetryRequest { var id: String; var text: String; var target: String; var folder: String }
    @Published var state = DesktopState()
    @Published var tabs: [WorkspaceTab] = [WorkspaceTab()]
    @Published var activeTabID = ""
    @Published var tabPosition = "left"
    @Published var theme = "system"
    @Published var screen: Screen = .chat
    @Published var snapshots: [String: ThreadSnapshot] = [:]
    @Published var messages: [String: [MessageEntry]] = [:]
    @Published var pending: [String: PendingMessage] = [:]
    @Published var loading = Set<String>()
    @Published var busyHands = Set<String>()
    @Published var isStarting = true
    @Published var error: String?
    @Published var showingSettings = false
    @Published var showingSearch = false
    @Published var showingHandSetup = false
    @Published var showingRemoteSetup = false
    @Published var editingHand: Hand?
    @Published var settings = AgentSettings()
    @Published var selectedHandForLogs: Hand?
    @Published private(set) var phoneSignInActive = false
    @Published private(set) var phoneSignInStartedConnected = false
    @Published private(set) var phoneSignInChallenge: SignInChallenge?
    let runtime: RuntimeClient
    private var closedTabs: [WorkspaceTab] = []
    private var retries: [String: RetryRequest] = [:]
    private var restoredLayout = false
    private var persistence: Task<Void, Never>?
    private var observation = Set<String>()
    private var didStart = false
    private let isolatedSession: Bool
    private var currentCredential: AccountKeychain.Credential?
    private var signInPreviousCredential: AccountKeychain.Credential?
    private var signInPreviousSavedCredential: AccountKeychain.Credential?
    private var signInChangedAccount = false
    private var signInCommitted = false
    private var signInSavedInKeychain = false
    private var accountTransition = false

    var activeTab: WorkspaceTab? { tabs.first { $0.id == activeTabID } }
    var activeSnapshot: ThreadSnapshot? { activeTab?.threadId.flatMap { snapshots[$0] } }
    var activeMessages: [MessageEntry] { activeTab?.threadId.flatMap { messages[$0] } ?? [] }
    var isRunning: Bool { !(activeSnapshot?.activeTurns.isEmpty ?? true) }
    var hasUnsentMessage: Bool { !(activeTab?.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true) }
    var preferredColorScheme: ColorScheme? { theme == "dark" ? .dark : theme == "light" ? .light : nil }
    var connectedHands: [Hand] { state.hands.filter { $0.status == "connected" } }
    var selectableHands: [Hand] { connectedHands.filter { $0.agentId == nil || $0.agentId == activeTab?.threadId } }
    var showsOnboarding: Bool { !state.connected || (phoneSignInActive && !phoneSignInStartedConnected) }

    init(runtimeDirectory: String? = nil) {
        runtime = RuntimeClient(dataDirectory: runtimeDirectory)
        isolatedSession = runtimeDirectory != nil || ProcessInfo.processInfo.environment["NANOCODEX_DESKTOP_DATA"] != nil || ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
        activeTabID = tabs[0].id
        runtime.onEvent = { [weak self] event in self?.receive(event) }
        runtime.onFailure = { [weak self] message in self?.error = message; self?.isStarting = false }
    }
    func start() async {
        guard !didStart else { return }; didStart = true
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
        let wasConnected = state.connected
        state = next
        if !restoredLayout, let layout = next.layout, !layout.tabs.isEmpty {
            tabs = layout.tabs; activeTabID = tabs.contains(where: { $0.id == layout.activeTabId }) ? layout.activeTabId : tabs[0].id
            tabPosition = layout.tabPosition; theme = layout.theme; restoredLayout = true
        }
        if !wasConnected, next.connected { Task { await restoreObservers() } }
    }
    private func apply(_ thread: ThreadSnapshot) {
        let previous = snapshots[thread.id]
        let eventsChanged = previous?.events != thread.events
        let needsAcceptance = tabs.contains { $0.threadId == thread.id && (pending[$0.id] != nil || retries[$0.id] != nil) }
        if !eventsChanged, !needsAcceptance, previous?.hasMore == thread.hasMore, previous?.connected == thread.connected,
           previous?.activeTurns == thread.activeTurns, previous?.settings == thread.settings,
           previous?.error == thread.error, previous?.acceptedTurns == thread.acceptedTurns { return }
        snapshots[thread.id] = thread
        if eventsChanged { messages[thread.id] = projectTimeline(thread.events) }
        for tab in tabs where tab.threadId == thread.id {
            let accepted = thread.events.filter { $0.data["type"].string == "turn_accepted" }
            if let submitted = pending[tab.id], accepted.contains(where: { $0.turnId == submitted.id || $0.data["id"].string == submitted.id }) { pending.removeValue(forKey: tab.id) }
            if let retry = retries[tab.id], accepted.contains(where: { $0.turnId == retry.id || $0.data["id"].string == retry.id }) {
                if let index = tabs.firstIndex(where: { $0.id == tab.id }), tabs[index].draft == retry.text { tabs[index].draft = "" }
                retries.removeValue(forKey: tab.id)
            }
        }
        if activeTab?.threadId == thread.id, settings != thread.settings { settings = thread.settings }
    }
    private func restoreObservers() async {
        for id in Set(tabs.compactMap(\.threadId)) { await observe(id) }
    }
    func observe(_ id: String) async {
        guard !observation.contains(id), state.connected else { return }
        observation.insert(id); loading.insert(id)
        defer { loading.remove(id) }
        do { apply(try await runtime.call("openThread", [.string(id)], as: ThreadSnapshot.self)) }
        catch { observation.remove(id); self.error = error.localizedDescription }
    }
    func title(_ tab: WorkspaceTab) -> String {
        if let title = tab.title, !title.isEmpty { return title }
        if let id = tab.threadId, let thread = state.threads.first(where: { $0.id == id }), thread.title != "New thread" { return thread.title }
        return pending[tab.id].map { String($0.text.prefix(72)) } ?? "New thread"
    }
    func select(_ id: String) {
        activeTabID = id; screen = .chat
        if let threadID = activeTab?.threadId { Task { await observe(threadID) }; if let snapshot = snapshots[threadID] { settings = snapshot.settings } }
        persistLayout()
    }
    func newTab(target: String = "") {
        let tab = WorkspaceTab(target: target)
        tabs.append(tab); activeTabID = tab.id; screen = .chat; settings = AgentSettings(); persistLayout()
    }
    func open(_ thread: AgentThread) {
        if let tab = tabs.first(where: { $0.threadId == thread.id }) { select(tab.id) }
        else { let tab = WorkspaceTab(threadId: thread.id); tabs.append(tab); select(tab.id) }
        showingSearch = false
    }
    func closeTab(_ id: String) {
        guard let index = tabs.firstIndex(where: { $0.id == id }) else { return }
        let tab = tabs.remove(at: index); closedTabs.append(tab)
        if closedTabs.count > 20 { closedTabs.removeFirst() }
        if tabs.isEmpty { tabs = [WorkspaceTab()] }
        if activeTabID == id { activeTabID = tabs[min(index, tabs.count - 1)].id }
        if let threadID = tab.threadId, !tabs.contains(where: { $0.threadId == threadID }) {
            observation.remove(threadID)
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
    func reopenTab() { guard let tab = closedTabs.popLast() else { return }; tabs.append(tab); select(tab.id) }
    func cycleTab(_ offset: Int) { guard let index = tabs.firstIndex(where: { $0.id == activeTabID }) else { return }; select(tabs[(index + offset + tabs.count) % tabs.count].id) }
    func moveTab(_ source: String, before destination: String) {
        guard source != destination, let from = tabs.firstIndex(where: { $0.id == source }), let to = tabs.firstIndex(where: { $0.id == destination }) else { return }
        let value = tabs.remove(at: from); tabs.insert(value, at: min(to, tabs.count)); persistLayout()
    }
    func updateDraft(_ draft: String) { updateTab { $0.draft = draft } }
    func updateTarget(_ target: String) {
        guard target.isEmpty || selectableHands.contains(where: { $0.id == target }) else { return }
        let hand = selectableHands.first(where: { $0.id == target })
        updateTab {
            $0.target = target
            if let hand { $0.folder = hand.kind == "local" ? hand.workspace : "" }
        }
    }
    func updateTab(_ action: (inout WorkspaceTab) -> Void) { guard let index = tabs.firstIndex(where: { $0.id == activeTabID }) else { return }; action(&tabs[index]); persistLayout() }
    func persistLayout() {
        persistence?.cancel()
        guard !accountTransition else { return }
        let layout = TabLayout(tabs: tabs, activeTabId: activeTabID, tabPosition: tabPosition, theme: theme)
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
    func chooseFolder() {
        let panel = NSOpenPanel(); panel.title = "Choose a folder for this tab"; panel.prompt = "Use folder"
        panel.canChooseDirectories = true; panel.canChooseFiles = false; panel.canCreateDirectories = true
        panel.message = "This folder is shared with this thread when you send. Commands run as your macOS user."
        if panel.runModal() == .OK, let path = panel.url?.path { updateTab { $0.folder = path; $0.target = "" } }
    }
    func send(_ text: String? = nil, targetOverride: String? = nil) async {
        guard state.connected else { showingSettings = true; return }
        guard let tab = activeTab else { return }
        let input = (text ?? tab.draft).trimmingCharacters(in: .whitespacesAndNewlines)
        guard !input.isEmpty, pending[tab.id] == nil else { return }
        let requestTarget = targetOverride ?? tab.target
        let requestFolder = targetOverride == nil ? tab.folder : ""
        let previous = retries[tab.id]
        let requestID = previous?.text == input && previous?.target == requestTarget && previous?.folder == requestFolder ? previous!.id : UUID().uuidString
        retries[tab.id] = RetryRequest(id: requestID, text: input, target: requestTarget, folder: requestFolder)
        pending[tab.id] = .init(id: requestID, text: input)
        updateDraft(""); error = nil
        do {
            let id: String
            if let existing = tab.threadId { id = existing }
            else {
                let thread: AgentThread = try await runtime.call("createThread", [try .encoded(settings)])
                id = thread.id
                guard let index = tabs.firstIndex(where: { $0.id == tab.id }) else { throw RuntimeFailure(message: "The tab was closed before sending.") }
                tabs[index].threadId = id; persistLayout()
            }
            await observe(id)
            var prompt = input
            var target = targetOverride ?? tab.target
            if targetOverride == nil, target.isEmpty, !tab.folder.isEmpty {
                let hand: Hand = try await runtime.call("prepareFolderHand", [.object(["agentId": .string(id), "workspace": .string(tab.folder)])])
                target = hand.id
                if let index = tabs.firstIndex(where: { $0.id == tab.id }) { tabs[index].target = hand.id; retries[tab.id]?.target = hand.id; persistLayout() }
            }
            if let hand = state.hands.first(where: { $0.id == target }), hand.status != "connected" {
                await startHand(hand.id)
                guard state.hands.first(where: { $0.id == hand.id })?.status == "connected" else { throw RuntimeFailure(message: "This Hand could not connect. Open Hands to retry.") }
            }
            if !target.isEmpty {
                let handName = state.hands.first(where: { $0.id == target })?.name ?? target
                prompt += "\n\n[Selected Hand: \(handName) (\(target)). Call accountInfo to resolve its exact mounted workspace. Execute commands and file operations on this Hand only. If unavailable, report that instead of choosing another machine.]"
            }
            if targetOverride == nil, target.isEmpty, !tab.folder.isEmpty { prompt += "\n\n[Working folder selected in Nanocodex: \(tab.folder). Use it on the selected Hand.]" }
            try await runtime.request("prompt", [.object(["agentId": .string(id), "input": .string(prompt), "requestId": .string(requestID)])])
            retries.removeValue(forKey: tab.id)
        } catch {
            pending.removeValue(forKey: tab.id)
            if let index = tabs.firstIndex(where: { $0.id == tab.id }), tabs[index].draft.isEmpty { tabs[index].draft = input }
            self.error = error.localizedDescription
        }
    }
    func steer() async {
        guard let tab = activeTab, let id = tab.threadId, let turn = activeSnapshot?.activeTurns.first, !tab.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        let draft = tab.draft
        do {
            try await runtime.request("steer", [.object(["agentId": .string(id), "turnId": .string(turn), "input": .string(draft)])])
            if let index = tabs.firstIndex(where: { $0.id == tab.id }), tabs[index].draft == draft { tabs[index].draft = ""; persistLayout() }
        }
        catch { self.error = error.localizedDescription }
    }
    func cancel() async {
        guard let id = activeTab?.threadId, let turns = activeSnapshot?.activeTurns else { return }
        for turn in turns {
            do { try await runtime.request("cancel", [.object(["agentId": .string(id), "turnId": .string(turn)])]) }
            catch { self.error = error.localizedDescription }
        }
    }
    func loadOlder() async {
        guard let id = activeTab?.threadId else { return }
        do { apply(try await runtime.call("older", [.string(id)], as: ThreadSnapshot.self)) }
        catch { self.error = error.localizedDescription }
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
        do { apply(try await runtime.call("refresh", as: DesktopState.self)) }
        catch { self.error = error.localizedDescription }
    }
    func connect(baseUrl: String, key: String, remember: Bool, dismissSettings: Bool = true) async throws {
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
            currentCredential = credential
            resetAccount(); apply(next)
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
        do { try await cancelPhoneSignIn(); apply(try await runtime.call("disconnect", as: DesktopState.self)); if !isolatedSession { AccountKeychain.remove() }; currentCredential = nil; resetAccount() }
        catch { self.error = error.localizedDescription }
    }
    private func resetAccount() {
        persistence?.cancel()
        snapshots = [:]; messages = [:]; pending = [:]; retries = [:]; observation = []; closedTabs = []; tabs = [WorkspaceTab()]; activeTabID = tabs[0].id; restoredLayout = false
    }
    func useThisMac() async {
        if let existing = state.hands.first(where: { $0.kind == "local" && $0.agentId == nil }) { await startHand(existing.id); return }
        let workspace = state.defaults["workspace"].string
        let hand = Hand(id: "mac-\(UUID().uuidString.prefix(8).lowercased())", name: state.defaults["name"].string.isEmpty ? "This Mac" : state.defaults["name"].string, kind: "local", workspace: workspace.isEmpty ? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Nanocodex").path : workspace)
        await saveHand(hand, start: true)
    }
    func saveHand(_ hand: Hand, start: Bool) async {
        do { apply(try await runtime.call("saveHand", [try .encoded(hand)], as: DesktopState.self)); showingHandSetup = false; if start { await startHand(hand.id) } }
        catch { self.error = error.localizedDescription }
    }
    func startHand(_ id: String) async { await handAction("startHand", id) }
    func stopHand(_ id: String) async { await handAction("stopHand", id) }
    func removeHand(_ id: String) async { await handAction("removeHand", id) }
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
    func discoverHands() { if activeTab?.threadId == nil { newTab() }; screen = .chat; Task { await send("Call accountInfo and show my available Hands with their names, exact workspace mounts, and capabilities.", targetOverride: "") } }
    func openAccount() { guard let url = URL(string: state.baseUrl + "/connect") else { return }; NSWorkspace.shared.open(url) }
    func prepareToQuit() async {
        persistence?.cancel()
        try? await cancelPhoneSignIn()
        _ = try? await runtime.request("saveLayout", [try Self.layoutPayload(TabLayout(tabs: tabs, activeTabId: activeTabID, tabPosition: tabPosition, theme: theme), scope: state.accountScope)])
        runtime.stop()
    }
    func shutdown() { persistence?.cancel(); runtime.stop() }
}
