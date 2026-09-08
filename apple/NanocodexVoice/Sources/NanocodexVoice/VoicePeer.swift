import AVFoundation
import Foundation
import InboxCore
import WebRTC

enum VoicePeerSignal: Sendable { case connected, controlReady, disconnected, failed, interrupted }

struct VoiceAudioStats: Sendable {
    var inputLevel: Double = 0
    var outputLevel: Double = 0
    var playbackEnabled = false
    var bytesSent: UInt64 = 0
    var bytesReceived: UInt64 = 0
}

/// Owns the native WebRTC audio device. Factory/SDP work starts off the main
/// actor; audio uses WebRTC's echo cancellation and system input/output route.
final class VoicePeer: NSObject, RTCPeerConnectionDelegate, RTCDataChannelDelegate, @unchecked Sendable {
    private enum Factory {
        static let shared = RTCPeerConnectionFactory()
    }
    private let lock = NSLock()
    private var peer: RTCPeerConnection?
    private var microphone: RTCAudioTrack?
    private var channel: RTCDataChannel?
    let realtimeEvents: AsyncThrowingStream<JSON, Error>
    private let realtimeContinuation: AsyncThrowingStream<JSON, Error>.Continuation
    private var closed = false
    private var muted = false
    private var activated = false
    private var playbackEnabled = false
    private var loggedConnectionStats = false
    private let captureMicrophone: Bool
    private let onSignal: @Sendable (VoicePeerSignal) -> Void

    init(captureMicrophone: Bool = true, onSignal: @escaping @Sendable (VoicePeerSignal) -> Void) {
        let (events, continuation) = AsyncThrowingStream<JSON, Error>.makeStream(bufferingPolicy: .bufferingOldest(128))
        realtimeEvents = events; realtimeContinuation = continuation
        self.captureMicrophone = captureMicrophone; self.onSignal = onSignal
        super.init()
    }

    static func requestMicrophone() async -> Bool {
        voiceTiming("microphone.authorization.begin")
        defer { voiceTiming("microphone.authorization.end") }
        let status = AVCaptureDevice.authorizationStatus(for: .audio)
        voiceTiming("microphone.authorization.status.\(status.rawValue)")
        switch status {
        case .authorized: return true
        case .notDetermined:
            return await withCheckedContinuation { continuation in
                AVCaptureDevice.requestAccess(for: .audio) { continuation.resume(returning: $0) }
            }
        default: return false
        }
    }

    /// Load WebRTC while the conversation UI opens. This creates no call,
    /// audio track, or microphone capture and does not request permission.
    static func warmUp() {
        Task.detached(priority: .userInitiated) { _ = Factory.shared }
    }

    func offer() async throws -> String {
        try Task.checkCancellation()
        voiceTiming("peer.prepare.begin")
        let connection = try await Task.detached(priority: .userInitiated) { [self] in
            try self.prepare()
        }.value
        try Task.checkCancellation()
        voiceTiming("peer.prepare.end")
        let description: RTCSessionDescription = try await withCheckedThrowingContinuation { continuation in
            connection.offer(for: RTCMediaConstraints(mandatoryConstraints: ["OfferToReceiveAudio": "true"], optionalConstraints: nil)) { description, error in
                if let error { continuation.resume(throwing: error) }
                else if let description { continuation.resume(returning: description) }
                else { continuation.resume(throwing: VoiceFailure.connection) }
            }
        }
        try Task.checkCancellation()
        voiceTiming("peer.offer.created")
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            connection.setLocalDescription(description) { error in
                if let error { continuation.resume(throwing: error) }
                else { continuation.resume() }
            }
        }
        voiceTiming("peer.local-description.set")
        // The server answers with ICE candidates. Send the offer immediately;
        // waiting for every local interface to finish gathering adds no media
        // readiness and is unnecessary for this non-trickle exchange.
        guard !isClosed else { throw CancellationError() }
        let sdp = connection.localDescription?.sdp ?? description.sdp
        return sdp
    }

    private func prepare() throws -> RTCPeerConnection {
        guard !isClosed else { throw CancellationError() }
        let configuration = RTCConfiguration()
        configuration.sdpSemantics = .unifiedPlan
        configuration.bundlePolicy = .maxBundle
        configuration.rtcpMuxPolicy = .require
        let constraints = RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: ["DtlsSrtpKeyAgreement": "true"])
        let factory = Factory.shared
        guard let connection = factory.peerConnection(with: configuration, constraints: constraints, delegate: self) else { throw VoiceFailure.connection }
        var track: RTCAudioTrack?
        if captureMicrophone {
            let source = factory.audioSource(with: RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: [
                "googEchoCancellation": "true", "googNoiseSuppression": "true", "googAutoGainControl": "true",
            ]))
            track = factory.audioTrack(with: source, trackId: "nanocodex-microphone")
            track?.isEnabled = false
            connection.add(track!, streamIds: ["nanocodex-voice"])
        } else {
            let receiving = RTCRtpTransceiverInit(); receiving.direction = .recvOnly
            connection.addTransceiver(of: .audio, init: receiving)
        }
        let dataChannel = connection.dataChannel(forLabel: "oai-events", configuration: RTCDataChannelConfiguration())
        dataChannel?.delegate = self
        let accepted = lock.withLock { () -> Bool in
            guard !closed else { return false }
            peer = connection; microphone = track; channel = dataChannel
            return true
        }
        guard accepted else { connection.close(); throw CancellationError() }
        #if os(iOS)
        let session = RTCAudioSession.sharedInstance()
        session.lockForConfiguration()
        defer { session.unlockForConfiguration() }
        let audio = RTCAudioSessionConfiguration.webRTC()
        audio.category = AVAudioSession.Category.playAndRecord.rawValue
        audio.mode = AVAudioSession.Mode.voiceChat.rawValue
        audio.categoryOptions = [.defaultToSpeaker, .allowBluetooth]
        try session.setConfiguration(audio)
        #endif
        return connection
    }

    func answer(_ sdp: String) async throws {
        guard let connection = lock.withLock({ closed ? nil : peer }) else { throw CancellationError() }
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            connection.setRemoteDescription(RTCSessionDescription(type: .answer, sdp: sdp)) { error in
                if let error { continuation.resume(throwing: error) }
                else { continuation.resume() }
            }
        }
        try Task.checkCancellation()
    }

    func setMuted(_ value: Bool) {
        let (track, enabled) = lock.withLock { muted = value; return (microphone, activated && !value && !closed) }
        track?.isEnabled = enabled
    }

    func pauseMicrophone() {
        let track = lock.withLock { activated = false; return microphone }
        track?.isEnabled = false
    }

    func activateMicrophone() {
        let (track, enabled) = lock.withLock { activated = true; return (microphone, !muted && !closed) }
        track?.isEnabled = enabled
    }

    func setPlaybackEnabled(_ value: Bool) {
        let connection = lock.withLock { playbackEnabled = value; return closed ? nil : peer }
        for receiver in connection?.receivers ?? [] { receiver.track?.isEnabled = value }
        if voiceTimingEnabled {
            voiceTiming("playback enabled=\(value) receivers=\(connection?.receivers.count ?? 0)")
        }
    }

    func send(_ frame: JSON) throws {
        let data = try JSONEncoder().encode(frame)
        guard data.count <= 65_536 else { throw VoiceFailure.connection }
        let current = lock.withLock { closed ? nil : channel }
        guard let current, current.readyState == .open, current.bufferedAmount <= 512 * 1024,
              current.sendData(RTCDataBuffer(data: data, isBinary: false)) else { throw VoiceFailure.connection }
        if voiceTimingEnabled {
            voiceTiming("send type=\(frame["type"].string) channel=\(frame["channel"].string) bytes=\(data.count)")
        }
    }

    /// The capture track and peer stop before protocol cleanup can suspend.
    func close() {
        // WebRTC dispatches synchronously to its signaling/worker threads.
        // Release our lock first so their callbacks can observe closure.
        let resources = lock.withLock { () -> (RTCPeerConnection?, RTCAudioTrack?, RTCDataChannel?)? in
            guard !closed else { return nil }
            closed = true
            let resources = (peer, microphone, channel)
            peer = nil; microphone = nil; channel = nil
            return resources
        }
        guard let (connection, track, dataChannel) = resources else { return }
        track?.isEnabled = false
        dataChannel?.delegate = nil
        if dataChannel?.readyState == .open {
            _ = dataChannel?.sendData(RTCDataBuffer(data: Data(#"{"type":"session.close"}"#.utf8), isBinary: false))
        }
        dataChannel?.close()
        realtimeContinuation.finish()
        connection?.delegate = nil
        for receiver in connection?.receivers ?? [] { receiver.track?.isEnabled = false }
        connection?.close()
    }

    private var isClosed: Bool { lock.withLock { closed } }
    private func signal(_ value: VoicePeerSignal) { if !isClosed { onSignal(value) } }

    func statistics() async -> VoiceAudioStats {
        guard let connection = lock.withLock({ closed ? nil : peer }) else { return .init() }
        return await withCheckedContinuation { continuation in
            connection.statistics { report in
                var result = VoiceAudioStats()
                result.playbackEnabled = self.lock.withLock { self.playbackEnabled && !self.closed }
                if voiceTimingEnabled,
                   let transport = report.statistics.values.first(where: { $0.type == "transport" }),
                   let pairID = transport.values["selectedCandidatePairId"] as? String,
                   let pair = report.statistics[pairID],
                   let roundTrip = pair.values["currentRoundTripTime"] as? NSNumber,
                   self.lock.withLock({ if self.loggedConnectionStats { return false }; self.loggedConnectionStats = true; return true }) {
                    // Timing only: never log candidate addresses, SDP or keys.
                    voiceTiming("network round_trip_ms=\(Int(roundTrip.doubleValue * 1_000))")
                }
                for statistic in report.statistics.values {
                    let values = statistic.values
                    if statistic.type == "media-source" { result.inputLevel = (values["audioLevel"] as? NSNumber)?.doubleValue ?? result.inputLevel }
                    if statistic.type == "inbound-rtp", values["kind"] as? String == "audio" {
                        result.outputLevel = (values["audioLevel"] as? NSNumber)?.doubleValue ?? result.outputLevel
                        result.bytesReceived += (values["bytesReceived"] as? NSNumber)?.uint64Value ?? 0
                    }
                    if statistic.type == "outbound-rtp", values["kind"] as? String == "audio" {
                        result.bytesSent += (values["bytesSent"] as? NSNumber)?.uint64Value ?? 0
                    }
                }
                continuation.resume(returning: result)
            }
        }
    }

    func dataChannelDidChangeState(_ dataChannel: RTCDataChannel) {
        if dataChannel.readyState == .open { voiceTiming("data-channel.ready"); signal(.controlReady) }
        else if dataChannel.readyState == .closed { signal(.failed) }
    }
    func dataChannel(_ dataChannel: RTCDataChannel, didReceiveMessageWith buffer: RTCDataBuffer) {
        guard !isClosed else { return }
        do {
            guard buffer.data.count <= 256 * 1024 else { throw VoiceFailure.connection }
            let event = try JSONDecoder().decode(JSON.self, from: buffer.data)
            guard case .object = event else { throw VoiceFailure.connection }
            if case .dropped = realtimeContinuation.yield(event) { throw VoiceFailure.connection }
        } catch {
            realtimeContinuation.finish(throwing: VoiceFailure.connection)
            signal(.failed)
        }
    }

    func peerConnection(_ peerConnection: RTCPeerConnection, didChange stateChanged: RTCSignalingState) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didAdd stream: RTCMediaStream) {
        let enabled = lock.withLock { playbackEnabled && !closed }
        for track in stream.audioTracks { track.isEnabled = enabled }
    }
    func peerConnection(_ peerConnection: RTCPeerConnection, didRemove stream: RTCMediaStream) {}
    func peerConnectionShouldNegotiate(_ peerConnection: RTCPeerConnection) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCIceConnectionState) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCPeerConnectionState) {
        // Report readiness only when the secure media transport is connected.
        if newState == .connected { signal(.connected) }
        else if newState == .failed { signal(.failed) }
        else if newState == .disconnected { signal(.disconnected) }
    }
    func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCIceGatheringState) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didGenerate candidate: RTCIceCandidate) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didRemove candidates: [RTCIceCandidate]) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didOpen dataChannel: RTCDataChannel) {}
}

enum VoiceFailure: LocalizedError {
    case microphone, connection, interrupted
    var errorDescription: String? {
        switch self {
        case .microphone: "Allow Microphone access in Settings to talk with Nanocodex."
        case .connection: "Voice could not connect. Check your connection and try again."
        case .interrupted: "Voice was interrupted. Tap Start voice when you’re ready."
        }
    }
}

let voiceTimingEnabled = ProcessInfo.processInfo.environment["NANOCODEX_VOICE_TIMING"] == "1"
private let voiceTimingLock = NSLock()

func voiceTiming(_ stage: String) {
    if voiceTimingEnabled {
        let line = "VOICE_TIMING \(Date().timeIntervalSince1970) \(stage)"
        print(line)
        // Opt-in diagnostics survive device tests whose app stdout is unavailable.
        // Callers pass stage names and numeric metrics, never speech or credentials.
        let directory = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
        let url = ProcessInfo.processInfo.environment["NANOCODEX_VOICE_TIMING_LOG"].map { URL(fileURLWithPath: $0) }
            ?? directory?.appendingPathComponent("voice-timing.log")
        guard let url, let data = (line + "\n").data(using: .utf8) else { return }
        voiceTimingLock.withLock {
            if !FileManager.default.fileExists(atPath: url.path) { FileManager.default.createFile(atPath: url.path, contents: nil) }
            guard let handle = try? FileHandle(forWritingTo: url) else { return }
            defer { try? handle.close() }
            if let size = try? handle.seekToEnd(), size > 2_000_000 { try? handle.truncate(atOffset: 0); try? handle.seek(toOffset: 0) }
            try? handle.write(contentsOf: data)
        }
    }
}
