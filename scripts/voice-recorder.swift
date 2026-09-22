// Packaged macOS microphone recorder. No network, subprocesses, or runtime compilation.
// AVAudioEngine delivers every input buffer directly to one persistent converter.
import AVFoundation
import Foundation

private let outputRate = 16_000.0
private let readyMessage = "nanocodex-recorder ready\n"

private func diagnostic(_ text: String) {
    FileHandle.standardError.write(Data(text.utf8))
}

private func wavHeader(frames: UInt32) -> Data {
    var data = Data()
    func text(_ value: String) { data.append(contentsOf: value.utf8) }
    func number<T: FixedWidthInteger>(_ value: T) {
        var little = value.littleEndian
        withUnsafeBytes(of: &little) { data.append(contentsOf: $0) }
    }
    text("RIFF"); number(UInt32(36) + frames * 2); text("WAVEfmt ")
    number(UInt32(16)); number(UInt16(1)); number(UInt16(1))
    number(UInt32(outputRate)); number(UInt32(outputRate) * 2)
    number(UInt16(2)); number(UInt16(16)); text("data"); number(frames * 2)
    return data
}

private final class Recorder {
    private let engine = AVAudioEngine()
    private let lock = NSLock()
    private let file: FileHandle
    private let maxFrames: AVAudioFrameCount
    private var converter: AVAudioConverter?
    private var outputFormat: AVAudioFormat!
    private var frames: AVAudioFrameCount = 0
    private var inputFrames: Int64 = 0
    private var firstSample: AVAudioFramePosition?
    private var nextSample: AVAudioFramePosition?
    private var discontinuities = 0
    private var inputRate = 0.0
    private var stopping = false
    private var requestedStop = false
    private var failure: String?
    private var configurationObserver: NSObjectProtocol?

    init(path: String, seconds: Int) throws {
        maxFrames = AVAudioFrameCount(seconds * Int(outputRate))
        file = try FileHandle(forWritingTo: URL(fileURLWithPath: path))
        try file.truncate(atOffset: 0)
        try file.write(contentsOf: wavHeader(frames: 0))
    }

    private func configure(_ format: AVAudioFormat) throws {
        guard format.sampleRate > 0, format.channelCount > 0,
              let output = AVAudioFormat(commonFormat: .pcmFormatFloat32,
                                         sampleRate: outputRate, channels: 1, interleaved: false),
              let conversion = AVAudioConverter(from: format, to: output) else {
            throw NSError(domain: "Recorder", code: 1,
                          userInfo: [NSLocalizedDescriptionKey: "Microphone has no usable audio format"])
        }
        inputRate = format.sampleRate
        outputFormat = output
        converter = conversion
    }

    func start() throws {
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        try configure(format)
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { [weak self] buffer, time in
            self?.receive(buffer, time: time)
        }
        engine.prepare()
        try engine.start()
        configurationObserver = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: engine, queue: .main
        ) { [weak self] _ in
            self?.finish(error: "Microphone configuration changed; please record again")
        }
        diagnostic(readyMessage)
    }

    private func write(_ buffer: AVAudioPCMBuffer) throws {
        let count = min(buffer.frameLength, maxFrames - frames)
        guard count > 0, let samples = buffer.floatChannelData?[0] else { return }
        var pcm = [Int16](repeating: 0, count: Int(count))
        for index in pcm.indices {
            let sample = samples[index].isFinite ? samples[index] : 0
            pcm[index] = Int16(max(-32768, min(32767, (sample * 32768).rounded()))).littleEndian
        }
        try pcm.withUnsafeBytes { try file.write(contentsOf: Data($0)) }
        frames += count
    }

    private func receive(_ buffer: AVAudioPCMBuffer, time: AVAudioTime) {
        lock.lock()
        defer { lock.unlock() }
        guard !stopping, !requestedStop else { return }
        do {
            if time.isSampleTimeValid {
                if let expected = nextSample, expected != time.sampleTime { discontinuities += 1 }
                if firstSample == nil { firstSample = time.sampleTime }
                nextSample = time.sampleTime + Int64(buffer.frameLength)
            }
            inputFrames += Int64(buffer.frameLength)
            let capacity = AVAudioFrameCount(ceil(Double(buffer.frameLength) * outputRate / inputRate)) + 256
            let output = AVAudioPCMBuffer(pcmFormat: outputFormat, frameCapacity: capacity)!
            var supplied = false
            var error: NSError?
            let status = converter!.convert(to: output, error: &error) { _, inputStatus in
                if supplied { inputStatus.pointee = .noDataNow; return nil }
                supplied = true
                inputStatus.pointee = .haveData
                return buffer
            }
            if let error { throw error }
            guard supplied, status != .error else {
                throw NSError(domain: "Recorder", code: 2,
                              userInfo: [NSLocalizedDescriptionKey: "Converter did not consume microphone input"])
            }
            try write(output)
            if frames == maxFrames {
                requestedStop = true
                DispatchQueue.main.async { self.finish() }
            }
        } catch {
            failure = error.localizedDescription
            requestedStop = true
            DispatchQueue.main.async { self.finish() }
        }
    }

    // Called on the main queue, after stdin q/EOF, the frame limit, or a device error.
    func finish(error: String? = nil) {
        if stopping { return }
        engine.stop()
        engine.inputNode.removeTap(onBus: 0)
        if let observer = configurationObserver { NotificationCenter.default.removeObserver(observer) }
        lock.lock()
        stopping = true
        failure = error ?? failure
        do {
            // Drain converter latency so the WAV includes the final input samples.
            if failure == nil, let converter {
                var status: AVAudioConverterOutputStatus = .haveData
                while status == .haveData {
                    let output = AVAudioPCMBuffer(pcmFormat: outputFormat, frameCapacity: 4096)!
                    var conversionError: NSError?
                    status = converter.convert(to: output, error: &conversionError) { _, inputStatus in
                        inputStatus.pointee = .endOfStream
                        return nil
                    }
                    if let conversionError { throw conversionError }
                    if status == .error { throw NSError(domain: "Recorder conversion", code: 2) }
                    try write(output)
                }
            }
            try file.seek(toOffset: 0)
            try file.write(contentsOf: wavHeader(frames: frames))
            try file.synchronize()
            try file.close()
        } catch { failure = error.localizedDescription }
        let span = Double((nextSample ?? 0) - (firstSample ?? 0)) / max(1, inputRate)
        diagnostic("nanocodex-recorder frames=\(frames) input_frames=\(inputFrames) input_rate=\(inputRate) input_span=\(span) discontinuities=\(discontinuities)\n")
        let expectedFrames = min(Int64(maxFrames), Int64((Double(inputFrames) * outputRate / max(1, inputRate)).rounded()))
        if abs(Int64(frames) - expectedFrames) > 1 && failure == nil {
            failure = "Microphone conversion lost audio samples; please record again"
        }
        if discontinuities > 0 && failure == nil {
            failure = "Microphone dropped audio buffers; please record again"
        }
        if frames == 0 && failure == nil { failure = "Microphone produced no audio" }
        let result = failure
        lock.unlock()
        if let result { diagnostic("\(result)\n"); exit(1) }
        exit(0)
    }
}

let args = CommandLine.arguments
// Only the parent-created private temporary WAV is opened. Limits cannot be raised.
guard args.count == 4, args[1] == "--max-seconds", let seconds = Int(args[2]),
      (1...120).contains(seconds) else {
    diagnostic("Usage: nanocodex-voice-recorder --max-seconds 1..120 existing.wav\n")
    exit(2)
}
private var activeRecorder: Recorder?

private func beginRecording() {
    do {
        let recorder = try Recorder(path: args[3], seconds: seconds)
        try recorder.start()
        activeRecorder = recorder
        DispatchQueue.main.asyncAfter(deadline: .now() + .seconds(seconds)) { recorder.finish() }
    } catch {
        diagnostic("Cannot record microphone: \(error.localizedDescription)\n")
        exit(1)
    }
}

// Observe parent shutdown even while first-use permission is pending. All
// recorder state and permission callbacks run on the main queue; EOF before
// permission exits without ever opening the microphone.
DispatchQueue.global().async {
    while let line = readLine() {
        if line.trimmingCharacters(in: .whitespacesAndNewlines) == "q" { break }
    }
    DispatchQueue.main.async {
        if let recorder = activeRecorder { recorder.finish() }
        else { exit(0) }
    }
}

switch AVCaptureDevice.authorizationStatus(for: .audio) {
case .authorized:
    beginRecording()
case .notDetermined:
    diagnostic("nanocodex-recorder permission\n")
    AVCaptureDevice.requestAccess(for: .audio) { granted in
        DispatchQueue.main.async {
            if granted { beginRecording() }
            else { diagnostic("Microphone permission was denied\n"); exit(1) }
        }
    }
case .denied, .restricted:
    diagnostic("Microphone permission is unavailable. Allow microphone access in System Settings.\n")
    exit(1)
@unknown default:
    diagnostic("Microphone permission status is unavailable\n")
    exit(1)
}
dispatchMain()
