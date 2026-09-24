import Foundation

/// Recognition requests are short-lived; only their text and position survive
/// completion. This ledger allows out-of-order results without retaining audio.
public struct MeetingSegmentPolicy {
    public static let segmentSeconds: TimeInterval = 45
    public static let maxSealedPending = 3
    private var pieces: [String] = []
    private var pending: Set<Int> = []
    public init() {}
    @discardableResult public mutating func begin() -> Int {
        let index = pieces.count
        pieces.append("")
        pending.insert(index)
        return index
    }
    public mutating func update(_ index: Int, text: String) {
        guard pending.contains(index) else { return }
        pieces[index] = text
    }
    public mutating func settle(_ index: Int) { pending.remove(index) }
    public var unfinished: Int { pending.count }
    public var transcript: String { pieces.filter { !$0.isEmpty }.joined(separator: "\n") }
    public func canRotate(sealedPending: Int) -> Bool { sealedPending < Self.maxSealedPending }
}
