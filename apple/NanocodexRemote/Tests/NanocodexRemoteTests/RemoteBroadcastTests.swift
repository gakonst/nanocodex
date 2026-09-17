import XCTest
@testable import NanocodexRemote
#if os(macOS)
import AVFoundation
import CoreVideo
import CoreGraphics

final class RemoteBroadcastTests: XCTestCase {
    func testProfilesAndSecretSafeValidation() throws {
        for preset in ["source", "1080p", "720p", "twitch", "x"] {
            let config = try RemoteBroadcastConfiguration(destination: "rtmp://localhost/live/key", preset: preset)
            let (w, h) = config.dimensions(width: 640, height: 480)
            XCTAssertEqual(w, 640); XCTAssertEqual(h, 480)
            let args = MacBroadcast.arguments(width: w, height: h, destination: config.destination, preset: preset, audioPath: "/audio")
            XCTAssertTrue(args.contains("h264_videotoolbox")); XCTAssertTrue(args.contains("aac"))
            // Only video uses arrival timestamps; PCM must retain its sample clock.
            XCTAssertEqual(args.filter { $0 == "-use_wallclock_as_timestamps" }.count, 1)
            XCTAssertEqual(args[args.firstIndex(of: "-allow_sw")! + 1], "0")
        }
        let config = try RemoteBroadcastConfiguration(destination: "rtmps://localhost/live/key", preset: "source")
        let dimensions = config.dimensions(width: 6016, height: 3384)
        XCTAssertEqual(dimensions.0, 3840); XCTAssertEqual(dimensions.1, 2160)
        for url in ["https://example.com/secret", "rtmp://user:secret@example.com/live", "rtmp://example.com/live\nsecret"] {
            XCTAssertThrowsError(try RemoteBroadcastConfiguration(destination: url, preset: "source")) { error in
                XCTAssertFalse(error.localizedDescription.contains("secret"))
            }
        }
    }

    @MainActor func testNativeScreenCapturePublisher() async throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_RTMP_TEST_NATIVE"] == "1",
              let destination = ProcessInfo.processInfo.environment["NANOCODEX_RTMP_TEST_URL"] else {
            throw XCTSkip("Opt in with NANOCODEX_RTMP_TEST_NATIVE=1 through loopback harness")
        }
        let publisher = MacBroadcast()
        publisher.onStatus = { print("RTMP_NATIVE_STATE \($0)") }
        let preset = ProcessInfo.processInfo.environment["NANOCODEX_RTMP_TEST_PRESET"] ?? "1080p"
        try await publisher.start(surfaceID: "display-\(CGMainDisplayID())",
            configuration: RemoteBroadcastConfiguration(destination: destination, preset: preset))
        let seconds = Double(ProcessInfo.processInfo.environment["NANOCODEX_RTMP_TEST_SECONDS"] ?? "10") ?? 10
        try await Task.sleep(for: .seconds(seconds))
        await publisher.stop()
    }

    /// Product sink, physical hardware encoder and real RTMP socket. A parent
    /// harness independently checks the received codecs, duration and decoding.
    @MainActor func testLoopbackPublisher() async throws {
        guard let destination = ProcessInfo.processInfo.environment["NANOCODEX_RTMP_TEST_URL"] else {
            throw XCTSkip("Run through scripts/test-hand-rtmp.py")
        }
        let seconds = Double(ProcessInfo.processInfo.environment["NANOCODEX_RTMP_TEST_SECONDS"] ?? "10") ?? 10
        let width = Int(ProcessInfo.processInfo.environment["NANOCODEX_RTMP_TEST_WIDTH"] ?? "1920") ?? 1920
        let height = Int(ProcessInfo.processInfo.environment["NANOCODEX_RTMP_TEST_HEIGHT"] ?? "1080") ?? 1080
        let preset = ProcessInfo.processInfo.environment["NANOCODEX_RTMP_TEST_PRESET"] ?? "1080p"
        let publisher = MacBroadcast()
        publisher.onStatus = { print("RTMP_STATE \(ProcessInfo.processInfo.systemUptime) \($0)") }
        let config = try RemoteBroadcastConfiguration(destination: destination, preset: preset)
        try publisher.startFrames(width: width, height: height, audio: true, configuration: config)
        let format = AVAudioFormat(standardFormatWithSampleRate: 48000, channels: 2)!
        let clock = ContinuousClock(), started = clock.now
        var index = 0
        var audioFrames: Int64 = 0
        while started.duration(to: clock.now) < .seconds(seconds) {
            var frame: CVPixelBuffer?
            XCTAssertEqual(CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_32BGRA,
                [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &frame), kCVReturnSuccess)
            let image = try XCTUnwrap(frame)
            CVPixelBufferLockBaseAddress(image, [])
            let base = try XCTUnwrap(CVPixelBufferGetBaseAddress(image))
            base.initializeMemory(as: UInt8.self, repeating: UInt8(index % 255), count: CVPixelBufferGetBytesPerRow(image) * height)
            CVPixelBufferUnlockBaseAddress(image, [])
            publisher.appendPhoneFrame(image)
            let elapsed = started.duration(to: clock.now).components
            let expectedAudio = Int64(elapsed.seconds) * 48000 + Int64(Double(elapsed.attoseconds) / 1e18 * 48000)
            let sampleCount = max(800, Int(expectedAudio - audioFrames))
            let audio = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(sampleCount))!
            audio.frameLength = AVAudioFrameCount(sampleCount)
            for channel in 0..<2 { for i in 0..<sampleCount {
                audio.floatChannelData![channel][i] = Float(sin(Double(audioFrames + Int64(i)) * 2 * .pi * 440 / 48000) * 0.2)
            } }
            var sample: CMSampleBuffer?
            var timing = CMSampleTimingInfo(duration: CMTime(value: 1, timescale: 48000), presentationTimeStamp: CMTime(value: audioFrames, timescale: 48000), decodeTimeStamp: .invalid)
            XCTAssertEqual(CMSampleBufferCreate(allocator: kCFAllocatorDefault, dataBuffer: nil, dataReady: false, makeDataReadyCallback: nil, refcon: nil,
                formatDescription: format.formatDescription, sampleCount: sampleCount, sampleTimingEntryCount: 1, sampleTimingArray: &timing,
                sampleSizeEntryCount: 0, sampleSizeArray: nil, sampleBufferOut: &sample), noErr)
            let audioSample = try XCTUnwrap(sample)
            XCTAssertEqual(CMSampleBufferSetDataBufferFromAudioBufferList(audioSample, blockBufferAllocator: kCFAllocatorDefault,
                blockBufferMemoryAllocator: kCFAllocatorDefault, flags: 0, bufferList: audio.audioBufferList), noErr)
            CMSampleBufferSetDataReady(audioSample)
            if publisher.appendAudioSample(audioSample) { audioFrames += Int64(sampleCount) }
            index += 1
            try await clock.sleep(until: started.advanced(by: .nanoseconds(Int64(index) * 1_000_000_000 / 60)))
        }
        await publisher.stop()
    }
}
#endif
