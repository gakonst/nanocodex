import Foundation

/// Keeps every Send intent attached to the same capture until admission or recovery
/// has finished. Recording and transcription callbacks can complete synchronously,
/// so a terminal result must also be retained for a waiter arriving afterwards.
@MainActor
public final class LockedVoiceCompletion {
    private var result: Result<Void, Error>?
    private var waiters: [CheckedContinuation<Void, Error>] = []

    public init() {}

    public func wait() async throws {
        if let result { return try result.get() }
        try await withCheckedThrowingContinuation { waiters.append($0) }
    }

    public func resolve(_ result: Result<Void, Error>) {
        guard self.result == nil else { return }
        self.result = result
        let pending = waiters
        waiters.removeAll()
        for waiter in pending { waiter.resume(with: result) }
    }
}
