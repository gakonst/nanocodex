import Foundation

/// Bounded handoff from WebRTC's callback thread to the main actor. Only adjacent
/// disposable motion records may replace each other; reliable records are barriers.
final class RemoteInputMailbox: @unchecked Sendable {
    struct Packet: Sendable {
        let data: Data
        let motion: Bool
        let hover: RemoteInput?
    }
    enum Batch {
        case packets([Packet])
        case idle
        case overflow
    }
    private let lock = NSLock()
    private var packets: [Packet] = []
    private var bytes = 0
    private var scheduled = false
    private var failed = false
    private var closed = false
    private let maximumPackets = 256
    private let maximumBytes = 262_144

    /// True means the caller must schedule the single consumer. Overflow fails
    /// the whole connection instead of silently dropping a release or displacement.
    func append(_ data: Data, motion: Bool) -> Bool {
        let input = data.count <= 8192 && motion ? try? RemoteInput.decode(data) : nil
        let hover = input?.kind == .move ? input : nil
        lock.lock(); defer { lock.unlock() }
        guard !closed, !failed else { return false }
        if data.count > 8192 {
            failed = true
        } else {
            if let hover, let last = packets.last, let previous = last.hover,
               previous.generation == hover.generation {
                // The motion channel is unordered. A late older position must
                // not replace the newest queued position for this generation.
                guard hover.sequence > previous.sequence else { return false }
                bytes -= last.data.count
                packets.removeLast()
            }
            if packets.count >= maximumPackets || bytes + data.count > maximumBytes {
                failed = true
            } else {
                packets.append(Packet(data: data, motion: motion, hover: hover))
                bytes += data.count
            }
        }
        if failed { packets.removeAll(); bytes = 0 }
        guard !scheduled else { return false }
        scheduled = true
        return true
    }

    /// Keep each UI turn bounded even while input continues to arrive.
    func take() -> Batch {
        lock.lock(); defer { lock.unlock() }
        if failed { return .overflow }
        guard !packets.isEmpty else { scheduled = false; return .idle }
        let count = min(32, packets.count)
        let batch = Array(packets.prefix(count))
        packets.removeFirst(count)
        bytes -= batch.reduce(0) { $0 + $1.data.count }
        return .packets(batch)
    }

    func close() {
        lock.lock(); defer { lock.unlock() }
        closed = true; failed = false; packets.removeAll(); bytes = 0
    }
}
