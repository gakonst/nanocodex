#if os(iOS)
import AVFoundation
import SwiftUI

/// Owns only locally recorded samples. Imported security-scoped files remain owned by Files.
@MainActor final class VoiceCloneRecording: NSObject, ObservableObject, AVAudioRecorderDelegate, AVAudioPlayerDelegate {
    @Published private(set) var preparing = false
    @Published private(set) var recording = false
    @Published private(set) var saving = false
    @Published private(set) var permissionDenied = false
    @Published private(set) var playing = false
    @Published private(set) var playingURL: URL?
    @Published private(set) var sample: URL?
    @Published private(set) var elapsed: TimeInterval = 0
    @Published private(set) var duration: TimeInterval = 0
    @Published private(set) var playbackElapsed: TimeInterval = 0
    @Published private(set) var playbackDuration: TimeInterval = 0
    @Published private(set) var level: Float = 0
    @Published var error: String?
    private var meterTask: Task<Void, Never>?
    private var playbackTask: Task<Void, Never>?
    private var recorder: AVAudioRecorder?
    private var player: AVAudioPlayer?
    private var pendingURL: URL?
    private var playbackAccess: PlaybackAccess?

    /// Balances imported-file access even when this controller is released during playback.
    private final class PlaybackAccess {
        let url: URL
        let scoped: Bool
        init(url: URL) {
            self.url = url
            scoped = url.startAccessingSecurityScopedResource()
        }
        deinit {
            if scoped { url.stopAccessingSecurityScopedResource() }
        }
    }
    private var generation = UUID()
    private var ownsAudio = false

    override init() {
        super.init()
        NotificationCenter.default.addObserver(self, selector: #selector(audioInterrupted), name: AVAudioSession.interruptionNotification, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(audioRouteChanged), name: AVAudioSession.routeChangeNotification, object: nil)
    }
    deinit {
        meterTask?.cancel()
        playbackTask?.cancel()
        NotificationCenter.default.removeObserver(self)
    }
    @objc nonisolated private func audioRouteChanged(_ notification: Notification) {
        guard let reason = notification.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
              reason == AVAudioSession.RouteChangeReason.oldDeviceUnavailable.rawValue else { return }
        Task { @MainActor [weak self] in self?.suspend() }
    }
    @objc nonisolated private func audioInterrupted(_ notification: Notification) {
        guard let type = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
              type == AVAudioSession.InterruptionType.began.rawValue else { return }
        Task { @MainActor [weak self] in self?.suspend() }
    }

    func start() async {
        guard !preparing, !recording, !saving else { return }
        stopPlayback()
        generation = UUID()
        elapsed = 0
        error = nil
        let token = generation
        preparing = true
        permissionDenied = false
        let granted: Bool
        #if DEBUG && targetEnvironment(simulator)
        if ProcessInfo.processInfo.arguments.contains("--voice-clone-ui-fixture") {
            granted = true
        } else {
            granted = await AVAudioApplication.requestRecordPermission()
        }
        #else
        granted = await AVAudioApplication.requestRecordPermission()
        #endif
        guard token == generation else { return }
        preparing = false
        permissionDenied = !granted
        guard granted else { error = "Allow microphone access in Settings to record a voice sample."; return }
        do {
            let audio = AVAudioSession.sharedInstance()
            try audio.setCategory(.playAndRecord, mode: .default, options: [.defaultToSpeaker])
            try audio.setActive(true)
            ownsAudio = true
            let recorder: AVAudioRecorder
            #if DEBUG && targetEnvironment(simulator)
            if ProcessInfo.processInfo.arguments.contains("--voice-clone-ui-fixture") {
                let url = FileManager.default.temporaryDirectory.appendingPathComponent("voice-clone-\(UUID().uuidString).wav")
                pendingURL = url
                recorder = try SimulatorVoiceCloneRecorder(url: url, settings: [AVFormatIDKey: kAudioFormatLinearPCM, AVSampleRateKey: 8000, AVNumberOfChannelsKey: 1, AVLinearPCMBitDepthKey: 16])
            } else {
                let url = FileManager.default.temporaryDirectory.appendingPathComponent("voice-clone-\(UUID().uuidString).m4a")
                pendingURL = url
                recorder = try AVAudioRecorder(url: url, settings: [AVFormatIDKey: kAudioFormatMPEG4AAC, AVSampleRateKey: 44100, AVNumberOfChannelsKey: 1, AVEncoderBitRateKey: 128000])
            }
            #else
            let url = FileManager.default.temporaryDirectory.appendingPathComponent("voice-clone-\(UUID().uuidString).m4a")
            pendingURL = url
            recorder = try AVAudioRecorder(url: url, settings: [AVFormatIDKey: kAudioFormatMPEG4AAC, AVSampleRateKey: 44100, AVNumberOfChannelsKey: 1, AVEncoderBitRateKey: 128000])
            #endif
            recorder.isMeteringEnabled = true
            recorder.delegate = self
            self.recorder = recorder
            guard recorder.record(forDuration: 120) else { throw CocoaError(.fileWriteUnknown) }
            recording = true
            meterTask = Task { [weak self] in
                while !Task.isCancelled {
                    self?.updateMeter()
                    do { try await Task.sleep(for: .milliseconds(100)) } catch { return }
                }
            }
        } catch { abandonPendingRecording(); self.error = "Could not start recording. Check microphone access and try again." }
    }
    #if DEBUG
    /// Seeds capture ownership without requesting microphone permission or starting audio I/O.
    func prepareRecordingForTesting(_ recorder: AVAudioRecorder) {
        abandonPendingRecording()
        stopPlayback()
        pendingURL = recorder.url
        self.recorder = recorder
        recording = true
    }
    #endif
    private func updateMeter() {
        guard let recorder, recording else { return }
        elapsed = recorder.currentTime
        recorder.updateMeters()
        level = min(1, max(0, pow(10, recorder.averagePower(forChannel: 0) / 20)))
    }
    func stop() {
        guard recording, let recorder else { return }
        updateMeter()
        recording = false
        saving = true
        meterTask?.cancel(); meterTask = nil; level = 0
        recorder.stop()
    }
    nonisolated func audioRecorderDidFinishRecording(_ recorder: AVAudioRecorder, successfully flag: Bool) {
        Task { @MainActor in
            guard self.recorder === recorder else { return }
            self.meterTask?.cancel(); self.meterTask = nil; self.level = 0
            self.recording = false; self.saving = false; self.recorder = nil
            if flag, let replacement = self.pendingURL {
                let previous = self.sample
                self.sample = replacement; self.pendingURL = nil
                self.duration = (try? AVAudioPlayer(contentsOf: replacement).duration) ?? self.elapsed
                if let previous, previous != replacement { try? FileManager.default.removeItem(at: previous) }
            }
            else { self.abandonPendingRecording(); self.error = "Recording was interrupted. Please record again." }
            self.releaseAudio()
        }
    }
    func play() {
        guard let sample else { return }
        play(url: sample)
    }
    func play(url: URL) {
        guard !preparing, !recording, !saving else { return }
        stopPlayback()
        error = nil
        playbackElapsed = 0
        playbackAccess = PlaybackAccess(url: url)
        do {
            try AVAudioSession.sharedInstance().setCategory(.playback, mode: .default)
            try AVAudioSession.sharedInstance().setActive(true)
            ownsAudio = true
            let player = try AVAudioPlayer(contentsOf: url)
            player.delegate = self; self.player = player
            playbackDuration = player.duration
            guard player.play() else { throw CocoaError(.fileReadCorruptFile) }
            playingURL = url
            playing = true
            playbackTask = Task { [weak self] in
                while !Task.isCancelled {
                    self?.updatePlaybackProgress()
                    do { try await Task.sleep(for: .milliseconds(100)) } catch { return }
                }
            }
        } catch { stopPlayback(); self.error = "Could not play this sample. Please record again." }
    }
    private func updatePlaybackProgress() {
        guard playing, let player else { return }
        playbackElapsed = player.currentTime
    }
    func stopPlayback() {
        playbackTask?.cancel(); playbackTask = nil
        let old = player
        player = nil
        old?.stop()
        playbackAccess = nil
        playingURL = nil
        playing = false
        playbackElapsed = 0
        playbackDuration = 0
        if recorder == nil { releaseAudio() }
    }
    nonisolated func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        Task { @MainActor in guard self.player === player else { return }; self.stopPlayback() }
    }
    nonisolated func audioPlayerDecodeErrorDidOccur(_ player: AVAudioPlayer, error: Error?) {
        Task { @MainActor in
            guard self.player === player else { return }
            self.stopPlayback(); self.error = "Could not play this sample. Please record again."
        }
    }
    nonisolated func audioRecorderEncodeErrorDidOccur(_ recorder: AVAudioRecorder, error: Error?) {
        Task { @MainActor in
            guard self.recorder === recorder else { return }
            self.abandonPendingRecording(); self.error = "Recording failed. Please record again."
        }
    }
    /// Cancels a permission request without removing the sample already under review.
    func cancelPreparation() {
        guard preparing else { return }
        generation = UUID()
        preparing = false
    }
    func cancelRecording() {
        abandonPendingRecording()
        error = nil
    }
    private func abandonPendingRecording() {
        generation = UUID()
        meterTask?.cancel(); meterTask = nil
        preparing = false; recording = false; saving = false
        elapsed = 0; level = 0
        let old = recorder
        recorder = nil
        old?.stop()
        if let pendingURL { try? FileManager.default.removeItem(at: pendingURL) }
        pendingURL = nil
        releaseAudio()
    }
    func discard() {
        abandonPendingRecording()
        stopPlayback()
        if let sample { try? FileManager.default.removeItem(at: sample) }
        sample = nil; duration = 0; error = nil
    }
    func suspend() {
        cancelPreparation()
        if recording { stop() }
        if playing { stopPlayback() }
    }
    private func releaseAudio() { guard ownsAudio else { return }; ownsAudio = false; try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation) }
}
#if DEBUG && targetEnvironment(simulator)
/// Explicit UI-test fixture; never captures microphone input and is absent from device/release builds.
private final class SimulatorVoiceCloneRecorder: AVAudioRecorder {
    private var startedAt: Date?
    private var stoppedElapsed: TimeInterval = 0
    private var limit: TimeInterval = 120
    private var stopTimer: Timer?

    override var currentTime: TimeInterval {
        guard let startedAt else { return stoppedElapsed }
        return min(limit, Date().timeIntervalSince(startedAt))
    }
    override func record(forDuration duration: TimeInterval) -> Bool {
        limit = duration
        startedAt = Date()
        stopTimer = Timer.scheduledTimer(withTimeInterval: duration, repeats: false) { [weak self] _ in self?.stop() }
        return true
    }
    override func updateMeters() {}
    override func averagePower(forChannel channelNumber: Int) -> Float { -12 }
    override func stop() {
        guard startedAt != nil else { return }
        stoppedElapsed = currentTime
        startedAt = nil
        stopTimer?.invalidate(); stopTimer = nil
        do {
            // A quiet tone makes real playback observable while avoiding any recorded personal audio.
            let count = max(1, Int(stoppedElapsed * 8000))
            let byteCount = UInt32(count * 2)
            var wav = Data()
            func word<T: FixedWidthInteger>(_ value: T) {
                var little = value.littleEndian
                withUnsafeBytes(of: &little) { wav.append(contentsOf: $0) }
            }
            wav.append(contentsOf: "RIFF".utf8); word(byteCount + 36)
            wav.append(contentsOf: "WAVEfmt ".utf8); word(UInt32(16))
            word(UInt16(1)); word(UInt16(1)); word(UInt32(8000))
            word(UInt32(16000)); word(UInt16(2)); word(UInt16(16))
            wav.append(contentsOf: "data".utf8); word(byteCount)
            for index in 0..<count {
                word(Int16(sin(Double(index) * 2 * .pi * 220 / 8000) * 1200))
            }
            try wav.write(to: url)
            delegate?.audioRecorderDidFinishRecording?(self, successfully: true)
        } catch {
            delegate?.audioRecorderDidFinishRecording?(self, successfully: false)
        }
    }
    deinit { stopTimer?.invalidate() }
}
#endif
#endif
