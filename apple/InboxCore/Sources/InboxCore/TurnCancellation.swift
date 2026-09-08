import Foundation

/// Observation cadence only: this never expires or completes a persisted Stop.
/// A new foreground task starts with an immediate request; repeated unchanged
/// receipts back off, while meaningful server progress restores the fast check.
public struct TurnCancellationPollSchedule: Sendable {
    private var previous: [JSON]?
    private var seconds = 0
    public init() {}
    public mutating func delay(after receipt: JSON) -> Duration {
        let progress = [receipt["state"], receipt["updated_at"], receipt["attempt_count"]]
        seconds = previous == progress ? min(15, seconds * 2) : 1
        previous = progress
        return .seconds(seconds)
    }
}

extension PendingTurnCancellation {
    public func isTerminal(receipt: JSON) -> Bool {
        receipt["turn_id"].string == turnID
            && ["completed", "cancelled", "failed"].contains(receipt["state"].string)
    }
}

/// Replacement on foreground wakes a sleeping poll immediately. An old task
/// may finish after its replacement, so only its own token may remove a slot.
@MainActor
public final class TurnCancellationTasks {
    private struct Entry { let token: UUID; let task: Task<Void, Never> }
    private var entries: [String: Entry] = [:]
    public init() {}
    public func contains(_ id: String) -> Bool { entries[id] != nil }
    public func start(_ id: String, restart: Bool = false, operation: @escaping @MainActor () async -> Void) {
        if let previous = entries[id] {
            guard restart else { return }
            previous.task.cancel()
        }
        let token = UUID()
        let task = Task {
            defer { if entries[id]?.token == token { entries[id] = nil } }
            guard !Task.isCancelled else { return }
            await operation()
        }
        entries[id] = Entry(token: token, task: task)
    }
    public func cancel(_ id: String) { entries.removeValue(forKey: id)?.task.cancel() }
    public func cancelAll() {
        let previous = entries.values
        entries = [:]
        for entry in previous { entry.task.cancel() }
    }
}
