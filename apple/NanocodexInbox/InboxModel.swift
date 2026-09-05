import SwiftUI
import Security
import CryptoKit
import InboxCore

@MainActor
final class InboxModel: ObservableObject {
    enum Filter: String, CaseIterable { case inbox = "Inbox", running = "Running", all = "All" }
    @Published var cards: [AgentCard] = []
    @Published var deck = InboxDeck()
    @Published var filter: Filter = .inbox { didSet { reconcile() } }
    @Published var drafts: [String: String] = [:]
    @Published var rows: [TranscriptRow] = []
    @Published var busy = Set<String>()
    @Published var connection = "Disconnected"
    @Published var error: String?
    @Published var notice: String?
    @Published var connected = false
    @Published var isDemo = false
    @Published var refreshing = false
    @Published var hasOlder = false
    @Published var loadingOlder = false
    @Published var selectedTurn = ""
    @Published var isCreating = false
    private var client: ManagedClient?
    private var polling: Task<Void, Never>?
    private var streaming: Task<Void, Never>?
    private var generation = UUID()
    private var observation = UUID()
    private var events: [AgentEvent] = []
    private var eventBytes: [Int] = []
    private var retainedBytes = 0
    private var projection: Task<Void, Never>?
    private var navigation: [(id: String, seen: String?)] = []
    private var deferred: [String: Cursor] = [:]
    private var cursor = Cursor.zero
    private var olderBefore: Cursor?
    private var seen: [String: String] = [:]
    private var scope = ""
    private var createID: String?
    private var retries: [String: AgentCommand] = [:]
    private var isActive = true
    private var didStart = false
    private var connectionAttempt = UUID()

    var focused: AgentCard? { cards.first { $0.id == deck.focusedID } }
    var attentionCount: Int { cards.filter { $0.needsAttention(seen: seenCursor($0.id)) }.count }
    var runningCount: Int { cards.filter(\.isRunning).count }
    var focusedTurn: String { focused?.activeTurns.contains(selectedTurn) == true ? selectedTurn : focused?.activeTurns.first ?? "" }
    var canGoBack: Bool { !navigation.isEmpty }
    var canRetry: Bool { focused.flatMap { retries[$0.id] }?.kind == .followUp }
    var draft: String {
        get { focused.flatMap { drafts[$0.id] } ?? "" }
        set { guard let id = focused?.id else { return }; drafts[id] = newValue; persist() }
    }

    func start() async {
        guard !didStart else { return }; didStart = true
        if ProcessInfo.processInfo.arguments.contains("--demo") { demo(); return }
        if let saved = KeychainAccount.read() {
            do { try await connect(origin: saved.origin, key: saved.apiKey) }
            catch { self.error = error.localizedDescription }
        }
    }
    func connect(origin: String, key: String) async throws {
        let attempt = UUID(); connectionAttempt = attempt
        let credential = try AccountCredential(origin: origin.trimmingCharacters(in: .whitespacesAndNewlines), apiKey: key.trimmingCharacters(in: .whitespacesAndNewlines))
        let candidate = ManagedClient(credential: credential)
        let initial: [AgentCard]
        do {
            initial = try await candidate.list()
            guard connectionAttempt == attempt else { candidate.close(); throw CancellationError() }
            try KeychainAccount.save(credential)
        } catch { candidate.close(); throw error }
        reset()
        client = candidate
        scope = SHA256.hash(data: Data((credential.origin + ":" + String(credential.apiKey.prefix(21))).utf8)).map { String(format: "%02x", $0) }.joined()
        drafts = UserDefaults.standard.dictionary(forKey: "inbox.drafts." + scope) as? [String: String] ?? [:]
        seen = UserDefaults.standard.dictionary(forKey: "inbox.seen." + scope) as? [String: String] ?? [:]
        cards = initial; connected = true; connection = "Connecting"; reconcile(); resume()
    }
    func disconnect() throws {
        try KeychainAccount.remove()
        reset()
    }
    private func reset() {
        connectionAttempt = UUID(); generation = UUID(); observation = UUID(); polling?.cancel(); streaming?.cancel(); client?.close(); client = nil
        projection?.cancel(); projection = nil; eventBytes = []; retainedBytes = 0; navigation = []; deferred = [:]
        connected = false; isDemo = false; cards = []; deck = InboxDeck(); rows = []; events = []; drafts = [:]; seen = [:]
        scope = ""; error = nil; notice = nil; busy = []; retries = [:]; createID = nil; isCreating = false; refreshing = false
        hasOlder = false; loadingOlder = false; connection = "Disconnected"
    }
    func setActive(_ active: Bool) {
        isActive = active
        if active { resume() }
        else { polling?.cancel(); streaming?.cancel(); observation = UUID(); connection = "Paused" }
    }
    private func resume() {
        guard connected, !isDemo, isActive else { return }
        polling?.cancel()
        let epoch = generation
        polling = Task { [weak self] in
            while !Task.isCancelled {
                guard let self, self.generation == epoch else { return }
                await self.refresh()
                do { try await Task.sleep(for: .seconds(15)) } catch { return }
            }
        }
        observeFocused()
    }
    func refresh() async {
        guard let client, !refreshing else { return }
        let epoch = generation
        refreshing = true
        defer { if generation == epoch { refreshing = false } }
        do {
            let listing = try await client.list()
            guard generation == epoch, !Task.isCancelled else { return }
            let retained = Dictionary(uniqueKeysWithValues: cards.map { ($0.id, $0) })
            cards = listing.map { summary in
                var card = retained[summary.id] ?? summary
                card.title = summary.title; card.updatedAt = summary.updatedAt; card.turnCount = summary.turnCount
                return card
            }
            reconcile()
            // Bound fan-out: inactive cards are sampled; only the visible agent streams.
            for offset in stride(from: 0, to: listing.count, by: 4) {
                if Task.isCancelled { return }
                let ids = listing[offset..<min(offset + 4, listing.count)].map(\.id)
                let updates = await withTaskGroup(of: CardUpdate.self) { group in
                    for id in ids {
                        group.addTask {
                            do {
                                async let state = client.state(id)
                                async let history = client.history(id)
                                return try await CardUpdate(id: id, state: state, page: history, failure: nil)
                            } catch { return CardUpdate(id: id, state: nil, page: nil, failure: error.localizedDescription) }
                        }
                    }
                    var result: [CardUpdate] = []
                    for await value in group { result.append(value) }
                    return result
                }
                guard generation == epoch, !Task.isCancelled else { return }
                for update in updates {
                    guard let index = cards.firstIndex(where: { $0.id == update.id }) else { continue }
                    if let state = update.state, let page = update.page {
                        do { try cards[index].apply(state: state); cards[index].apply(events: page.events) }
                        catch { cards[index].error = error.localizedDescription }
                    } else { cards[index].error = update.failure }
                }
                reconcile()
            }
            prioritizeNext()
        } catch {
            guard generation == epoch, !Task.isCancelled else { return }
            self.error = error.localizedDescription
        }
    }
    private func seenCursor(_ id: String) -> Cursor? { seen[id].flatMap { Cursor(rawValue: $0) } }
    private func reconcile() {
        let previous = deck.focusedID
        let eligible = cards.filter { card in
            switch filter {
            case .inbox: return card.isRunning || !card.checked || card.latestCursor > (seenCursor(card.id) ?? .zero)
            case .running: return card.isRunning
            case .all: return true
            }
        }.sorted { a, b in
            let ar = a.needsAttention(seen: seenCursor(a.id)), br = b.needsAttention(seen: seenCursor(b.id))
            return ar != br ? ar : a.updatedAt != b.updatedAt ? a.updatedAt > b.updatedAt : a.id < b.id
        }
        deck.reconcile(eligible.map(\.id))
        if previous != deck.focusedID { observeFocused() }
    }
    private func prioritizeNext() {
        let visible = Set(deck.order)
        let ranked = cards.filter { visible.contains($0.id) }.sorted { a, b in
            let ad = deferred[a.id] == a.latestCursor, bd = deferred[b.id] == b.latestCursor
            if ad != bd { return !ad }
            let ar = a.needsAttention(seen: seenCursor(a.id)), br = b.needsAttention(seen: seenCursor(b.id))
            if ar != br { return ar }
            let ai = deck.order.firstIndex(of: a.id) ?? 0, bi = deck.order.firstIndex(of: b.id) ?? 0
            return ai < bi
        }
        deck.prioritize(ranked.map(\.id))
    }
    func advance(reviewed: Bool) {
        guard let card = focused else { return }
        navigation.append((card.id, seen[card.id])); if navigation.count > 50 { navigation.removeFirst() }
        if reviewed { seen[card.id] = card.latestCursor.rawValue; persist() }
        deferred[card.id] = card.latestCursor
        prioritizeNext()
        deck.advance(reviewed: reviewed ? card.latestCursor : nil)
        reconcile(); observeFocused()
        notice = reviewed ? "Update marked seen" : "Saved for another pass"
    }
    func back() {
        guard let previous = navigation.popLast() else { return }
        seen[previous.id] = previous.seen; deferred.removeValue(forKey: previous.id); persist(); reconcile(); deck.focus(previous.id); observeFocused()
    }
    func select(_ id: String) { filter = .all; deck.focus(id); observeFocused() }
    private func observeFocused() {
        streaming?.cancel(); projection?.cancel(); projection = nil; observation = UUID(); loadingOlder = false; rows = []; events = []; eventBytes = []; retainedBytes = 0; cursor = .zero; olderBefore = nil; hasOlder = false; selectedTurn = ""
        guard let id = deck.focusedID else { return }
        if isDemo { rows = DemoContent.rows(id); connection = "Demo"; return }
        guard let client, isActive else { return }
        let epoch = generation, token = observation
        connection = "Connecting"
        streaming = Task { [weak self] in
            guard let self else { return }
            var delay = 1
            var loaded = false
            while !Task.isCancelled, self.generation == epoch, self.observation == token {
                do {
                    if !loaded {
                        let page = try await client.history(id)
                        guard self.generation == epoch, self.observation == token, !Task.isCancelled else { return }
                        self.events = page.events; self.hasOlder = page.hasMore; self.measureEvents(); self.cursor = page.latest; self.olderBefore = self.events.first?.cursor
                        self.rows = transcript(self.events); loaded = true
                    }
                    try await client.stream(id, after: self.cursor) { [weak self] frame in
                        await self?.receive(frame, id: id, epoch: epoch, token: token)
                    }
                    delay = 1
                } catch {
                    guard !Task.isCancelled, self.generation == epoch, self.observation == token else { return }
                    if let apiError = error as? APIError, apiError == .http(401) || apiError == .http(403) {
                        self.connection = "Sign in again"; self.error = apiError.localizedDescription; return
                    }
                }
                guard !Task.isCancelled else { return }
                self.connection = "Reconnecting"
                do { try await Task.sleep(for: .seconds(delay)) } catch { return }
                delay = min(delay * 2, 15)
            }
        }
    }
    private func receive(_ frame: SSEFrame, id: String, epoch: UUID, token: UUID) {
        guard generation == epoch, observation == token else { return }
        if let event = frame.event, event.cursor > cursor {
            events.append(event)
            let bytes = (try? JSONEncoder().encode(event.data).count) ?? 0
            eventBytes.append(bytes); retainedBytes += bytes
            // Token arrival never re-encodes the complete transcript on the main actor.
            while events.count > 1 && (events.count > 512 || retainedBytes > 16 * 1024 * 1024) {
                events.removeFirst(); retainedBytes -= eventBytes.removeFirst(); hasOlder = true
            }
            olderBefore = events.first?.cursor
            if projection == nil {
                projection = Task { [weak self] in
                    do { try await Task.sleep(for: .milliseconds(100)) } catch { return }
                    guard let self, self.generation == epoch, self.observation == token else { return }
                    self.rows = transcript(self.events)
                    if let index = self.cards.firstIndex(where: { $0.id == id }) { self.cards[index].apply(events: self.events) }
                    self.projection = nil
                }
            }
        }
        if let position = frame.cursor { cursor = max(cursor, position) }
        connection = "Live"
    }
    private func measureEvents() {
        eventBytes = events.map { (try? JSONEncoder().encode($0.data).count) ?? 0 }
        retainedBytes = eventBytes.reduce(0, +)
        while events.count > 1 && retainedBytes > 16 * 1024 * 1024 {
            events.removeFirst(); retainedBytes -= eventBytes.removeFirst(); hasOlder = true
        }
    }
    func loadOlder() async {
        guard let client, let id = focused?.id, let before = olderBefore, hasOlder, !loadingOlder else { return }
        let token = observation
        loadingOlder = true
        defer { if token == observation { loadingOlder = false } }
        do {
            let page = try await client.history(id, before: before)
            guard token == observation else { return }
            let known = Set(events.map(\.cursor.rawValue))
            events.insert(contentsOf: page.events.filter { !known.contains($0.cursor.rawValue) }, at: 0)
            // Older history is explicitly loaded; cap at 2,048 events and explain the limit.
            if events.count > 2048 { events = Array(events.suffix(2048)); notice = "History limit reached. Open the web conversation for earlier messages."; hasOlder = false }
            else { hasOlder = page.hasMore }
            measureEvents(); olderBefore = events.first?.cursor; rows = transcript(events)
            if olderBefore == before { hasOlder = false; notice = "History limit reached. Earlier messages remain available on the service." }
        } catch { if token == observation { self.error = error.localizedDescription } }
    }
    func send(steer: Bool) async {
        guard let card = focused, !busy.contains(card.id), !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        let command: AgentCommand
        if !steer, let retry = retries[card.id], retry.kind == .followUp, retry.input == draft { command = retry }
        else { command = AgentCommand(agentID: card.id, turnID: focusedTurn, input: draft, kind: steer ? .steer : .followUp) }
        await perform(command)
    }
    func stop(agentID: String, turnID: String) async {
        guard !turnID.isEmpty else { return }
        await perform(AgentCommand(agentID: agentID, turnID: turnID, kind: .stop))
    }
    func retry() async { if let id = focused?.id, let command = retries[id], command.kind == .followUp { await perform(command) } }
    private func perform(_ command: AgentCommand) async {
        guard !busy.contains(command.agentID) else { return }
        if isDemo {
            if command.kind == .stop, let index = cards.firstIndex(where: { $0.id == command.agentID }) { cards[index].activeTurns = []; cards[index].status = "Stopped" }
            else { rows.append(.init(id: UUID().uuidString, role: "You", text: command.input)); drafts[command.agentID] = "" }
            notice = "Demo action · " + (command.kind == .steer ? "Direction updated" : command.kind == .stop ? "Stopped" : "Follow-up queued")
            return
        }
        guard let client else { return }
        let epoch = generation
        busy.insert(command.agentID); error = nil
        defer { if generation == epoch { busy.remove(command.agentID) } }
        do {
            try await client.command(command)
            guard generation == epoch else { return }
            if command.kind != .stop, drafts[command.agentID] == command.input { drafts[command.agentID] = ""; persist() }
            retries.removeValue(forKey: command.agentID)
            notice = command.kind == .steer ? "Direction sent" : command.kind == .stop ? "Stop requested" : "Follow-up accepted"
            await refresh()
        } catch {
            guard generation == epoch else { return }
            if command.kind == .followUp { retries[command.agentID] = command }
            self.error = error.localizedDescription + (command.kind == .followUp ? " Retry the same follow-up to avoid sending it twice." : " The action was not confirmed; check the latest state before trying again.")
        }
    }
    func newAgent() async {
        guard !isCreating else { return }
        if isDemo { notice = "Connect your account to start a real agent."; return }
        guard let client else { return }
        let epoch = generation
        isCreating = true
        defer { if generation == epoch { isCreating = false } }
        let requestID = createID ?? UUID().uuidString; createID = requestID
        do {
            let id = try await client.create(requestID: requestID)
            guard generation == epoch else { return }
            createID = nil
            cards.insert(AgentCard(id: id, title: "New agent"), at: 0)
            select(id)
        } catch { if generation == epoch { self.error = error.localizedDescription } }
    }
    private func persist() {
        guard !isDemo, !scope.isEmpty else { return }
        UserDefaults.standard.set(drafts, forKey: "inbox.drafts." + scope)
        UserDefaults.standard.set(seen, forKey: "inbox.seen." + scope)
    }
    func demo() {
        reset(); isDemo = true; connected = true; connection = "Demo"
        cards = DemoContent.cards(); reconcile(); observeFocused()
    }
}

private struct CardUpdate: Sendable { let id: String; let state: JSON?; let page: EventPage?; let failure: String? }

private enum KeychainAccount {
    private static let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: "xyz.paradigm.nanocodex.inbox", kSecAttrAccount as String: "managed"]
    static func read() -> AccountCredential? {
        var q = query; q[kSecReturnData as String] = true; q[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        guard SecItemCopyMatching(q as CFDictionary, &result) == errSecSuccess, let data = result as? Data else { return nil }
        return try? JSONDecoder().decode(AccountCredential.self, from: data)
    }
    static func save(_ credential: AccountCredential) throws {
        let data = try JSONEncoder().encode(credential)
        let status = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecSuccess { return }
        guard status == errSecItemNotFound else { throw KeychainError(status: status) }
        var q = query; q[kSecValueData as String] = data; q[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let result = SecItemAdd(q as CFDictionary, nil)
        guard result == errSecSuccess else { throw KeychainError(status: result) }
    }
    static func remove() throws {
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw KeychainError(status: status) }
    }
    private struct KeychainError: LocalizedError {
        let status: OSStatus
        var errorDescription: String? { "The account could not be saved securely (\(status)). Try again." }
    }
}
