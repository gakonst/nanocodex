import AVFoundation
import Foundation
import InboxCore

/// A session-local, ordered queue. Cancellation revokes pending network audio as
/// well as the current player, so a late response cannot speak over a new turn.
@MainActor final class VoiceSpeechPlayer {
    private var pending: Task<Void, Never>?
    private var player: AVAudioPlayer?
    private var tasks: [UUID: Task<Void, Never>] = [:]
    private var epoch = UUID()
    private var queued = 0

    var level: Double {
        guard let player, player.isPlaying else { return 0 }
        player.updateMeters()
        return min(1, pow(10, Double(player.averagePower(forChannel: 0)) / 20))
    }

    func enqueue(audio: @escaping @Sendable () async throws -> Data,
                 onError: @escaping @MainActor (Error) -> Void) {
        guard queued < 32 else {
            onError(ManagedError(code: "speech_overflow", message: "Speech fell behind. Interrupt to continue."))
            return
        }
        queued += 1
        let previous = pending, token = epoch, id = UUID()
        pending = Task { [weak self] in
            await previous?.value
            guard let self, self.epoch == token, !Task.isCancelled else { return }
            defer { if self.epoch == token { self.queued -= 1; self.tasks.removeValue(forKey: id) } }
            do {
                let data = try await audio()
                try Task.checkCancellation()
                guard self.epoch == token else { return }
                let player = try AVAudioPlayer(data: data)
                player.isMeteringEnabled = true
                self.player = player
                guard player.prepareToPlay(), player.play() else {
                    throw ManagedError(code: "speech_playback", message: "Spoken audio could not play.")
                }
                while player.isPlaying {
                    try await Task.sleep(for: .milliseconds(50))
                    guard self.epoch == token else { return }
                }
                if self.epoch == token { self.player = nil }
            } catch is CancellationError {} catch {
                if self.epoch == token, !Task.isCancelled { onError(error) }
            }
        }
        tasks[id] = pending
    }

    func cancel() {
        epoch = UUID(); tasks.values.forEach { $0.cancel() }; tasks = [:]; pending = nil
        player?.stop(); player = nil; queued = 0
    }
}

/// Emits each completed caption once, preserving whole-response prosody.
/// Partial captions still identify the response to revoke on interruption.
struct VoiceSpeechCaptions {
    private var caption: UInt64?
    private var suppressed: UInt64?
    private var emittedFinal = false
    mutating func interrupt() {
        if let caption { suppressed = max(suppressed ?? 0, caption) }
    }
    func isSuppressed(_ entry: ManagedVoiceTranscript) -> Bool {
        entry.speaker == "assistant" && entry.id.map { id in suppressed.map { id <= $0 } ?? false } == true
    }
    mutating func consume(_ entry: ManagedVoiceTranscript) -> String? {
        guard entry.speaker == "assistant", let id = entry.id,
              suppressed.map({ id > $0 }) ?? true,
              caption.map({ id >= $0 }) ?? true else { return nil }
        if caption != id { caption = id; emittedFinal = false }
        guard entry.isFinal, !emittedFinal else { return nil }
        emittedFinal = true
        let text = entry.text.trimmingCharacters(in: .whitespacesAndNewlines)
        return text.isEmpty ? nil : text
    }
}
