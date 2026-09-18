import Foundation

/// Main-actor-owned pending writes. Coalesce only adjacent moves in the same
/// control generation; every other message is an ordering barrier.
struct RemoteSendQueue {
    private var messages: [RemoteMessage] = []
    var count: Int { messages.count }

    mutating func append(_ message: RemoteMessage, limit: Int = 128) -> Bool {
        if message.type == "input", case .input(let move) = message.data, move.kind == .move,
           let previous = messages.last, previous.type == "input",
           case .input(let prior) = previous.data, prior.kind == .move,
           prior.generation == move.generation {
            if move.sequence > prior.sequence { messages[messages.count - 1] = message }
            return true
        }
        guard messages.count < limit else { return false }
        messages.append(message)
        return true
    }

    mutating func popFirst() -> RemoteMessage? {
        // The queue is bounded to 128; no long-lived task chain retains drained
        // messages, and a move burst occupies a single pending slot.
        messages.isEmpty ? nil : messages.removeFirst()
    }
}
