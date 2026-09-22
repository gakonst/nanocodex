#!/usr/bin/env python3
"""Compile the production LockedAudioRecorder with observable AV/Speech test doubles."""
from pathlib import Path
import os
import re
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
view = (root / 'NanocodexInbox/QuickVoiceView.swift').read_text()
start = view.index('@MainActor\nfinal class LockedAudioRecorder:')
recorder = view[start:view.index('\nstruct QuickVoiceView:', start)]
# OSLog interpolation is unavailable in the portable harness. File protection
# operations are observed by a filesystem adapter; capture logic is unchanged.
recorder = re.sub(r'^    private let log = Logger.*\n', '', recorder, flags=re.M)
recorder = re.sub(r'^        log\.error\(.*\n', '', recorder, flags=re.M)
recorder = recorder.replace('FileManager.default', 'TestFiles.shared')
source = r'''
import Foundation
extension FileAttributeKey { static let protectionKey = FileAttributeKey("testProtection") }
enum FileProtectionType { case completeUntilFirstUserAuthentication }
final class TestFiles {
    static let shared = TestFiles()
    let temporaryDirectory = FileManager.default.temporaryDirectory
    var protected = Set<String>()
    var rejectFileProtection = false
    func createDirectory(at url: URL, withIntermediateDirectories: Bool, attributes: [FileAttributeKey: Any]) throws {
        precondition(attributes[.protectionKey] is FileProtectionType)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: withIntermediateDirectories)
        protected.insert(url.path)
    }
    func setAttributes(_ attributes: [FileAttributeKey: Any], ofItemAtPath path: String) throws {
        precondition(attributes[.protectionKey] is FileProtectionType)
        precondition(FileManager.default.fileExists(atPath: path))
        if rejectFileProtection && path.hasSuffix(".m4a") { throw CocoaError(.fileWriteNoPermission) }
        protected.insert(path)
    }
    func contentsOfDirectory(at url: URL, includingPropertiesForKeys keys: [URLResourceKey]?) throws -> [URL] {
        try FileManager.default.contentsOfDirectory(at: url, includingPropertiesForKeys: keys)
    }
    func removeItem(at url: URL) throws { try FileManager.default.removeItem(at: url); protected.remove(url.path) }
}
let AVFormatIDKey = "format", AVSampleRateKey = "rate"
let AVNumberOfChannelsKey = "channels", AVEncoderAudioQualityKey = "quality"
let kAudioFormatMPEG4AAC = 1
enum AVAudioQuality: Int { case high = 1 }
protocol AVAudioRecorderDelegate: AnyObject {}
final class AVAudioRecorder {
    static var created: [AVAudioRecorder] = []
    let url: URL
    weak var delegate: AVAudioRecorderDelegate?
    var stopped = false
    init(url: URL, settings: [String: Any]) throws {
        self.url = url
        Self.created.append(self)
    }
    static var preparationSucceeds = true
    func prepareToRecord() -> Bool {
        precondition(TestFiles.shared.protected.contains(url.deletingLastPathComponent().path), "output directory must be protected before preparation")
        TestFiles.shared.protected.remove(url.path) // Framework replaces any placeholder.
        FileManager.default.createFile(atPath: url.path, contents: Data())
        return Self.preparationSucceeds
    }
    func record() -> Bool {
        precondition(TestFiles.shared.protected.contains(url.path), "actual prepared output must be protected before recording")
        return true
    }
    func stop() { stopped = true }
}
final class AVAudioSession {
    enum Category { case record }
    enum Mode { case measurement }
    struct Options: OptionSet {
        let rawValue: Int
        static let notifyOthersOnDeactivation = Options(rawValue: 1)
    }
    static let shared = AVAudioSession()
    static func sharedInstance() -> AVAudioSession { shared }
    var active = false
    func setCategory(_ category: Category, mode: Mode) throws {}
    func setActive(_ active: Bool, options: Options = []) throws { self.active = active }
}
struct Transcription { let formattedString: String }
struct SFSpeechRecognitionResult {
    let bestTranscription: Transcription
    let isFinal: Bool
}
final class SFSpeechURLRecognitionRequest {
    let url: URL
    var shouldReportPartialResults = true
    init(url: URL) { self.url = url }
}
final class SFSpeechRecognitionTask {
    var cancelled = false
    let callback: (SFSpeechRecognitionResult?, Error?) -> Void
    init(_ callback: @escaping (SFSpeechRecognitionResult?, Error?) -> Void) { self.callback = callback }
    func cancel() { cancelled = true }
    // Deliberately deliver callbacks even after cancel, like an in-flight framework callback.
    func emit(_ text: String, final: Bool = true) {
        callback(SFSpeechRecognitionResult(bestTranscription: Transcription(formattedString: text), isFinal: final), nil)
    }
}
final class SFSpeechRecognizer {
    static var locales: [String] = []
    static var requests: [SFSpeechURLRecognitionRequest] = []
    static var tasks: [SFSpeechRecognitionTask] = []
    let isAvailable = true
    init?(locale: Locale) { Self.locales.append(locale.identifier) }
    func recognitionTask(with request: SFSpeechURLRecognitionRequest,
                         resultHandler: @escaping (SFSpeechRecognitionResult?, Error?) -> Void) -> SFSpeechRecognitionTask {
        precondition(!AVAudioSession.shared.active, "transcription began before audio stopped")
        Self.requests.append(request)
        let task = SFSpeechRecognitionTask(resultHandler)
        Self.tasks.append(task)
        return task
    }
}
enum QuickVoiceRecorder {
    enum PermissionMode { case preauthorized }
    static weak var audioOwner: AnyObject?
    static let permissionsGranted = true
}
enum QuickVoiceInput {
    static func finalText(_ text: String) -> String? {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        return text.isEmpty ? nil : text
    }
}
'''
source += recorder
source += r'''
@main struct Tests {
    @MainActor static func drain() async {
        for _ in 0..<100 { await Task.yield() }
    }
    @MainActor static func main() async {
        let recorder = LockedAudioRecorder()
        var submissions: [String] = [], errors: [String] = [], statuses: [String] = []
        var audioEnded = 0
        recorder.onFinal = { submissions.append($0) }
        recorder.onError = { errors.append($0) }
        recorder.onStatus = { statuses.append($0) }
        recorder.onAudioEnded = { audioEnded += 1 }

        let temporary = FileManager.default.temporaryDirectory
        let storage = temporary.appendingPathComponent("locked-voice", isDirectory: true)
        try! FileManager.default.createDirectory(at: storage, withIntermediateDirectories: true)
        let legacy = temporary.appendingPathComponent("locked-voice-orphan.m4a")
        let orphan = storage.appendingPathComponent("locked-voice-orphan.m4a")
        let unrelated = temporary.appendingPathComponent("other-feature.m4a")
        for url in [legacy, orphan, unrelated] { try! Data().write(to: url) }
        await recorder.start(locale: "el-GR", permissions: .preauthorized)
        precondition(!FileManager.default.fileExists(atPath: legacy.path))
        precondition(!FileManager.default.fileExists(atPath: orphan.path))
        precondition(FileManager.default.fileExists(atPath: unrelated.path))
        let cancelledAudio = AVAudioRecorder.created.last!
        precondition(recorder.recording && AVAudioSession.shared.active)
        precondition(FileManager.default.fileExists(atPath: cancelledAudio.url.path))
        precondition(SFSpeechRecognizer.locales.last == "el-GR", "Greek locale was lost")
        let contender = LockedAudioRecorder()
        var contenderErrors: [String] = []
        contender.onError = { contenderErrors.append($0) }
        await contender.start(locale: "en-US", permissions: .preauthorized)
        precondition(!contender.recording && contenderErrors == ["Another recording is in progress."])
        precondition(QuickVoiceRecorder.audioOwner === recorder && recorder.recording)
        precondition(AVAudioRecorder.created.count == 1 && AVAudioSession.shared.active)
        precondition(FileManager.default.fileExists(atPath: cancelledAudio.url.path),
                     "rejected recorder must not reap the owner's audio")
        await drain()
        precondition(SFSpeechRecognizer.tasks.isEmpty && submissions.isEmpty,
                     "start must capture audio without starting speech recognition")
        recorder.stop()
        recorder.finish()
        precondition(QuickVoiceRecorder.audioOwner == nil)
        precondition(cancelledAudio.stopped && !recorder.recording && !AVAudioSession.shared.active)
        precondition(!FileManager.default.fileExists(atPath: cancelledAudio.url.path))
        precondition(SFSpeechRecognizer.tasks.isEmpty && submissions.isEmpty && audioEnded == 1)

        await recorder.start(locale: "el-GR", permissions: .preauthorized)
        let interruptedAudio = AVAudioRecorder.created.last!
        recorder.finish()
        precondition(SFSpeechRecognizer.tasks.count == 1 && interruptedAudio.stopped)
        precondition(SFSpeechRecognizer.requests.last!.url == interruptedAudio.url)
        precondition(!SFSpeechRecognizer.requests.last!.shouldReportPartialResults)
        precondition(FileManager.default.fileExists(atPath: interruptedAudio.url.path))
        let late = SFSpeechRecognizer.tasks.last!
        recorder.stop()
        precondition(late.cancelled && !FileManager.default.fileExists(atPath: interruptedAudio.url.path))
        late.emit("stale final")
        await drain()
        precondition(submissions.isEmpty && recorder.transcript.isEmpty, "late final survived stop")

        await recorder.start(locale: "el-GR", permissions: .preauthorized)
        let committedAudio = AVAudioRecorder.created.last!
        late.emit("stale final after restart")
        await drain()
        precondition(recorder.recording && submissions.isEmpty && recorder.transcript.isEmpty)
        precondition(SFSpeechRecognizer.tasks.count == 1)
        recorder.finish()
        recorder.finish()
        precondition(SFSpeechRecognizer.tasks.count == 2, "finish should start recognition once")
        let final = SFSpeechRecognizer.tasks.last!
        final.emit("partial", final: false)
        await drain()
        precondition(submissions.isEmpty, "partial result submitted")
        final.emit("Στείλε εργασία")
        final.emit("duplicate final")
        await drain()
        precondition(submissions == ["Στείλε εργασία"] && recorder.transcript == "Στείλε εργασία")
        precondition(final.cancelled && !FileManager.default.fileExists(atPath: committedAudio.url.path))
        final.emit("another late final")
        await drain()
        precondition(submissions.count == 1 && errors.isEmpty)
        precondition(statuses == ["listening", "listening", "transcribing", "listening", "transcribing"])
        precondition(audioEnded == 3 && !recorder.recording && !AVAudioSession.shared.active)
        for rejectProtection in [false, true] {
            AVAudioRecorder.preparationSucceeds = rejectProtection
            TestFiles.shared.rejectFileProtection = rejectProtection
            await recorder.start(locale: "en-US", permissions: .preauthorized)
            let failed = AVAudioRecorder.created.last!
            precondition(!recorder.recording && !AVAudioSession.shared.active && failed.stopped)
            precondition(!FileManager.default.fileExists(atPath: failed.url.path))
            precondition(QuickVoiceRecorder.audioOwner == nil && submissions.count == 1)
        }
        precondition(errors == ["Microphone could not start.", "Microphone could not start."])
        print("PASS: protected directory and prepared output; preparation/protection failures clean up; capture without recognition; finish-only transcription; Greek locale; cancel cleanup; stale callbacks ignored; final submitted once")
    }
}
'''
with tempfile.TemporaryDirectory(prefix='locked-audio-test-') as directory:
    path = Path(directory)
    (path / 'Tests.swift').write_text(source)
    subprocess.run(['swiftc', '-parse-as-library', str(path / 'Tests.swift'), '-o', str(path / 'tests')], check=True)
    # Isolate production orphan cleanup from concurrent tests and real recordings.
    subprocess.run([str(path / 'tests')], check=True, env={**os.environ, 'TMPDIR': str(path) + '/'})
