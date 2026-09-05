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
    @Published var threadLoading = false
    @Published var threadError: String?
    private var observedAgentID: String?
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
    @Published var pending: [PendingMessage] = []
    private var pinnedThreadID: String?
    private var demoRows: [String: [TranscriptRow]] = [:]
    private var demoFaults = Set<String>()
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
    var controllableTurns: [String] { (focused?.activeTurns ?? []).filter { id in !focusedPending.contains { $0.id == id } } }
    var focusedTurn: String { controllableTurns.contains(selectedTurn) ? selectedTurn : controllableTurns.first ?? "" }
    var hasUnconfirmedMessage: Bool { focusedPending.contains { $0.phase == .failed } }
    var focusedPending: [PendingMessage] { pending.filter { $0.agentID == focused?.id } }
    func openThread() {
        pinnedThreadID = focused?.id
        if isDemo, ProcessInfo.processInfo.environment["NANOCODEX_DEMO_LONG_THREAD"] == "1", let id = focused?.id {
            hasOlder = true
            let epoch = generation
            Task {
                try? await Task.sleep(for: .seconds(12))
                guard generation == epoch, focused?.id == id, !rows.contains(where: { $0.id == "live-tail" }) else { return }
                rows.append(.init(id: "live-tail", role: "Agent", text: "Live update arrived while you were reading."))
                demoRows[id] = rows
            }
        }
        if isDemo, ProcessInfo.processInfo.environment["NANOCODEX_DEMO_FINISH_IN_THREAD"] == "1",
           let id = focused?.id, !focusedTurn.isEmpty {
            let turn = focusedTurn, epoch = generation
            Task {
                try? await Task.sleep(for: .milliseconds(1000))
                guard generation == epoch else { return }
                demoFinish(agentID: id, turnID: turn)
            }
        }
    }
    func closeThread() { pinnedThreadID = nil; reconcile() }
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
        restorePending()
        cards = initial; connected = true; connection = "Connecting"; reconcile(); resume()
    }
    func disconnect() throws {
        try KeychainAccount.remove()
        reset()
    }
    private func reset() {
        connectionAttempt = UUID(); generation = UUID(); observation = UUID(); polling?.cancel(); streaming?.cancel(); client?.close(); client = nil
        projection?.cancel(); projection = nil; eventBytes = []; retainedBytes = 0; navigation = []; deferred = [:]
        observedAgentID = nil; threadLoading = false; threadError = nil
        connected = false; isDemo = false; cards = []; deck = InboxDeck(); rows = []; events = []; drafts = [:]; seen = [:]
        scope = ""; error = nil; notice = nil; busy = []; retries = [:]; createID = nil; isCreating = false; refreshing = false
        hasOlder = false; loadingOlder = false; connection = "Disconnected"; pending = []; pinnedThreadID = nil; demoRows = [:]; demoFaults = []
    }
    func setActive(_ active: Bool) {
        isActive = active
        if active { if isDemo { connection = "Demo" } else { resume() } }
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
        observeFocused(restart: true)
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
                        do {
                            try cards[index].apply(state: state); cards[index].apply(events: page.events)
                            reconcilePending(id: update.id, events: page.events, state: cards[index])
                        }
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
            if card.id == pinnedThreadID { return true }
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
        notice = nil
    }
    func back() {
        guard let previous = navigation.popLast() else { return }
        seen[previous.id] = previous.seen; deferred.removeValue(forKey: previous.id); persist(); reconcile(); deck.focus(previous.id); observeFocused()
    }
    func select(_ id: String) { filter = .all; deck.focus(id); observeFocused() }
    private func observeFocused(restart: Bool = false) {
        let changed = observedAgentID != deck.focusedID
        guard changed || restart else { return }
        observedAgentID = deck.focusedID
        streaming?.cancel(); projection?.cancel(); projection = nil; observation = UUID(); loadingOlder = false
        threadError = nil
        if changed {
            rows = []; events = []; eventBytes = []; retainedBytes = 0; cursor = .zero
            olderBefore = nil; hasOlder = false; selectedTurn = ""
        }
        guard let id = deck.focusedID else { threadLoading = false; return }
        if isDemo { rows = demoRows[id] ?? DemoContent.rows(id); connection = "Demo"; threadLoading = false; return }
        threadLoading = rows.isEmpty
        guard let client, isActive else { return }
        let epoch = generation, token = observation
        connection = "Connecting"
        streaming = Task { [weak self] in
            guard let self else { return }
            var delay = 1
            var loaded = !self.events.isEmpty
            while !Task.isCancelled, self.generation == epoch, self.observation == token {
                do {
                    if !loaded {
                        let page = try await client.history(id)
                        guard self.generation == epoch, self.observation == token, !Task.isCancelled else { return }
                        self.events = page.events; self.hasOlder = page.hasMore; self.measureEvents(); self.cursor = page.latest; self.olderBefore = self.events.first?.cursor
                        self.rows = transcript(self.events); self.threadLoading = false; self.reconcilePending(id: id, events: self.events); loaded = true
                    }
                    try await client.stream(id, after: self.cursor) { [weak self] frame in
                        await self?.receive(frame, id: id, epoch: epoch, token: token)
                    }
                    delay = 1
                } catch {
                    guard !Task.isCancelled, self.generation == epoch, self.observation == token else { return }
                    if let apiError = error as? APIError, apiError == .http(401) || apiError == .http(403) {
                        self.connection = "Sign in again"; self.error = apiError.localizedDescription; self.threadError = apiError.localizedDescription; self.threadLoading = false; return
                    }
                }
                guard !Task.isCancelled else { return }
                self.threadLoading = false
                if !loaded { self.threadError = "Could not load this conversation. Reconnecting…" }
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
            reconcilePending(id: id, events: [event])
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
        connection = "Live"; threadError = nil; threadLoading = false
    }
    private func measureEvents() {
        eventBytes = events.map { (try? JSONEncoder().encode($0.data).count) ?? 0 }
        retainedBytes = eventBytes.reduce(0, +)
        while events.count > 1 && retainedBytes > 16 * 1024 * 1024 {
            events.removeFirst(); retainedBytes -= eventBytes.removeFirst(); hasOlder = true
        }
    }
    func loadOlder() async {
        if isDemo, ProcessInfo.processInfo.environment["NANOCODEX_DEMO_LONG_THREAD"] == "1", let id = focused?.id, hasOlder {
            guard !loadingOlder else { return }
            loadingOlder = true
            try? await Task.sleep(for: .milliseconds(600))
            guard focused?.id == id else { return }
            rows.insert(contentsOf: (-12..<0).map { .init(id: "older-\($0)", role: "Agent", text: "Earlier note \($0 + 13). Context retained before the current work.") }, at: 0)
            demoRows[id] = rows; hasOlder = false; loadingOlder = false
            return
        }
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
        } catch { if token == observation { self.threadError = error.localizedDescription } }
    }
    // Reserve identity and the busy slot synchronously at the tap, before a swipe
    // or another tap can change focus. The server owns the queued follow-up.
    func appendVoiceDraft(_ text: String, agentID: String) {
        let spoken = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !spoken.isEmpty, cards.contains(where: { $0.id == agentID }) else { return }
        let existing = drafts[agentID] ?? ""
        drafts[agentID] = existing + (existing.isEmpty ? "" : "\n") + spoken
        persist()
    }
    func send() {
        guard let card = focused, !busy.contains(card.id), !hasUnconfirmedMessage else { return }
        let input = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !input.isEmpty else { return }
        let predecessor = focusedPending.last?.id ?? focusedTurn
        let message = PendingMessage(agentID: card.id, input: input, predecessor: predecessor)
        pending.append(message); drafts[card.id] = ""; busy.insert(card.id); notice = nil; persist()
        let epoch = generation
        Task { await submit(message, epoch: epoch) }
    }
    func retryPending(_ id: String) {
        guard let index = pending.firstIndex(where: { $0.id == id }), pending[index].phase == .failed,
              !busy.contains(pending[index].agentID) else { return }
        pending[index].phase = .submitting; pending[index].error = nil
        let message = pending[index], epoch = generation
        busy.insert(message.agentID); persist()
        Task { await submit(message, epoch: epoch) }
    }
    private func submit(_ message: PendingMessage, epoch: UUID) async {
        defer { if generation == epoch { busy.remove(message.agentID) } }
        do {
            let receipt = try await execute(message.submission)
            guard generation == epoch else { return }
            guard receipt["turn_id"].string == message.id else { throw APIError.invalidResponse }
            if let index = pending.firstIndex(where: { $0.id == message.id }) {
                pending[index].phase = .queued
                pending[index].acceptedCursor = Cursor(rawValue: receipt["cursor"].string.isEmpty ? receipt["accepted_cursor"].string : receipt["cursor"].string)
            }
            if ["completed", "cancelled", "failed"].contains(receipt["state"].string) {
                pending.removeAll { $0.id == message.id }
            }
            persist()
            if isDemo {
                demoAdmit(message)
                notice = nil
            } else { await refresh() }
        } catch {
            guard generation == epoch else { return }
            if let index = pending.firstIndex(where: { $0.id == message.id }) {
                pending[index].phase = .failed
                pending[index].error = "Delivery unconfirmed. Retry checks the same message; it won't create another one."
                persist()
            }
        }
    }
    func steerNow(_ id: String) {
        guard let message = pending.first(where: { $0.id == id }),
              pending.first(where: { $0.agentID == message.agentID })?.id == id,
              let command = message.interruption, !busy.contains(message.agentID) else { return }
        controlPending(message, command: command, phase: .starting)
    }
    func cancelPending(_ id: String) {
        guard let message = pending.first(where: { $0.id == id }), !busy.contains(message.agentID) else { return }
        controlPending(message, command: AgentCommand(agentID: message.agentID, turnID: message.id, kind: .stop), phase: .cancelling)
    }
    private func controlPending(_ message: PendingMessage, command: AgentCommand, phase: PendingMessage.Phase) {
        guard let index = pending.firstIndex(where: { $0.id == message.id }) else { return }
        pending[index].phase = phase; pending[index].error = nil; busy.insert(message.agentID); persist()
        let epoch = generation
        Task {
            defer { if generation == epoch { busy.remove(message.agentID) } }
            do {
                let receipt = try await execute(command)
                guard generation == epoch else { return }
                if command.turnID == message.id, ["completed", "cancelled", "failed"].contains(receipt["state"].string) {
                    removeCancelledPending(message.id); persist()
                }
                if isDemo { demoFinish(agentID: message.agentID, turnID: command.turnID) }
                else { await refresh() }
                // An acknowledgement means cancellation was requested, not that
                // it finished. Keep the row until its own start/terminal event.
            } catch {
                guard generation == epoch else { return }
                if let index = pending.firstIndex(where: { $0.id == message.id }) {
                    pending[index].phase = message.phase
                    pending[index].error = "Cancellation unconfirmed. The queued message is retained; try again."
                    persist()
                }
            }
        }
    }
    private func removeCancelledPending(_ id: String) {
        guard let cancelled = pending.first(where: { $0.id == id }) else { return }
        for index in pending.indices where pending[index].agentID == cancelled.agentID && pending[index].predecessor == id {
            pending[index].predecessor = cancelled.predecessor
        }
        pending.removeAll { $0.id == id }
    }
    private func reconcilePending(id: String, events: [AgentEvent], state: AgentCard? = nil) {
        let previousCount = pending.count
        for event in events where event.type == "turn_cancelled" {
            if pending.contains(where: { $0.agentID == id && $0.id == event.turnID }) { removeCancelledPending(event.turnID) }
        }
        pending.removeAll { message in
            message.agentID == id && (message.hasStarted(in: events)
                || state.map { message.hasFinished(activeTurns: $0.activeTurns, stateCursor: $0.stateCursor) } == true)
        }
        if pending.count != previousCount { persist() }
    }
    private func restorePending() {
        if let data = UserDefaults.standard.data(forKey: "inbox.pending." + scope),
           let saved = try? JSONDecoder().decode([PendingMessage].self, from: data) {
            pending = saved
            for index in pending.indices { pending[index].restore() }
        }
    }
    private func execute(_ command: AgentCommand) async throws -> JSON {
        if isDemo {
            let delay = Int(ProcessInfo.processInfo.environment["NANOCODEX_DEMO_DELAY_MS"] ?? "200") ?? 200
            try await Task.sleep(for: .milliseconds(delay))
            let fault = command.kind == .stop ? "cancel" : "submit"
            if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_FAIL_ONCE"] == fault, demoFaults.insert(fault).inserted {
                throw APIError.http(503)
            }
            return .object(["turn_id": .string(command.kind == .followUp ? command.requestID : command.turnID), "state": .string(command.kind == .stop ? "cancelling" : "accepted")])
        }
        guard let client else { throw APIError.invalidResponse }
        return try await client.command(command)
    }
    private func demoAdmit(_ message: PendingMessage) {
        guard let index = cards.firstIndex(where: { $0.id == message.agentID }) else { return }
        if !cards[index].activeTurns.contains(message.id) { cards[index].activeTurns.append(message.id) }
        cards[index].status = "Running"
        var history = demoRows[message.agentID] ?? DemoContent.rows(message.agentID)
        if !history.contains(where: { $0.id == message.id }) { history.append(.init(id: message.id, role: "You", text: message.input)) }
        demoRows[message.agentID] = history
        if focused?.id == message.agentID { rows = history }
        if message.predecessor.isEmpty { pending.removeAll { $0.id == message.id } }
        else {
            let epoch = generation
            let delay = Int(ProcessInfo.processInfo.environment["NANOCODEX_DEMO_COMPLETE_AFTER_MS"] ?? "15000") ?? 15000
            Task {
                try? await Task.sleep(for: .milliseconds(delay))
                guard generation == epoch, cards.contains(where: { $0.id == message.agentID && $0.activeTurns.contains(message.predecessor) }) else { return }
                demoFinish(agentID: message.agentID, turnID: message.predecessor)
            }
        }
        persist()
    }
    private func demoFinish(agentID: String, turnID: String) {
        guard let index = cards.firstIndex(where: { $0.id == agentID }) else { return }
        cards[index].activeTurns.removeAll { $0 == turnID }
        // Cancelling a queued item must not start its successor while an older
        // turn is still running. Rebase the successor onto that older turn.
        let wasQueued = pending.contains { $0.agentID == agentID && $0.id == turnID }
        if wasQueued { removeCancelledPending(turnID) }
        if let next = pending.first(where: { $0.agentID == agentID && $0.predecessor == turnID && $0.phase != .failed && $0.phase != .submitting }) {
            pending.removeAll { $0.id == next.id }
            var history = demoRows[agentID] ?? DemoContent.rows(agentID)
            history.append(.init(id: "started-" + next.id, role: "Agent", text: "Working on: " + next.input, running: true))
            demoRows[agentID] = history; cards[index].preview = "Working on: " + next.input
            if focused?.id == agentID { rows = history }
        }
        cards[index].status = cards[index].isRunning ? "Running" : "Stopped"
        persist(); reconcile()
    }
    func stop(agentID: String, turnID: String) async {
        guard !turnID.isEmpty else { return }
        await perform(AgentCommand(agentID: agentID, turnID: turnID, kind: .stop))
    }
    func retry() async { if let id = focused?.id, let command = retries[id], command.kind == .followUp { await perform(command) } }
    private func perform(_ command: AgentCommand) async {
        guard !busy.contains(command.agentID) else { return }
        let epoch = generation
        busy.insert(command.agentID); error = nil
        defer { if generation == epoch { busy.remove(command.agentID) } }
        do {
            _ = try await execute(command)
            guard generation == epoch else { return }
            if isDemo, command.kind == .stop { demoFinish(agentID: command.agentID, turnID: command.turnID) }
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
        if isDemo {
            let id = "demo-" + UUID().uuidString
            var card = AgentCard(id: id, title: "New agent")
            card.checked = true; card.status = "Idle"; card.preview = "Send a message to begin."
            cards.insert(card, at: 0); demoRows[id] = []; select(id)
            return
        }
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
        guard !scope.isEmpty else { return }
        UserDefaults.standard.set(drafts, forKey: "inbox.drafts." + scope)
        UserDefaults.standard.set(seen, forKey: "inbox.seen." + scope)
        if let data = try? JSONEncoder().encode(pending) { UserDefaults.standard.set(data, forKey: "inbox.pending." + scope) }
        if isDemo {
            if let data = try? JSONEncoder().encode(demoRows) { UserDefaults.standard.set(data, forKey: "inbox.demoRows." + scope) }
            UserDefaults.standard.set(Dictionary(uniqueKeysWithValues: cards.map { ($0.id, $0.activeTurns) }), forKey: "inbox.demoTurns." + scope)
        }
    }
    func demo() {
        reset(); isDemo = true; connected = true; connection = "Demo"
        cards = DemoContent.cards()
        if let profile = ProcessInfo.processInfo.environment["NANOCODEX_DEMO_PROFILE"] {
            scope = "demo." + profile
            restorePending()
            drafts = UserDefaults.standard.dictionary(forKey: "inbox.drafts." + scope) as? [String: String] ?? [:]
            if let data = UserDefaults.standard.data(forKey: "inbox.demoRows." + scope) { demoRows = (try? JSONDecoder().decode([String: [TranscriptRow]].self, from: data)) ?? [:] }
            if let turns = UserDefaults.standard.dictionary(forKey: "inbox.demoTurns." + scope) as? [String: [String]] {
                for index in cards.indices { cards[index].activeTurns = turns[cards[index].id] ?? cards[index].activeTurns }
            }
        }
        reconcile(); observeFocused()
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
