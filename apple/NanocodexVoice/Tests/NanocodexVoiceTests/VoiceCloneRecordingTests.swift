#if os(iOS) && DEBUG
import AVFoundation
import Combine
import XCTest
@testable import NanocodexVoice

final class VoiceCloneRecordingTests: XCTestCase {
    /// Constructs a recorder and a short silent PCM fixture, but never starts audio I/O.
    private func fixture() throws -> AVAudioRecorder {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("clone-test-\(UUID().uuidString).wav")
        let recorder = try AVAudioRecorder(url: url, settings: [
            AVFormatIDKey: kAudioFormatLinearPCM, AVSampleRateKey: 8000,
            AVNumberOfChannelsKey: 1, AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsFloatKey: false, AVLinearPCMIsBigEndianKey: false,
        ])
        var wav = Data()
        func word<T: FixedWidthInteger>(_ value: T) {
            var little = value.littleEndian
            withUnsafeBytes(of: &little) { wav.append(contentsOf: $0) }
        }
        wav.append(contentsOf: "RIFF".utf8); word(UInt32(196))
        wav.append(contentsOf: "WAVEfmt ".utf8); word(UInt32(16))
        word(UInt16(1)); word(UInt16(1)); word(UInt32(8000))
        word(UInt32(16000)); word(UInt16(2)); word(UInt16(16))
        wav.append(contentsOf: "data".utf8); word(UInt32(160))
        wav.append(Data(repeating: 0, count: 160))
        try wav.write(to: url)
        return recorder
    }

    @MainActor private func finish(_ model: VoiceCloneRecording, recorder: AVAudioRecorder, success: Bool) async {
        let completed = expectation(description: "Saving finishes through delegate")
        let subscription = model.$saving.dropFirst().filter { !$0 }.prefix(1).sink { _ in completed.fulfill() }
        model.audioRecorderDidFinishRecording(recorder, successfully: success)
        await fulfillment(of: [completed], timeout: 1)
        withExtendedLifetime(subscription) {}
    }

    @MainActor func testStopWaitsForDelegateBeforePublishingSample() async throws {
        let model = VoiceCloneRecording()
        let recorder = try fixture()
        defer { model.discard(); try? FileManager.default.removeItem(at: recorder.url) }
        model.prepareRecordingForTesting(recorder)
        model.stop()
        XCTAssertFalse(model.recording)
        XCTAssertTrue(model.saving)
        XCTAssertNil(model.sample)
        model.stop()
        XCTAssertTrue(model.saving)
        await finish(model, recorder: recorder, success: true)
        XCTAssertFalse(model.saving)
        XCTAssertEqual(model.sample, recorder.url)
        XCTAssertGreaterThan(model.duration, 0)
    }

    @MainActor func testFailedReplacementPreservesReviewedSample() async throws {
        let model = VoiceCloneRecording()
        let original = try fixture()
        let replacement = try fixture()
        defer {
            model.discard()
            try? FileManager.default.removeItem(at: original.url)
            try? FileManager.default.removeItem(at: replacement.url)
        }
        model.prepareRecordingForTesting(original)
        model.stop()
        await finish(model, recorder: original, success: true)
        let originalDuration = model.duration
        model.prepareRecordingForTesting(replacement)
        XCTAssertEqual(model.sample, original.url)
        model.stop()
        await finish(model, recorder: replacement, success: false)
        XCTAssertFalse(model.recording)
        XCTAssertFalse(model.saving)
        XCTAssertEqual(model.sample, original.url)
        XCTAssertEqual(model.duration, originalDuration)
        XCTAssertTrue(FileManager.default.fileExists(atPath: original.url.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: replacement.url.path))
    }
}
#endif
