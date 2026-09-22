import Foundation

/// Destination validation deliberately returns only fixed, credential-free errors.
struct RemoteBroadcastConfiguration {
    let destination: String
    let preset: String
    init(destination: String, preset: String?) throws {
        guard destination.utf8.count <= 4096, !destination.unicodeScalars.contains(where: { CharacterSet.whitespacesAndNewlines.contains($0) || CharacterSet.controlCharacters.contains($0) }),
              let url = URLComponents(string: destination), ["rtmp", "rtmps"].contains(url.scheme ?? ""),
              let host = url.host, !host.isEmpty, url.fragment == nil, url.user == nil, url.password == nil,
              !url.path.isEmpty, url.path != "/", url.port.map({ (1...65535).contains($0) }) ?? true,
              ["source", "1080p", "720p", "twitch", "x"].contains(preset ?? "source") else { throw RemoteError.invalidMessage }
        self.destination = destination; self.preset = preset ?? "source"
    }
    func dimensions(width: Int, height: Int) -> (Int, Int) {
        let bounds = preset == "720p" ? (1280.0, 720.0) : preset != "source" ? (1920.0, 1080.0) : (3840.0, 2160.0)
        let scale = min(1, bounds.0 / Double(max(1, width)), bounds.1 / Double(max(1, height)))
        return (max(2, Int(Double(width) * scale) / 2 * 2), max(2, Int(Double(height) * scale) / 2 * 2))
    }
}

#if os(macOS)
import ScreenCaptureKit
import AppKit
import Darwin
import CoreImage
import AVFoundation

/// Independent ScreenCaptureKit publisher. One retained frame and a nonblocking
/// pipe bound memory even if the network stops consuming. FFmpeg diagnostics
/// are discarded because they can contain the secret destination URL.
final class MacBroadcast: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    static var executable: URL? {
        [Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/ffmpeg").path,
         "/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"]
            .first { FileManager.default.isExecutableFile(atPath: $0) }.map { URL(fileURLWithPath: $0) }
    }
    private let queue = DispatchQueue(label: "nanocodex.broadcast", qos: .userInitiated)
    private var stream: SCStream?
    private var process: Process?
    private var retiring: [Process] = []
    private let frameSlots = DispatchSemaphore(value: 1)
    private let audioSlots = DispatchSemaphore(value: 8)
    private var pipe: Pipe?
    private var timer: DispatchSourceTimer?
    private var writer: DispatchSourceWrite?
    private var writerSuspended = true
    private var latest: CVPixelBuffer?
    private var audioInput: BroadcastAudioPipe?
    private var configuration: RemoteBroadcastConfiguration?
    private let imageContext = CIContext()
    private var phone = false
    private var pending = Data()
    private var offset = 0
    private var stalledAt: TimeInterval?
    private var width = 0, height = 0
    private var stopped = false
    private var encoderEpoch = UUID()
    private var retries = 0
    private var progress = Data()
    private var progressPipe: Pipe?
    var onStatus: @Sendable (String) -> Void = { _ in }

    @MainActor func start(surfaceID: String, configuration: RemoteBroadcastConfiguration) async throws {
        guard let executable = Self.executable, CGPreflightScreenCaptureAccess() else { throw RemoteError.unavailable }
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        guard !Task.isCancelled, !queue.sync(execute: { stopped }) else { throw RemoteError.unavailable }
        guard let display = content.displays.first(where: { "display-\($0.displayID)" == surfaceID }) else { throw RemoteError.unavailable }
        let (w, h) = configuration.dimensions(width: CGDisplayPixelsWide(display.displayID), height: CGDisplayPixelsHigh(display.displayID))
        width = w; height = h; self.configuration = configuration
        let config = SCStreamConfiguration()
        config.width = w; config.height = h; config.pixelFormat = kCVPixelFormatType_32BGRA
        config.minimumFrameInterval = CMTime(value: 1, timescale: 60); config.queueDepth = 3
        config.showsCursor = true; config.capturesAudio = true; config.sampleRate = 48000; config.channelCount = 2
        let capture = SCStream(filter: SCContentFilter(display: display, excludingWindows: []), configuration: config, delegate: self)
        try capture.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        try capture.addStreamOutput(self, type: .audio, sampleHandlerQueue: queue)
        stream = capture
        try launch(executable: executable, configuration: configuration, audio: true)
        onStatus("starting")
        do { try await capture.startCapture() } catch { await stop(); throw RemoteError.unavailable }
        startTimer()
    }

    private func launch(executable: URL, configuration: RemoteBroadcastConfiguration, audio: Bool) throws {
        if audio { audioInput = try BroadcastAudioPipe(queue: queue); audioInput?.onFailure = { [weak self] in self?.fail() } }
        let input = Pipe(), output = Pipe(), child = Process()
        child.executableURL = executable
        child.arguments = Self.arguments(width: width, height: height, destination: configuration.destination, preset: configuration.preset, audioPath: audioInput?.path)
        child.standardInput = input; child.standardOutput = output; child.standardError = FileHandle.nullDevice
        let attempt = UUID(); encoderEpoch = attempt
        child.terminationHandler = { [weak self] _ in self?.queue.async { [weak self] in
            guard let self, encoderEpoch == attempt else { return }; fail()
        } }
        try child.run()
        // EPIPE must become a normal failed broadcast, never terminate the host.
        let fd = input.fileHandleForWriting.fileDescriptor
        _ = fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK)
        _ = fcntl(fd, F_SETNOSIGPIPE, 1)
        let writer = DispatchSource.makeWriteSource(fileDescriptor: fd, queue: queue)
        writer.setEventHandler { [weak self] in self?.drain() }
        // Dispatch sources must finish cancellation before their fd is closed.
        writer.setCancelHandler { try? input.fileHandleForWriting.close() }
        self.writer = writer; writerSuspended = true
        process = child; pipe = input; progressPipe = output
        output.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            guard !data.isEmpty else { handle.readabilityHandler = nil; return }
            self?.queue.async { [weak self] in
                guard let self, !stopped, encoderEpoch == attempt else { return }
                progress.append(data)
                if progress.count > 8192 { progress.removeAll() }
                // A muxed output timestamp proves output, unlike process launch.
                if let text = String(data: progress, encoding: .utf8), text.split(separator: "\n").contains(where: { line in
                    line.hasPrefix("out_time_us=") && (Int64(line.dropFirst(12)) ?? 0) > 0
                }) { progress.removeAll(); onStatus("live") }
            }
        }
    }
    private func startTimer() {
        queue.sync {
            guard !stopped else { return }
            let timer = DispatchSource.makeTimerSource(queue: queue)
            timer.schedule(deadline: .now(), repeating: .nanoseconds(1_000_000_000 / 60))
            timer.setEventHandler { [weak self] in self?.tick() }; self.timer = timer; timer.resume()
        }
    }

    @MainActor func startPhone(width: Int, height: Int, configuration: RemoteBroadcastConfiguration) throws {
        try startFrames(width: width, height: height, audio: false, configuration: configuration)
    }
    @MainActor func startFrames(width: Int, height: Int, audio: Bool, configuration: RemoteBroadcastConfiguration) throws {
        guard let executable = Self.executable else { throw RemoteError.unavailable }
        (self.width, self.height) = configuration.dimensions(width: width, height: height)
        self.configuration = configuration; phone = !audio
        try launch(executable: executable, configuration: configuration, audio: audio)
        onStatus("starting"); startTimer()
    }
    @discardableResult func appendAudioSample(_ sample: CMSampleBuffer) -> Bool {
        guard audioSlots.wait(timeout: .now()) == .success else { return false }
        queue.async { [self] in
            defer { audioSlots.signal() }
            guard !stopped else { return }; appendAudio(sample)
        }
        return true
    }
    func appendPhoneFrame(_ frame: CVPixelBuffer) {
        guard frameSlots.wait(timeout: .now()) == .success else { return }
        queue.async { [self] in
            defer { frameSlots.signal() }
            guard !stopped else { return }; latest = frame
        }
    }
    static func arguments(width: Int, height: Int, destination: String, preset: String = "source", audioPath: String? = nil) -> [String] {
        var args = ["-nostdin", "-hide_banner", "-loglevel", "quiet", "-thread_queue_size", "4", "-use_wallclock_as_timestamps", "1", "-probesize", "32", "-analyzeduration", "0", "-f", "rawvideo", "-pixel_format", "bgra",
         "-video_size", "\(width)x\(height)", "-framerate", "60", "-i", "pipe:0"]
        // PCM timestamps come from its sample clock, never FIFO arrival wall time.
        if let audioPath { args += ["-thread_queue_size", "8", "-probesize", "32", "-analyzeduration", "0", "-f", "f32le", "-ar", "48000", "-ac", "2", "-i", audioPath] }
        let bitrate = preset == "twitch" ? 6000000 : preset == "x" ? 9000000 : preset == "720p" ? 4500000 : preset == "source" ? 24000000 : 8000000
        args += ["-map", "0:v:0", "-c:v", "h264_videotoolbox", "-realtime", "1", "-allow_sw", "0", "-b:v", "\(bitrate)", "-maxrate", "\(bitrate)", "-bufsize", "\(bitrate * 2)", "-pix_fmt", "yuv420p", "-g", preset == "x" ? "90" : "120", "-bf", "0", "-fps_mode", "cfr", "-r", preset == "x" ? "30" : "60"]
        if audioPath != nil { args += ["-map", "1:a:0", "-c:a", "aac", "-b:a", preset == "x" ? "128k" : "192k", "-af", "aresample=async=1:first_pts=0"] }
        else { args += ["-an"] }
        if destination.hasPrefix("rtmps:") { args += ["-tls_verify", "1", "-ca_file", "/etc/ssl/cert.pem"] }
        return args + ["-progress", "pipe:1", "-stats_period", "1", "-rw_timeout", "10000000", "-f", "flv", "-flvflags", "no_duration_filesize", destination]
    }

    private func tick() {
        guard !stopped, pipe != nil else { return }
        if let stalledAt, ProcessInfo.processInfo.systemUptime - stalledAt > 10 { fail(); return }
        if pending.isEmpty {
            guard var buffer = latest else { return }
            if CVPixelBufferGetWidth(buffer) != width || CVPixelBufferGetHeight(buffer) != height {
                var scaled: CVPixelBuffer?
                guard CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_32BGRA, [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &scaled) == kCVReturnSuccess, let scaled else { return }
                let image = CIImage(cvPixelBuffer: buffer)
                imageContext.render(image.transformed(by: CGAffineTransform(scaleX: Double(width) / image.extent.width, y: Double(height) / image.extent.height)), to: scaled)
                buffer = scaled
            }
            CVPixelBufferLockBaseAddress(buffer, .readOnly)
            defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
            guard let base = CVPixelBufferGetBaseAddress(buffer) else { return }
            let stride = CVPixelBufferGetBytesPerRow(buffer)
            pending.reserveCapacity(width * height * 4)
            for row in 0..<height { pending.append(base.advanced(by: row * stride).assumingMemoryBound(to: UInt8.self), count: width * 4) }
            offset = 0
        }
        if writerSuspended { writerSuspended = false; writer?.resume() }
    }
    private func drain() {
        guard !stopped, let pipe else { return }
        var budget = 16 * 1024 * 1024
        while offset < pending.count && budget > 0 {
            let count = pending.withUnsafeBytes { bytes in Darwin.write(pipe.fileHandleForWriting.fileDescriptor, bytes.baseAddress!.advanced(by: offset), bytes.count - offset) }
            if count > 0 { offset += count; budget -= count }
            else if count < 0 && errno == EINTR { continue }
            else if count < 0 && errno == EAGAIN { break }
            else { fail(); return }
        }
        if offset == pending.count {
            pending.removeAll(keepingCapacity: true); offset = 0; stalledAt = nil
            if !writerSuspended { writer?.suspend(); writerSuspended = true }
        } else {
            let now = ProcessInfo.processInfo.systemUptime
            if stalledAt == nil { stalledAt = now }
            if now - (stalledAt ?? now) > 10 { fail() }
        }
    }
    private func fail() {
        guard !stopped else { return }
        cleanupEncoder(); retries += 1
        guard retries <= 8 else { shutdown(); onStatus("failed"); return }
        onStatus("reconnecting")
        let attempt = encoderEpoch
        queue.asyncAfter(deadline: .now() + Double(min(15, 1 << min(retries - 1, 4)))) { [weak self] in
            guard let self, !stopped, encoderEpoch == attempt, let configuration, let executable = Self.executable else { return }
            do { try launch(executable: executable, configuration: configuration, audio: !phone) }
            catch { fail() }
        }
    }
    private func cleanupEncoder(graceful: Bool = false) {
        encoderEpoch = UUID()
        if writerSuspended { writer?.resume(); writerSuspended = false }
        writer?.cancel(); writer = nil; pipe = nil
        pending.removeAll(keepingCapacity: true); offset = 0; stalledAt = nil
        audioInput?.close(); audioInput = nil
        progressPipe?.fileHandleForReading.readabilityHandler = nil; progress.removeAll()
        retiring.removeAll { !$0.isRunning }
        if let process, process.isRunning { retiring.append(process); let child = process
            if !graceful { child.terminate() }
            queue.asyncAfter(deadline: .now() + 2) {
                if child.isRunning { child.terminate() }
                self.queue.asyncAfter(deadline: .now() + 2) { if child.isRunning { kill(child.processIdentifier, SIGKILL) } }
            }
        }
        process = nil; progressPipe = nil
    }
    private func shutdown(graceful: Bool = false) {
        stopped = true; timer?.cancel(); timer = nil; latest = nil; configuration = nil
        cleanupEncoder(graceful: graceful)
    }
    @MainActor func stop() async {
        let capture = stream; stream = nil; try? await capture?.stopCapture()
        queue.sync { timer?.cancel(); timer = nil; latest = nil }
        // Finish the current raw frame and queued PCM before closing inputs.
        // A bounded wait keeps stop responsive during an ingest outage.
        for _ in 0..<100 {
            if queue.sync(execute: { pending.isEmpty && (audioInput?.isDrained ?? true) }) { break }
            try? await Task.sleep(for: .milliseconds(10))
        }
        let children = queue.sync { shutdown(graceful: true); return retiring }
        await Task.detached { for child in children { child.waitUntilExit() } }.value
    }
    func stream(_ stream: SCStream, didStopWithError error: Error) { queue.async { [weak self] in guard let self, !stopped else { return }; shutdown(); onStatus("failed") } }
    func stream(_ stream: SCStream, didOutputSampleBuffer sample: CMSampleBuffer, of type: SCStreamOutputType) {
        guard !stopped, sample.isValid else { return }
        if type == .audio { appendAudio(sample); return }
        guard type == .screen,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              attachments.first?[.status] as? Int == SCFrameStatus.complete.rawValue,
              let buffer = sample.imageBuffer, CVPixelBufferGetWidth(buffer) == width, CVPixelBufferGetHeight(buffer) == height else { return }
        latest = buffer
    }
    private func appendAudio(_ sample: CMSampleBuffer) {
        guard let audioInput, let description = sample.formatDescription,
              let format = CMAudioFormatDescriptionGetStreamBasicDescription(description)?.pointee,
              format.mFormatID == kAudioFormatLinearPCM, format.mBitsPerChannel == 32,
              format.mFormatFlags & kAudioFormatFlagIsFloat != 0, format.mChannelsPerFrame == 2 else { return }
        var size = 0
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sample, bufferListSizeNeededOut: &size, bufferListOut: nil, bufferListSize: 0, blockBufferAllocator: nil, blockBufferMemoryAllocator: nil, flags: 0, blockBufferOut: nil)
        guard size > 0 else { return }
        let storage = UnsafeMutableRawPointer.allocate(byteCount: size, alignment: MemoryLayout<AudioBufferList>.alignment)
        defer { storage.deallocate() }
        let list = storage.bindMemory(to: AudioBufferList.self, capacity: 1)
        var block: CMBlockBuffer?
        guard CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sample, bufferListSizeNeededOut: nil, bufferListOut: list, bufferListSize: size, blockBufferAllocator: nil, blockBufferMemoryAllocator: nil, flags: 0, blockBufferOut: &block) == noErr else { return }
        let buffers = UnsafeMutableAudioBufferListPointer(list), count = sample.numSamples
        let byteCount = count * 2 * MemoryLayout<Float>.size
        var data = Data(count: byteCount)
        data.withUnsafeMutableBytes { output in
            let target = output.bindMemory(to: Float.self)
            if buffers.count == 2, let left = buffers[0].mData?.assumingMemoryBound(to: Float.self), let right = buffers[1].mData?.assumingMemoryBound(to: Float.self) {
                for index in 0..<count { target[index * 2] = left[index]; target[index * 2 + 1] = right[index] }
            } else if buffers.count == 1, let input = buffers[0].mData, Int(buffers[0].mDataByteSize) >= byteCount {
                output.baseAddress!.copyMemory(from: input, byteCount: output.count)
            }
        }
        audioInput.append(data)
    }

}
/// Float32 stereo FIFO, bounded to five seconds. All operations run on
/// the publisher queue; cancel closes the descriptor only after source teardown.
private final class BroadcastAudioPipe {
    let path: String
    private let fd: Int32
    private var writer: DispatchSourceWrite?
    private var suspended = true
    private var closed = false
    private var pending = Data()
    private var offset = 0
    var onFailure: () -> Void = {}
    init(queue: DispatchQueue) throws {
        path = FileManager.default.temporaryDirectory.appendingPathComponent("nanocodex-audio-" + UUID().uuidString).path
        guard mkfifo(path, 0o600) == 0 else { throw RemoteError.unavailable }
        fd = open(path, O_RDWR | O_NONBLOCK)
        guard fd >= 0 else { unlink(path); throw RemoteError.unavailable }
        _ = fcntl(fd, F_SETNOSIGPIPE, 1)
        let writer = DispatchSource.makeWriteSource(fileDescriptor: fd, queue: queue)
        self.writer = writer
        writer.setEventHandler { [weak self] in self?.drain() }
        let descriptor = fd; writer.setCancelHandler { Darwin.close(descriptor) }
    }
    var isDrained: Bool { pending.isEmpty }
    func append(_ data: Data) {
        guard !closed else { return }
        guard pending.count - offset + data.count <= 1_920_000 else { onFailure(); return }
        if offset > 0 { pending.removeFirst(offset); offset = 0 }
        pending.append(data)
        if suspended { suspended = false; writer?.resume() }
        // Small PCM writes may not cross the FIFO readiness low-water mark.
        // Attempt the nonblocking write now; the source handles backpressure.
        drain()
    }
    private func drain() {
        guard !closed else { return }
        while offset < pending.count {
            let count = pending.withUnsafeBytes { Darwin.write(fd, $0.baseAddress!.advanced(by: offset), $0.count - offset) }
            if count > 0 { offset += count }
            else if count < 0 && errno == EINTR { continue }
            else if count < 0 && errno == EAGAIN { return }
            else { onFailure(); return }
        }
        pending.removeAll(keepingCapacity: true); offset = 0
        if !suspended { writer?.suspend(); suspended = true }
    }
    func close() {
        guard !closed else { return }; closed = true
        if suspended { writer?.resume(); suspended = false }
        writer?.cancel(); writer = nil; unlink(path); pending.removeAll()
    }
    deinit { close() }
}
#endif
