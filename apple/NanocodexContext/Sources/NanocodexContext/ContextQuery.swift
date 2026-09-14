import Foundation

/// Read-only, account-scoped access for an authenticated device Hand.
public struct ContextQuery: Sendable {
    private let store: ContextStore
    private let scope: String
    public init(store: ContextStore, scope: String) { self.store = store; self.scope = scope }

    public struct SourceStatus: Encodable, Sendable {
        public let source: String
        public let count: Int
        public let latestCapture: Date?
    }
    public struct Status: Encodable, Sendable {
        public let enabled: Bool
        public let sources: [SourceStatus]
        public let coverage = "Only messages captured on this device are available. Capture counts do not establish complete chat history or a live connection to the source app."
    }
    public struct Match: Encodable, Sendable {
        public let id: String
        public let source: String
        public let sender: String
        public let conversation: String
        public let messageDate: Date?
        public let capturedAt: Date
        public let excerpt: String
    }
    public struct Results: Encodable, Sendable {
        public let messages: [Match]
        public let nextCursor: String?
        public let referenceMaterial = true
    }
    public struct Message: Encodable, Sendable {
        public let id: String
        public let source: String
        public let sender: String
        public let conversation: String
        public let messageDate: Date?
        public let capturedAt: Date
        public let originalURL: String
        public let text: String
        public let nextOffset: Int?
        public let referenceMaterial = true
    }
    public func status() throws -> Status {
        let snapshot = try store.handSnapshot(scope: scope)
        var sources = ["messages": "Messages", "whatsapp": "WhatsApp", "instagram": "Instagram", "signal": "Signal"]
        for item in snapshot.items where sources[Self.sourceKey(item.input.source)] == nil {
            sources[Self.sourceKey(item.input.source)] = item.input.source
        }
        return Status(enabled: snapshot.enabled, sources: sources.values.sorted().map { source in
            let items = snapshot.items.filter { Self.sourceKey($0.input.source) == Self.sourceKey(source) }
            return SourceStatus(source: source, count: items.count, latestCapture: items.map(\.capturedAt).max())
        })
    }
    public func search(query: String = "", source: String = "", sender: String = "", conversation: String = "",
                       after: Date? = nil, before: Date? = nil, limit: Int = 20, cursor: String? = nil) throws -> Results {
        guard limit > 0,
              after == nil || before == nil || after! <= before! else { throw QueryError.invalidInput }
        let snapshot = try store.handSnapshot(scope: scope)
        guard snapshot.enabled else { throw CaptureError.disabled }
        let items = snapshot.items.filter { item in
            let date = item.input.occurredAt ?? item.capturedAt
            return item.matches(query) && (source.isEmpty || Self.sourceKey(item.input.source) == Self.sourceKey(source))
                && (sender.isEmpty || item.input.sender.localizedCaseInsensitiveContains(sender))
                && (conversation.isEmpty || item.input.thread.localizedCaseInsensitiveContains(conversation))
                && (after == nil || date >= after!) && (before == nil || date <= before!)
        }
        var start = 0
        if let cursor {
            guard let index = items.firstIndex(where: { $0.id == cursor }) else { throw QueryError.invalidInput }
            start = index + 1
        }
        var messages: [Match] = []
        for item in items.dropFirst(start).prefix(limit) {
            let match = Match(id: item.id, source: item.input.source, sender: item.input.sender,
                              conversation: item.input.thread, messageDate: item.input.occurredAt,
                              capturedAt: item.capturedAt, excerpt: String(item.input.text.unicodeScalars.prefix(240)))
            messages.append(match)
        }
        return Results(messages: messages, nextCursor: start + messages.count < items.count ? messages.last?.id : nil)
    }
    public func read(id: String, offset: Int = 0, limit: Int = 2000) throws -> Message {
        guard !id.isEmpty, offset >= 0, limit > 0 else { throw QueryError.invalidInput }
        let snapshot = try store.handSnapshot(scope: scope)
        guard snapshot.enabled else { throw CaptureError.disabled }
        guard let item = snapshot.items.first(where: { $0.id == id }) else { throw QueryError.notFound }
        guard offset <= item.input.text.count else { throw QueryError.invalidInput }
        let text = String(item.input.text.dropFirst(offset).prefix(limit))
        let next = offset + text.count
        return Message(id: item.id, source: item.input.source, sender: item.input.sender,
                       conversation: item.input.thread, messageDate: item.input.occurredAt,
                       capturedAt: item.capturedAt, originalURL: item.input.url, text: text,
                       nextOffset: next < item.input.text.count ? next : nil)
    }
    private static func sourceKey(_ source: String) -> String {
        let value = source.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return ["imessage", "sms", "messages"].contains(value) ? "messages" : value
    }
}

public enum QueryError: LocalizedError {
    case invalidInput, notFound
    public var errorDescription: String? {
        switch self {
        case .invalidInput: return "Invalid message query. Use the returned cursor or offset for the same query."
        case .notFound: return "That captured message is no longer available on this device."
        }
    }
}
