/// Live histories are bounded by payload bytes, not event count: hundreds of
/// small token deltas can belong to one answer. Always retain the newest event.
public enum TranscriptRetention {
    public static func removablePrefixCount(byteCounts: [Int], retainedBytes: Int, byteLimit: Int) -> Int {
        var remaining = retainedBytes, count = 0
        while count + 1 < byteCounts.count, remaining > byteLimit {
            remaining -= byteCounts[count]
            count += 1
        }
        return count
    }
}
