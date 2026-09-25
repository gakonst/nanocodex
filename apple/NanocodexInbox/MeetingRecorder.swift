import AVFoundation
import Foundation
import InboxCore
import OSLog
import Speech

/// The audio callback never touches SwiftUI or actor-isolated state. Swapping the
/// request under the same lock as append gives adjacent segments a single boundary:
/// every buffer belongs to exactly one recognition request.
private final class MeetingAudioRouter: @unchecked Sendable {
    private let lock = NSLock()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var recentLevels: [UInt8] = []
    private var lastLevelAt: TimeInterval = 0

    func append(_ buffer: AVAudioPCMBuffer) {
        // Quantized amplitude only; no audio content is retained for Lock Screen UI.
        let now = ProcessInfo.processInfo.systemUptime
        var nextLevel: UInt8?
        if let samples = buffer.floatChannelData?.pointee {
            let frames = Int(buffer.frameLength)
            var peak: Float = 0
            for index in stride(from: 0, to: frames, by: max(1, frames / 64)) {
                peak = max(peak, abs(samples[index]))
            }
            nextLevel = UInt8(min(15, max(1, Int(peak * 55))))
        }
        lock.lock()
        request?.append(buffer)
        if let nextLevel, now - lastLevelAt >= 0.12 {
            recentLevels.append(nextLevel)
            if recentLevels.count > 28 { recentLevels.removeFirst() }
            lastLevelAt = now
        }
        lock.unlock()
    }

    func levels() -> [UInt8] { lock.withLock { recentLevels } }
    func resetLevels() { lock.withLock { recentLevels.removeAll(); lastLevelAt = 0 } }

    func replace(with next: SFSpeechAudioBufferRecognitionRequest?) -> SFSpeechAudioBufferRecognitionRequest? {
        lock.lock()
        let previous = request
        request = next
        lock.unlock()
        return previous
    }
}

/// Explicitly started foreground meeting capture. Audio keeps flowing through one
/// AVAudioEngine tap while bounded Speech requests rotate (including after a
/// recognizer's early final result). Recognition never implicitly sends a task.
@MainActor
final class MeetingRecorder: ObservableObject {
    static let shared = MeetingRecorder()
    @Published private(set) var transcript = ""
    @Published private(set) var status = "Ready to listen"
    @Published private(set) var recording = false
    @Published private(set) var working = false
    @Published private(set) var reviewing = false
    @Published private(set) var seconds = 0
    @Published private(set) var waveform: [UInt8] = []

    private final class Segment {
        let index: Int
        let request: SFSpeechAudioBufferRecognitionRequest
        var task: SFSpeechRecognitionTask?
        var sealed = false
        var settled = false
        init(index: Int, request: SFSpeechAudioBufferRecognitionRequest) { self.index = index; self.request = request }
    }

    private let engine = AVAudioEngine()
    private let router = MeetingAudioRouter()
    private let log = Logger(subsystem: "xyz.paradigm.centaur", category: "Meeting")
    private var recognizer: SFSpeechRecognizer?
    private var segments: [Segment] = []
    private var ledger = MeetingSegmentPolicy()
    private var rotation: Task<Void, Never>?
    private var clock: Task<Void, Never>?
    private var completion: Task<Void, Never>?
    private var permissionRun = UUID()
    private var sessionActive = false
    private var tapped = false
    private var startedAt: Date?
    private var stopReason: String?
    /// Recognition may finish with a partial transcript after an interruption or
    /// timeout. Never auto-submit that text on the ordinary Stop path.
    var completedWithWarning: Bool { stopReason != nil }
    // Apple's Speech API documents a ~one-minute audio limit per recognition.
    // 45 seconds leaves headroom for scheduling and processing delays.
    static let segmentSeconds = MeetingSegmentPolicy.segmentSeconds

    func start(locale: String, permissionsGranted: Bool = false) async {
        discard()
        router.resetLevels()
        guard QuickVoiceRecorder.audioOwner == nil else {
            status = "Another voice recording is in progress. Finish it first."
            return
        }
        QuickVoiceRecorder.audioOwner = self
        let run = permissionRun
        working = true
        status = "Requesting Microphone and Speech Recognition access…"
        let allowed: Bool
        if permissionsGranted { allowed = QuickVoiceRecorder.permissionsGranted }
        else {
            let speech = await withCheckedContinuation { continuation in
                SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0) }
            }
            guard permissionRun == run else { return }
            let microphone = await AVAudioApplication.requestRecordPermission()
            guard permissionRun == run else { return }
            allowed = speech == .authorized && microphone
        }
        guard allowed else {
            stopWithWarning("Allow Microphone and Speech Recognition in Settings, then try again.")
            return
        }
        guard let recognizer = SFSpeechRecognizer(locale: Locale(identifier: locale)), recognizer.isAvailable else {
            stopWithWarning("Speech Recognition is unavailable for this language. Try again when connected.")
            return
        }
        self.recognizer = recognizer
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.playAndRecord, mode: .measurement, options: [.mixWithOthers])
            VoiceDiagnostic.note("meeting.recorder.sessionActivation")
            try session.setActive(true)
            VoiceDiagnostic.note("meeting.recorder.sessionActivated")
            sessionActive = true
            let input = engine.inputNode
            let format = input.outputFormat(forBus: 0)
            guard format.sampleRate > 0, format.channelCount > 0 else {
                stopWithWarning("No microphone is available.")
                return
            }
            // Prepare the first recognizer before delivering any microphone buffers.
            let first = newSegment(run: run)
            _ = router.replace(with: first.request)
            input.installTap(onBus: 0, bufferSize: 1024, format: format) { [router] buffer, _ in
                router.append(buffer)
            }
            tapped = true
            engine.prepare()
            try engine.start()
            VoiceDiagnostic.note("meeting.recorder.engineStarted")
            recording = true
            startedAt = Date()
            status = "Recording. Tap Stop Recording to start an agent."
            scheduleRotation(run: run)
            clock = Task { [weak self] in
                while !Task.isCancelled {
                    do { try await Task.sleep(for: .seconds(1)) } catch { return }
                    guard let self, self.permissionRun == run, self.recording else { return }
                    self.seconds = Int(Date().timeIntervalSince(self.startedAt ?? Date()))
                self.waveform = self.router.levels()
                }
            }
        } catch {
            let failure = error as NSError
            VoiceDiagnostic.note("meeting.recorder.audioStartFailed", error: error)
            log.error("Meeting audio start failed: domain=\(failure.domain, privacy: .public) code=\(failure.code)")
            stopWithWarning("Microphone could not start. Try again.")
        }
    }

    private func newSegment(run: UUID) -> Segment {
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        request.taskHint = .dictation
        let segment = Segment(index: ledger.begin(), request: request)
        segments.append(segment)
        segment.task = recognizer?.recognitionTask(with: request) { [weak self, weak segment] result, error in
            let text = result?.bestTranscription.formattedString
            let final = result?.isFinal == true
            Task { @MainActor in
                guard let self, let segment, self.permissionRun == run, !segment.settled else { return }
                if let text { self.ledger.update(segment.index, text: text); self.updateTranscript() }
                if let error {
                    let failure = error as NSError
                    self.log.error("Meeting speech failed: domain=\(failure.domain, privacy: .public) code=\(failure.code)")
                    self.settle(segment)
                    if self.recording { self.stopWithWarning("Speech Recognition stopped. Review the partial transcript; some words may be missing.") }
                    else { self.checkCompletion() }
                } else if final {
                    let isCurrent = self.segments.last === segment
                    self.settle(segment)
                    if self.recording, isCurrent {
                        // A pause can finalize a task before the timer. It must not
                        // finish the meeting or submit anything.
                        self.rotate(run: run)
                    } else { self.checkCompletion() }
                }
            }
        }
        return segment
    }

    private func scheduleRotation(run: UUID) {
        rotation?.cancel()
        rotation = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(Self.segmentSeconds)) } catch { return }
            guard let self, self.permissionRun == run, self.recording else { return }
            self.rotate(run: run)
        }
    }

    private func rotate(run: UUID) {
        guard recording, permissionRun == run, let previous = segments.last else { return }
        // If Speech stalls, do not silently build an unbounded queue of unsent
        // audio. Stop visibly and let the user review the partial text.
        guard ledger.canRotate(sealedPending: segments.filter({ $0.sealed && !$0.settled }).count) else {
            stopWithWarning("Transcription is falling behind. Review the partial transcript; some words may be missing.")
            return
        }
        let next = newSegment(run: run)
        let oldRequest = router.replace(with: next.request)
        previous.sealed = true
        oldRequest?.endAudio()
        if previous.settled { release(previous) }
        seconds = Int(Date().timeIntervalSince(startedAt ?? Date()))
        scheduleRotation(run: run)
    }

    func finish() {
        guard recording else { return }
        stopCapture()
        stopReason = nil
        status = "Finishing transcription…"
        awaitCompletion()
    }

    func interrupt(_ reason: String = "Recording interrupted. Review the partial transcript before sending.") {
        guard working else { return }
        stopWithWarning(reason)
    }

    private func stopWithWarning(_ reason: String) {
        stopReason = reason
        stopCapture()
        status = reason
        awaitCompletion()
    }

    private func stopCapture() {
        rotation?.cancel(); rotation = nil
        clock?.cancel(); clock = nil
        if recording { seconds = Int(Date().timeIntervalSince(startedAt ?? Date())) }
        recording = false
        let last = router.replace(with: nil)
        engine.stop()
        if tapped { engine.inputNode.removeTap(onBus: 0); tapped = false }
        if let last, let current = segments.last { current.sealed = true; last.endAudio(); if current.settled { release(current) } }
        if sessionActive {
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            sessionActive = false
        }
        if QuickVoiceRecorder.audioOwner === self { QuickVoiceRecorder.audioOwner = nil }
    }

    private func awaitCompletion() {
        checkCompletion()
        guard working else { return }
        let run = permissionRun
        completion?.cancel()
        completion = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(10)) } catch { return }
            guard let self, self.permissionRun == run else { return }
            self.stopReason = "Transcription timed out. Review the partial transcript; some words may be missing."
            for segment in Array(self.segments) where !segment.settled {
                segment.task?.cancel()
                self.settle(segment)
            }
            self.checkCompletion()
        }
    }

    private func checkCompletion() {
        guard !recording, ledger.unfinished == 0 else { return }
        completion?.cancel(); completion = nil
        working = false
        reviewing = true
        updateTranscript()
        status = stopReason ?? (transcript.isEmpty ? "No words recognized. Edit the transcript or record again." : "Review and edit the transcript, then send it in a new conversation.")
    }

    private func settle(_ segment: Segment) {
        guard !segment.settled else { return }
        segment.settled = true
        ledger.settle(segment.index)
        if segment.sealed { release(segment) }
    }

    private func release(_ segment: Segment) {
        segment.task = nil
        segments.removeAll { $0 === segment }
    }

    private func updateTranscript() {
        // Results can arrive out of order after rotation; assemble by capture order.
        // Once in review, edits belong to the user, not late Speech callbacks.
        guard working else { return }
        transcript = ledger.transcript
    }

    func edit(_ text: String) { if reviewing && !working { transcript = text } }

    func discard() {
        permissionRun = UUID() // Invalidate permission continuations and callbacks.
        completion?.cancel(); completion = nil
        stopCapture()
        for segment in segments { segment.task?.cancel() }
        segments.removeAll()
        ledger = MeetingSegmentPolicy()
        recognizer = nil
        transcript = ""
        status = "Ready to listen"
        seconds = 0
        waveform = []
        startedAt = nil
        stopReason = nil
        reviewing = false
        working = false
    }
}
