#if canImport(Combine)
import Foundation
import Combine

/// The host supplies an already authorized project roster. A project label alone
/// grants no access. Implementations must enforce this roster for every operation.
public struct ProjectConversationScope: Equatable, Sendable {
    public let projectID: String
    public let conversations: [ProjectConversation]
    public init(projectID: String, conversations: [ProjectConversation]) {
        self.projectID = projectID
        var seen = Set<String>()
        self.conversations = conversations.filter { !$0.id.isEmpty && seen.insert($0.id).inserted }
    }
}

public struct ProjectConversation: Identifiable, Equatable, Sendable {
    public let id: String
    public let title: String
    public init(id: String, title: String) { self.id = id; self.title = title }
}

/// Public frame construction lets independent transports and test doubles replay
/// both durable events and cursor-only checkpoints without an SSE parser.
public struct ProjectConversationFrame: Sendable {
    public let event: AgentEvent?
    public let cursor: Cursor?
    public init(event: AgentEvent? = nil, cursor: Cursor? = nil) {
        self.event = event; self.cursor = cursor
    }
}

@MainActor public protocol ProjectConversationTransport {
    var scope: ProjectConversationScope { get }
    func state(_ id: String) async throws -> JSON
    func history(_ id: String, before: Cursor?, after: Cursor?) async throws -> EventPage
    /// Must propagate cancellation to the underlying foreground connection.
    func stream(_ id: String, after: Cursor, receive: @escaping @Sendable (ProjectConversationFrame) async -> Void) async throws
    func send(_ command: AgentCommand) async throws
}

public enum ProjectConversationConnection: Equatable, Sendable {
    case suspended, loading, streaming, disconnected
}

public struct ProjectConversationPending: Equatable, Sendable {
    public let command: AgentCommand
    public fileprivate(set) var isSending: Bool
    /// A failed write may have been accepted. Only explicit retry reuses its ID.
    public fileprivate(set) var error: String?
}

/// Headless, text-first foreground conversation state. Inject this into a host's
/// SwiftUI view builders. No account-wide discovery, creation, uploads, steering,
/// background reconnect loop or durable draft persistence is performed here.
/// Replacing authorization requires a new transport/store. Call suspend when the
/// host leaves the foreground. Older paging pauses live delivery until resume.
@MainActor public final class ProjectConversationStore: ObservableObject {
    public let scope: ProjectConversationScope
    public var roster: [ProjectConversation] { scope.conversations }
    @Published public private(set) var cards: [AgentCard]
    public var activeTurns: [String] { cards.first { $0.id == selection }?.activeTurns ?? [] }
    public var items: [ConversationItem] { ConversationItem.group(rows, activeTurns: activeTurns) }
    @Published public private(set) var selection: String?
    @Published public private(set) var rows: [TranscriptRow] = []
    @Published public private(set) var drafts: [String: String] = [:]
    @Published public private(set) var pending: [String: ProjectConversationPending] = [:]
    @Published public private(set) var isLoading = false
    @Published public private(set) var isLoadingOlder = false
    @Published public private(set) var hasOlder = false
    @Published public private(set) var error: String?
    @Published public private(set) var connection: ProjectConversationConnection = .suspended
    private let transport: any ProjectConversationTransport
    private let byteLimit: Int
    private var events: [AgentEvent] = []
    private var cursor: Cursor = .zero
    private var generation = UUID()
    private var streamTask: Task<Void, Never>?
    private var foreground = true
    private var historyTask: Task<Void, Never>?
    private var projectionTask: Task<Void, Never>?
    private var projector = TranscriptStreamProjection()
    private var sizes: [Int] = []

    public init(transport: any ProjectConversationTransport, byteLimit: Int = 2 * 1024 * 1024) {
        self.transport = transport; scope = transport.scope; self.byteLimit = max(1, byteLimit)
        cards = transport.scope.conversations.map { AgentCard(id: $0.id, title: $0.title) }
    }
    deinit { streamTask?.cancel(); historyTask?.cancel(); projectionTask?.cancel() }

    public func setDraft(_ text: String, for id: String) {
        guard roster.contains(where: { $0.id == id }) else { return }
        drafts[id] = text
    }

    public func select(_ id: String?) async {
        guard id == nil || roster.contains(where: { $0.id == id }) else { return }
        invalidate()
        selection = id; events = []; sizes = []; projector = TranscriptStreamProjection(); rows = []; cursor = .zero; hasOlder = false; error = nil
        guard let id else { connection = .suspended; return }
        await beginLatest(id)
    }

    public func suspend() {
        foreground = false; invalidate(); connection = .suspended
    }

    /// Reload the newest bounded page and replay SSE strictly after its cursor.
    /// This also exits backward browsing without leaving holes in the projection.
    public func resume() async {
        foreground = true; invalidate()
        guard let selection else { return }
        await beginLatest(selection)
    }

    public func jumpToLatest() async { await resume() }

    private func beginLatest(_ id: String) async {
        let token = generation
        let task = Task { await loadLatest(id, token: token) }; historyTask = task
        await withTaskCancellationHandler(operation: { await task.value }, onCancel: { task.cancel() })
    }

    private func invalidate() {
        historyTask?.cancel(); historyTask = nil; projectionTask?.cancel(); projectionTask = nil
        generation = UUID(); streamTask?.cancel(); streamTask = nil
        isLoading = false; isLoadingOlder = false
    }
    private func current(_ token: UUID, _ id: String) -> Bool { generation == token && selection == id }

    private func loadLatest(_ id: String, token: UUID) async {
        guard current(token, id), !Task.isCancelled else { return }
        isLoading = true; connection = .loading; error = nil
        defer { if current(token, id) { isLoading = false; if Task.isCancelled { connection = .suspended } } }
        do {
            let state = try await transport.state(id)
            guard current(token, id) else { return }
            if let index = cards.firstIndex(where: { $0.id == id }) {
                try cards[index].apply(state: state)
                reconcilePending(id, events: [])
            }
            let page = try await transport.history(id, before: nil, after: nil)
            guard current(token, id) else { return }
            guard !Task.isCancelled else { isLoading = false; connection = .suspended; return }
            events = page.events; cursor = page.events.last?.cursor ?? .zero; hasOlder = page.hasMore
            let counts = try await TranscriptPreparation.byteCounts(page.events)
            guard current(token, id), !Task.isCancelled else { return }
            sizes = counts
            trim(older: false)
            let projected = try await projector.rows(events)
            guard current(token, id), !Task.isCancelled else { return }
            rows = projected; applyCard(id, events: page.events); isLoading = false
            if foreground { startStream(id, token: token) } else { connection = .suspended }
        } catch {
            guard current(token, id) else { return }
            isLoading = false; self.error = error.localizedDescription; connection = .disconnected
        }
    }

    private static func retryable(_ error: Error) -> Bool {
        if let error = error as? APIError {
            if case .http(let code) = error { return code == 429 || code >= 500 }
            return false
        }
        return error is URLError && (error as? URLError)?.code != .cancelled
    }
    private func startStream(_ id: String, token: UUID) {
        let transport = transport
        streamTask = Task { [weak self] in
            var delay: UInt64 = 250_000_000
            while self?.current(token, id) == true, self?.foreground == true, !Task.isCancelled {
                guard let after = self?.cursor else { return }
                self?.connection = .streaming
                do {
                    try await transport.stream(id, after: after) { [weak self] frame in
                        await self?.receive(frame, id: id, token: token)
                    }
                } catch {
                    guard let self, self.current(token, id), !Task.isCancelled else { return }
                    self.error = error.localizedDescription
                    self.connection = .disconnected
                    guard Self.retryable(error) else { return }
                }
                guard self?.current(token, id) == true, !Task.isCancelled else { return }
                self?.connection = .disconnected
                do { try await Task.sleep(nanoseconds: delay) } catch { return }
                delay = min(delay * 2, 8_000_000_000)
            }
        }
    }
    private func receive(_ frame: ProjectConversationFrame, id: String, token: UUID) async {
        guard current(token, id), foreground else { return }
        error = nil
        if let event = frame.event, event.cursor > cursor {
            guard let counts = try? await TranscriptPreparation.byteCounts([event]), current(token, id), foreground, event.cursor > cursor else { return }
            events.append(event); sizes.append(contentsOf: counts); cursor = event.cursor
            trim(older: false); applyCard(id, events: [event])
            scheduleProjection(id, token: token)
        }
        if let checkpoint = frame.cursor, checkpoint > cursor { cursor = checkpoint }
    }
    private func scheduleProjection(_ id: String, token: UUID) {
        guard projectionTask == nil else { return }
        projectionTask = Task { [weak self] in
            do { try await Task.sleep(nanoseconds: 16_000_000) } catch { return }
            guard let self, self.current(token, id) else { return }
            let snapshot = self.events
            guard let rows = try? await self.projector.rows(snapshot), self.current(token, id), !Task.isCancelled else { return }
            self.rows = rows; self.projectionTask = nil
            if let event = snapshot.last { self.applyCard(id, events: [event]) }
            if snapshot.last?.cursor != self.events.last?.cursor { self.scheduleProjection(id, token: token) }
        }
    }
    private func applyCard(_ id: String, events: [AgentEvent]) {
        if let index = cards.firstIndex(where: { $0.id == id }) { cards[index].apply(events: events, transcriptRows: rows) }
        reconcilePending(id, events: events)
    }
    private func reconcilePending(_ id: String, events: [AgentEvent]) {
        guard let item = pending[id] else { return }
        if cards.first(where: { $0.id == id })?.activeTurns.contains(item.command.requestID) == true || events.contains(where: { $0.turnID == item.command.requestID && $0.type == "turn_accepted" }) { pending[id] = nil }
    }

    public func loadOlder() async {
        guard !isLoading, !isLoadingOlder else { return }
        let token = generation
        let task = Task {
            guard generation == token, !Task.isCancelled else { return }
            await loadOlderPage()
        }; historyTask = task
        await withTaskCancellationHandler(operation: { await task.value }, onCancel: { task.cancel() })
    }
    private func loadOlderPage() async {
        guard !isLoading, !isLoadingOlder, hasOlder, let id = selection, let before = events.first?.cursor else { return }
        generation = UUID(); streamTask?.cancel(); streamTask = nil
        projectionTask?.cancel(); projectionTask = nil; connection = .suspended
        let token = generation
        isLoadingOlder = true; error = nil
        defer { if current(token, id) { isLoadingOlder = false } }
        do {
            let page = try await transport.history(id, before: before, after: nil)
            guard current(token, id) else { return }
            guard !Task.isCancelled else { isLoadingOlder = false; return }
            let older = page.events.filter { $0.cursor < before }
            let counts = try await TranscriptPreparation.byteCounts(older)
            guard current(token, id), !Task.isCancelled else { return }
            events = older + events; sizes = counts + sizes; hasOlder = page.hasMore && !older.isEmpty
            trim(older: true)
            let projected = try await projector.rows(events)
            guard current(token, id), !Task.isCancelled else { return }
            rows = projected; isLoadingOlder = false
        } catch {
            guard current(token, id) else { return }
            isLoadingOlder = false; self.error = error.localizedDescription
        }
    }

    private func trim(older: Bool) {
        let bytes = sizes.reduce(0, +)
        if older {
            let count = TranscriptRetention.removableSuffixCount(byteCounts: sizes, retainedBytes: bytes, byteLimit: byteLimit)
            if count > 0 { events.removeLast(count); sizes.removeLast(count) }
        } else {
            let count = TranscriptRetention.removablePrefixCount(byteCounts: sizes, retainedBytes: bytes, byteLimit: byteLimit)
            if count > 0 { events.removeFirst(count); sizes.removeFirst(count); hasOlder = true }
        }
    }

    public func stop() async {
        guard let id = selection, let turn = activeTurns.first else { return }
        let command = AgentCommand(agentID: id, turnID: turn, kind: .stop)
        do { try await transport.send(command) } catch { self.error = error.localizedDescription }
    }

    public func send() async {
        guard let id = selection, pending[id] == nil, let text = drafts[id], !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        let command = AgentCommand(agentID: id, input: text, kind: .followUp)
        pending[id] = .init(command: command, isSending: true)
        drafts[id] = ""
        await perform(command)
    }
    public func retryPending(for id: String) async {
        guard let item = pending[id], !item.isSending else { return }
        pending[id]?.isSending = true; pending[id]?.error = nil
        await perform(item.command)
    }
    private func perform(_ command: AgentCommand) async {
        do {
            try await transport.send(command)
            pending[command.agentID]?.isSending = false
        } catch {
            pending[command.agentID]?.isSending = false
            pending[command.agentID]?.error = error.localizedDescription
        }
    }
}
#endif
