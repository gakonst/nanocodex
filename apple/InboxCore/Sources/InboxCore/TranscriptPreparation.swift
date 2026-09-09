import Foundation

/// Expensive projection and payload accounting run on the cooperative executor.
/// Callers own observation identity and must validate it before publishing results.
public enum TranscriptPreparation {
    public static func rows(_ events: [AgentEvent]) async throws -> [TranscriptRow] {
        let task = Task.detached(priority: .userInitiated) {
            assert(!Thread.isMainThread)
            try Task.checkCancellation()
            let rows = transcript(events)
            try Task.checkCancellation()
            return rows
        }
        return try await withTaskCancellationHandler(operation: { try await task.value }, onCancel: { task.cancel() })
    }

    public static func byteCounts(_ events: [AgentEvent]) async throws -> [Int] {
        let task = Task.detached(priority: .userInitiated) {
            assert(!Thread.isMainThread)
            let encoder = JSONEncoder()
            return try events.map { event in
                try Task.checkCancellation()
                return try encoder.encode(event.data).count
            }
        }
        return try await withTaskCancellationHandler(operation: { try await task.value }, onCancel: { task.cancel() })
    }
}

/// Enqueue synchronously so writes retain submission order even across accounts.
/// Encoding and UserDefaults serialization execute only on this serial queue.
public final class InboxPreferencesWriter: @unchecked Sendable {
    private let queue = DispatchQueue(label: "xyz.paradigm.nanocodex.inbox-preferences", qos: .utility)
    private let suiteName: String?

    public init(suiteName: String? = nil) { self.suiteName = suiteName }

    public func enqueue(_ write: @escaping @Sendable (UserDefaults) -> Void) {
        queue.async { [suiteName] in
            assert(!Thread.isMainThread)
            write(suiteName.flatMap(UserDefaults.init(suiteName:)) ?? .standard)
        }
    }

    /// Await this before sending a durable command or finishing background work.
    public func flush() async {
        await withCheckedContinuation { continuation in
            queue.async { continuation.resume() }
        }
    }
}
