import Foundation

/// Receiver measurements. These describe decoding and transport, not display
/// presentation or input-to-photon latency; no synchronized host clock is assumed.
public struct RemotePerformance: Equatable, Sendable {
    public internal(set) var connectionMilliseconds: Double?
    public internal(set) var firstDecodedFrameMilliseconds: Double?
    public internal(set) var decodedFramesPerSecond: Double?
    public internal(set) var receiveMegabitsPerSecond: Double?
    public internal(set) var networkRoundTripMilliseconds: Double?
    public internal(set) var jitterBufferMilliseconds: Double?
    public internal(set) var decodeMilliseconds: Double?
    public internal(set) var droppedFrames: Double?
    public internal(set) var packetLossPercent: Double?
    public internal(set) var width: Int?
    public internal(set) var height: Int?
    public internal(set) var route: String?
    public internal(set) var controlBufferedBytes: UInt64?
    public internal(set) var motionBufferedBytes: UInt64?

    public init() {}
}

// Copy only the fields we use out of WebRTC's callback. No candidate addresses,
// credentials or ObjC objects are retained by the sampler or exposed to the UI.
struct RemoteRTCStatistic: Sendable {
    let id: String
    let type: String
    let values: [String: String]

    func number(_ key: String) -> Double? {
        guard let text = values[key], let value = Double(text), value.isFinite else { return nil }
        return value
    }
}

struct RemoteRTCReport: Sendable {
    let timestamp: TimeInterval
    let statistics: [RemoteRTCStatistic]
}

struct RemotePerformanceAccumulator {
    private var previous: RemoteRTCReport?

    mutating func sample(_ report: RemoteRTCReport) -> RemotePerformance {
        defer { previous = report.timestamp.isFinite ? report : nil }
        var result = RemotePerformance()
        let video = report.statistics.filter { $0.type == "inbound-rtp" && ($0.values["kind"] ?? $0.values["mediaType"]) == "video" && $0.number("framesDecoded") != nil }
        let oldVideo = previous?.statistics.filter { $0.type == "inbound-rtp" && ($0.values["kind"] ?? $0.values["mediaType"]) == "video" && $0.number("framesDecoded") != nil } ?? []
        let oldByID = Dictionary(oldVideo.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        let seconds = previous.map { report.timestamp - $0.timestamp } ?? 0
        // A new SSRC, reset, stalled clock, or long gap needs a fresh baseline.
        // Never turn missing fields into zero, nor use lifetime FPS/averages.
        let validInterval = seconds.isFinite && seconds > 0 && seconds <= 5 && !video.isEmpty && Set(video.map(\.id)) == Set(oldVideo.map(\.id))
        func delta(_ stat: RemoteRTCStatistic, _ key: String) -> Double? {
            guard validInterval, let value = stat.number(key), let old = oldByID[stat.id]?.number(key), value >= old else { return nil }
            return value - old
        }
        func sumDelta(_ key: String) -> Double? {
            guard validInterval else { return nil }
            var total = 0.0
            for stat in video {
                guard let value = delta(stat, key) else { return nil }
                total += value
            }
            return total.isFinite ? total : nil
        }
        func finite(_ value: Double) -> Double? { value.isFinite && value >= 0 ? value : nil }
        func milliseconds(_ totalKey: String, _ countKey: String) -> Double? {
            guard let total = sumDelta(totalKey), let count = sumDelta(countKey), count > 0 else { return nil }
            return finite(total / count * 1000)
        }
        if let frames = sumDelta("framesDecoded") { result.decodedFramesPerSecond = finite(frames / seconds) }
        if let bytes = sumDelta("bytesReceived") { result.receiveMegabitsPerSecond = finite(bytes / 1_000_000 * 8 / seconds) }
        result.jitterBufferMilliseconds = milliseconds("jitterBufferDelay", "jitterBufferEmittedCount")
        result.decodeMilliseconds = milliseconds("totalDecodeTime", "framesDecoded")
        result.droppedFrames = sumDelta("framesDropped")
        // WebRTC may correct packetsLost downward when reordered packets arrive.
        // Such an interval is unknown, not a negative loss percentage.
        if let lost = sumDelta("packetsLost"), let received = sumDelta("packetsReceived"), lost + received > 0 {
            result.packetLossPercent = lost / (lost + received) * 100
        }
        let active = video.max { lhs, rhs in
            (delta(lhs, "framesDecoded") ?? lhs.number("framesDecoded") ?? 0) < (delta(rhs, "framesDecoded") ?? rhs.number("framesDecoded") ?? 0)
        }
        func dimension(_ key: String) -> Int? {
            guard let value = active?.number(key), value > 0, value <= 32_768 else { return nil }
            return Int(value)
        }
        result.width = dimension("frameWidth"); result.height = dimension("frameHeight")
        let byID = Dictionary(report.statistics.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        // A nominated pair can remain in stats after ICE switches paths. Only
        // the transport's selected pair is evidence of the current network RTT.
        let transportID = active?.values["transportId"]
        let transport = transportID.flatMap { byID[$0] } ?? report.statistics.first { $0.type == "transport" && $0.values["selectedCandidatePairId"] != nil }
        if let pairID = transport?.values["selectedCandidatePairId"], let pair = byID[pairID], pair.type == "candidate-pair" {
            if let rtt = pair.number("currentRoundTripTime"), rtt >= 0 { result.networkRoundTripMilliseconds = finite(rtt * 1000) }
            let candidates = ["localCandidateId", "remoteCandidateId"].compactMap { pair.values[$0].flatMap { byID[$0]?.values["candidateType"] } }
            if candidates.contains("relay") { result.route = "Relay" }
            else if candidates.count == 2 { result.route = "Direct" }
        }
        return result
    }
}
