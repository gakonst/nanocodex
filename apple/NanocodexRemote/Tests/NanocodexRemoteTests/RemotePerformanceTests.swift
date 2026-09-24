import XCTest
@testable import NanocodexRemote

final class RemotePerformanceTests: XCTestCase {
    private func video(id: String = "video", _ fields: [String: String]) -> RemoteRTCStatistic {
        .init(id: id, type: "inbound-rtp", values: ["kind": "video", "transportId": "transport", "frameWidth": "1920", "frameHeight": "1080"].merging(fields) { _, new in new })
    }
    private func report(_ time: Double, _ statistics: RemoteRTCStatistic...) -> RemoteRTCReport {
        .init(timestamp: time, statistics: statistics)
    }
    private let before = ["framesDecoded": "10000", "bytesReceived": "50000000", "totalDecodeTime": "100", "jitterBufferDelay": "500", "jitterBufferEmittedCount": "10000", "framesDropped": "100", "packetsReceived": "50000", "packetsLost": "1000"]
    private let after = ["framesDecoded": "10120", "bytesReceived": "52000000", "totalDecodeTime": "100.6", "jitterBufferDelay": "502.4", "jitterBufferEmittedCount": "10120", "framesDropped": "103", "packetsReceived": "50098", "packetsLost": "1002"]

    func testCounterResetAndSSRCReplacementNeedNewBaselines() {
        var accumulator = RemotePerformanceAccumulator()
        _ = accumulator.sample(report(1, video(after)))
        let reset = accumulator.sample(report(2, video(before)))
        XCTAssertNil(reset.decodedFramesPerSecond)
        XCTAssertNil(reset.receiveMegabitsPerSecond)
        XCTAssertNil(reset.jitterBufferMilliseconds)
        XCTAssertNil(reset.droppedFrames)
        XCTAssertNotNil(accumulator.sample(report(3, video(after))).decodedFramesPerSecond)
        let replaced = accumulator.sample(report(4, video(id: "replacement", before)))
        XCTAssertNil(replaced.decodedFramesPerSecond)
        XCTAssertNotNil(accumulator.sample(report(5, video(id: "replacement", after))).decodedFramesPerSecond)
    }

    func testMissingOrInvalidCountersRemainUnknownAndLossCorrectionsCannotGoNegative() {
        var accumulator = RemotePerformanceAccumulator()
        _ = accumulator.sample(report(1, video(before)))
        var fields = after
        fields["bytesReceived"] = "NaN"; fields["jitterBufferDelay"] = nil
        fields["packetsLost"] = "999"; fields["totalDecodeTime"] = "inf"
        fields["frameWidth"] = "1e100"; fields["frameHeight"] = "-1"
        let value = accumulator.sample(report(2, video(fields)))
        XCTAssertNotNil(value.decodedFramesPerSecond)
        XCTAssertNil(value.receiveMegabitsPerSecond)
        XCTAssertNil(value.jitterBufferMilliseconds)
        XCTAssertNil(value.decodeMilliseconds)
        XCTAssertNil(value.packetLossPercent)
        XCTAssertNil(value.width); XCTAssertNil(value.height)
    }

    func testRepeatedBackwardOrLongGapTimestampsDoNotProduceRates() {
        for time in [1.0, 0.5, 7.0, Double.nan, Double.infinity] {
            var accumulator = RemotePerformanceAccumulator()
            _ = accumulator.sample(report(1, video(before)))
            let value = accumulator.sample(report(time, video(after)))
            XCTAssertNil(value.decodedFramesPerSecond)
            XCTAssertNil(value.receiveMegabitsPerSecond)
            XCTAssertNil(value.jitterBufferMilliseconds)
        }
    }

    func testSelectedTransportPairWinsOverOldNominatedPair() {
        let oldPair = RemoteRTCStatistic(id: "old", type: "candidate-pair", values: ["nominated": "true", "currentRoundTripTime": "2"])
        let selected = RemoteRTCStatistic(id: "selected", type: "candidate-pair", values: ["currentRoundTripTime": "0.012", "localCandidateId": "local", "remoteCandidateId": "remote"])
        let transport = RemoteRTCStatistic(id: "transport", type: "transport", values: ["selectedCandidatePairId": "selected"])
        let local = RemoteRTCStatistic(id: "local", type: "local-candidate", values: ["candidateType": "host"])
        let remote = RemoteRTCStatistic(id: "remote", type: "remote-candidate", values: ["candidateType": "relay"])
        var accumulator = RemotePerformanceAccumulator()
        let value = accumulator.sample(report(1, oldPair, video(before), transport, selected, local, remote))
        XCTAssertEqual(value.networkRoundTripMilliseconds, 12)
        XCTAssertEqual(value.route, "Relay")
        let noSelectedPair = accumulator.sample(report(2, oldPair, video(after)))
        XCTAssertNil(noSelectedPair.networkRoundTripMilliseconds, "Nominated does not mean currently selected")
        XCTAssertNil(noSelectedPair.route)
    }

    func testAudioIsExcludedAndMultipleVideoStreamsUseWeightedFrameAverages() throws {
        var accumulator = RemotePerformanceAccumulator()
        let audio = RemoteRTCStatistic(id: "audio", type: "inbound-rtp", values: ["kind": "audio", "bytesReceived": "99999999"])
        _ = accumulator.sample(report(1, video(before), video(id: "second", before), audio))
        var second = after
        second["framesDecoded"] = "10060"; second["totalDecodeTime"] = "100.6"
        let value = accumulator.sample(report(3, video(after), video(id: "second", second), audio))
        XCTAssertEqual(value.decodedFramesPerSecond, 90)
        XCTAssertEqual(value.receiveMegabitsPerSecond, 16)
        XCTAssertEqual(try XCTUnwrap(value.decodeMilliseconds), 1200.0 / 180, accuracy: 0.001)
    }
}
