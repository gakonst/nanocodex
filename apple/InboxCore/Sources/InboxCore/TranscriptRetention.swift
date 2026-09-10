/// Live histories are bounded by payload bytes, not event count: hundreds of
/// small token deltas can belong to one answer. Always retain the newest event.
public enum TranscriptRetention {
    /// Cached tabs share one budget. The focused tab is owned separately.
    public static func cachedPrefixCount(byteCounts: [Int], byteLimit: Int, countLimit: Int) -> Int {
        var remaining = byteCounts.reduce(0, +), count = 0
        while count < byteCounts.count && (remaining > byteLimit || byteCounts.count - count > countLimit) {
            remaining -= byteCounts[count]
            count += 1
        }
        return count
    }

    public static func removablePrefixCount(byteCounts: [Int], retainedBytes: Int, byteLimit: Int) -> Int {
        var remaining = retainedBytes, count = 0
        while count + 1 < byteCounts.count, remaining > byteLimit {
            remaining -= byteCounts[count]
            count += 1
        }
        return count
    }
    /// Backward paging evicts the opposite edge. Even a single oversized event
    /// remains readable; eviction never changes whether history exists.
    public static func removableSuffixCount(byteCounts: [Int], retainedBytes: Int, byteLimit: Int) -> Int {
        var remaining = retainedBytes, count = 0
        while count + 1 < byteCounts.count, remaining > byteLimit {
            remaining -= byteCounts[byteCounts.count - 1 - count]
            count += 1
        }
        return count
    }

}
