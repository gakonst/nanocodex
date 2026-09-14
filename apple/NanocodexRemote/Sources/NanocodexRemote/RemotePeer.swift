import Foundation
import WebRTC

public struct RemoteICE: Codable, Sendable {
    public let urls: [String]
    public let username: String?
    public let credential: String?
    public init(urls: [String], username: String? = nil, credential: String? = nil) {
        self.urls = urls; self.username = username; self.credential = credential
    }
}

public struct RemoteSignal: Codable, Sendable {
    public enum Kind: String, Codable, Sendable { case offer, answer, candidate }
    public let type: Kind
    public var sdp: String?
    public var candidate: String?
    public var sdpMid: String?
    public var sdpMLineIndex: Int32?
    public init(type: Kind, sdp: String? = nil, candidate: String? = nil, sdpMid: String? = nil, sdpMLineIndex: Int32? = nil) {
        self.type = type; self.sdp = sdp; self.candidate = candidate
        self.sdpMid = sdpMid; self.sdpMLineIndex = sdpMLineIndex
    }
}

/// One screen connection. Media never traverses the managed HTTP/tool transport.
/// All session mutations are serialized; frame capture feeds RTCVideoSource directly.
@MainActor
public final class RemotePeer: NSObject {
    private static let factory: RTCPeerConnectionFactory = {
        let encoder = RTCDefaultVideoEncoderFactory()
        if let h264 = RTCDefaultVideoEncoderFactory.supportedCodecs().first(where: { $0.name == "H264" }) {
            encoder.preferredCodec = h264
        }
        let factory = RTCPeerConnectionFactory(encoderFactory: encoder, decoderFactory: RTCDefaultVideoDecoderFactory())
        // The native default excludes loopback. A paired phone host and its
        // local Mac viewer must be able to connect without hairpinning through
        // a VPN, virtual bridge, or public NAT mapping.
        let options = RTCPeerConnectionFactoryOptions()
        options.ignoreLoopbackNetworkAdapter = false
        factory.setOptions(options)
        return factory
    }()
    public let videoSource: RTCVideoSource
    public let localVideoTrack: RTCVideoTrack?
    public private(set) var remoteVideoTrack: RTCVideoTrack?
    public var onSignal: (RemoteSignal) -> Void = { _ in }
    public var onState: (RTCPeerConnectionState) -> Void = { _ in }
    public var onVideoTrack: (RTCVideoTrack) -> Void = { _ in }
    public var onData: (Data, Bool) -> Void = { _, _ in }
    public var onChannelsReady: () -> Void = {}
    private var connection: RTCPeerConnection!
    private var reliable: RTCDataChannel?
    private var motion: RTCDataChannel?
    private var pendingCandidates: [RTCIceCandidate] = []
    private var remoteDescriptionSet = false
    private var localDescriptionSent = false
    private var localCandidates: [RemoteSignal] = []
    private var negotiationDeadline: Task<Void, Never>?
    private var closed = false
    private let publishing: Bool
    private var gatheredCandidates = 0
    private var receivedCandidates = 0
    private var appliedCandidates = 0
    var diagnosticState: String { "\(connection.connectionState.rawValue)/\(connection.iceConnectionState.rawValue) channels=\(reliable?.readyState.rawValue ?? -1),\(motion?.readyState.rawValue ?? -1) ICE=\(gatheredCandidates)/\(receivedCandidates)/\(appliedCandidates) SDP=\(connection.localDescription != nil)/\(remoteDescriptionSet)" }
    func selectedLocalCandidate() async -> String? {
        await withCheckedContinuation { continuation in
            connection.statistics { report in
                for transport in report.statistics.values where transport.type == "transport" {
                    guard let pairID = transport.values["selectedCandidatePairId"] as? String,
                          let pair = report.statistics[pairID], let candidateID = pair.values["localCandidateId"] as? String,
                          let candidate = report.statistics[candidateID] else { continue }
                    continuation.resume(returning: ["candidateType", "address", "port"].map { candidate.values[$0]?.description ?? "" }.joined(separator: ":"))
                    return
                }
                continuation.resume(returning: nil)
            }
        }
    }
    func diagnosticICE(includeAddresses: Bool = true) async -> String {
        await withCheckedContinuation { continuation in
            connection.statistics { report in
                let pairs = report.statistics.values.filter { $0.type == "candidate-pair" }
                let values = pairs.prefix(12).map { pair -> [String: String] in
                    var result: [String: String] = [:]
                    for key in ["state", "nominated", "requestsSent", "responsesReceived", "requestsReceived", "responsesSent"] {
                        result[key] = pair.values[key]?.description
                    }
                    for side in ["local", "remote"] {
                        if let id = pair.values[side + "CandidateId"] as? String, let candidate = report.statistics[id] {
                            let keys = includeAddresses ? ["candidateType", "protocol", "address", "port"] : ["candidateType", "protocol"]
                            for key in keys { result[side + "." + key] = candidate.values[key]?.description }
                        }
                    }
                    return result
                }
                continuation.resume(returning: String(describing: values))
            }
        }
    }

    static func screenSource() -> RTCVideoSource { factory.videoSource(forScreenCast: true) }

    public init(publishing: Bool, ice: [RemoteICE], relayOnly: Bool = false, source: RTCVideoSource? = nil) throws {
        self.publishing = publishing
        videoSource = source ?? Self.screenSource()
        localVideoTrack = publishing ? Self.factory.videoTrack(with: videoSource, trackId: "screen") : nil
        super.init()
        let config = RTCConfiguration()
        config.sdpSemantics = .unifiedPlan; config.bundlePolicy = .maxBundle; config.rtcpMuxPolicy = .require
        config.iceTransportPolicy = relayOnly ? .relay : .all
        config.iceServers = ice.map { RTCIceServer(urlStrings: $0.urls, username: $0.username, credential: $0.credential) }
        guard let peer = Self.factory.peerConnection(with: config,
            constraints: RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil), delegate: self)
        else { throw RemoteError.unavailable }
        connection = peer
        if let localVideoTrack {
            let initOptions = RTCRtpTransceiverInit(); initOptions.direction = .sendOnly
            let encoding = RTCRtpEncodingParameters()
            encoding.maxBitrateBps = 12_000_000; encoding.maxFramerate = 60
            initOptions.sendEncodings = [encoding]
            peer.addTransceiver(with: localVideoTrack, init: initOptions)
            let controlConfig = RTCDataChannelConfiguration(); controlConfig.isOrdered = true
            reliable = peer.dataChannel(forLabel: "remote-control-v1", configuration: controlConfig)
            let motionConfig = RTCDataChannelConfiguration(); motionConfig.isOrdered = false; motionConfig.maxRetransmits = 0
            motion = peer.dataChannel(forLabel: "remote-motion-v1", configuration: motionConfig)
            reliable?.delegate = self; motion?.delegate = self
        }
    }

    public func updateICE(_ ice: [RemoteICE]) throws {
        guard !closed else { throw RemoteError.closed }
        let config = connection.configuration
        config.iceServers = ice.map { RTCIceServer(urlStrings: $0.urls, username: $0.username, credential: $0.credential) }
        guard connection.setConfiguration(config) else { throw RemoteError.unavailable }
    }

    public func restartICE(_ ice: [RemoteICE]) async throws {
        try updateICE(ice)
        try await offer(restart: true)
    }

    public func offer(restart: Bool = false) async throws {
        guard !closed, publishing else { throw RemoteError.closed }
        localDescriptionSent = false; remoteDescriptionSet = false
        negotiationDeadline?.cancel()
        negotiationDeadline = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(25)) } catch { return }
            guard let self, !remoteDescriptionSet else { return }; close()
        }
        let description: RTCSessionDescription = try await withCheckedThrowingContinuation { continuation in
            connection.offer(for: RTCMediaConstraints(mandatoryConstraints: restart ? ["IceRestart": "true"] : nil, optionalConstraints: nil)) { sdp, error in
                if let error { continuation.resume(throwing: error) }
                else if let sdp { continuation.resume(returning: sdp) }
                else { continuation.resume(throwing: RemoteError.unavailable) }
            }
        }
        try await setLocal(description)
        guard !closed else { throw RemoteError.closed }
        onSignal(.init(type: .offer, sdp: description.sdp))
        flushLocalCandidates()
    }

    public func receive(_ signal: RemoteSignal) async throws {
        guard !closed else { throw RemoteError.closed }
        if signal.type == .candidate {
            receivedCandidates += 1
            guard let value = signal.candidate, value.utf8.count <= 4096,
                  let index = signal.sdpMLineIndex, index >= 0, index <= 32 else { throw RemoteError.invalidMessage }
            let candidate = RTCIceCandidate(sdp: value, sdpMLineIndex: index, sdpMid: signal.sdpMid)
            if remoteDescriptionSet { try await connection.add(candidate); appliedCandidates += 1 }
            else {
                guard pendingCandidates.count < 128 else { throw RemoteError.invalidMessage }
                pendingCandidates.append(candidate)
            }
            return
        }
        guard let sdp = signal.sdp, !sdp.isEmpty, sdp.utf8.count <= 65_536,
              (signal.type == .answer) == publishing else { throw RemoteError.invalidMessage }
        if signal.type == .offer { localDescriptionSent = false }
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            connection.setRemoteDescription(.init(type: signal.type == .offer ? .offer : .answer, sdp: sdp)) { error in
                if let error { continuation.resume(throwing: error) } else { continuation.resume() }
            }
        }
        guard !closed else { throw RemoteError.closed }
        remoteDescriptionSet = true
        negotiationDeadline?.cancel(); negotiationDeadline = nil
        let candidates = pendingCandidates; pendingCandidates.removeAll()
        for candidate in candidates { try await connection.add(candidate); appliedCandidates += 1 }
        if signal.type == .offer {
            let answer: RTCSessionDescription = try await withCheckedThrowingContinuation { continuation in
                connection.answer(for: RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)) { sdp, error in
                    if let error { continuation.resume(throwing: error) }
                    else if let sdp { continuation.resume(returning: sdp) }
                    else { continuation.resume(throwing: RemoteError.unavailable) }
                }
            }
            try await setLocal(answer)
            guard !closed else { throw RemoteError.closed }
            onSignal(.init(type: .answer, sdp: answer.sdp))
            flushLocalCandidates()
        }
    }

    private func flushLocalCandidates() {
        localDescriptionSent = true
        let pending = localCandidates; localCandidates.removeAll()
        for signal in pending { onSignal(signal) }
    }

    private func setLocal(_ description: RTCSessionDescription) async throws {
        guard !closed else { throw RemoteError.closed }
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            connection.setLocalDescription(description) { error in
                if let error { continuation.resume(throwing: error) } else { continuation.resume() }
            }
        }
    }

    /// Motion is disposable. Discrete input must never accumulate or be replayed.
    @discardableResult public func send(_ data: Data, motion isMotion: Bool = false) throws -> Bool {
        guard data.count <= 8192 else { throw RemoteError.invalidMessage }
        guard !closed, let channel = isMotion ? motion : reliable, channel.readyState == .open else { throw RemoteError.closed }
        if channel.bufferedAmount > (isMotion ? 4096 : 32_768) {
            if isMotion { return false }
            close(); throw RemoteError.unavailable
        }
        guard channel.sendData(.init(data: data, isBinary: false)) else { throw RemoteError.unavailable }
        return true
    }

    public func close() {
        guard !closed else { return }; closed = true
        negotiationDeadline?.cancel(); negotiationDeadline = nil
        localVideoTrack?.isEnabled = false
        reliable?.delegate = nil; motion?.delegate = nil
        reliable?.close(); motion?.close(); reliable = nil; motion = nil
        connection.delegate = nil; connection.close(); pendingCandidates.removeAll(); localCandidates.removeAll()
        remoteVideoTrack = nil; onState(.closed)
    }
}

extension RemotePeer: RTCPeerConnectionDelegate, RTCDataChannelDelegate {
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didChange stateChanged: RTCSignalingState) {}
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didAdd stream: RTCMediaStream) {}
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didRemove stream: RTCMediaStream) {}
    nonisolated public func peerConnectionShouldNegotiate(_ peerConnection: RTCPeerConnection) {}
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCIceConnectionState) {}
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCIceGatheringState) {}
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didRemove candidates: [RTCIceCandidate]) {}
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didGenerate candidate: RTCIceCandidate) {
        let signal = RemoteSignal(type: .candidate, candidate: candidate.sdp, sdpMid: candidate.sdpMid, sdpMLineIndex: candidate.sdpMLineIndex)
        Task { @MainActor [weak self] in
            guard let self, !closed else { return }; gatheredCandidates += 1
            if localDescriptionSent { onSignal(signal) }
            else if localCandidates.count < 128 { localCandidates.append(signal) }
            else { close() }
        }
    }
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCPeerConnectionState) {
        Task { @MainActor [weak self] in guard let self, !closed else { return }; onState(newState) }
    }
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didAdd rtpReceiver: RTCRtpReceiver, streams: [RTCMediaStream]) {
        guard let track = rtpReceiver.track as? RTCVideoTrack else { return }
        Task { @MainActor [weak self] in guard let self, !closed else { return }; remoteVideoTrack = track; onVideoTrack(track) }
    }
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didOpen dataChannel: RTCDataChannel) {
        Task { @MainActor [weak self] in
            guard let self, !closed, !publishing else { dataChannel.close(); return }
            // ObjC exposes absent retransmission limits as uint16_t(-1).
            if dataChannel.label == "remote-control-v1", reliable == nil, dataChannel.isOrdered,
               dataChannel.maxRetransmits == UInt16.max, dataChannel.maxPacketLifeTime == UInt16.max { reliable = dataChannel }
            else if dataChannel.label == "remote-motion-v1", motion == nil, !dataChannel.isOrdered,
                    dataChannel.maxRetransmits == 0, dataChannel.maxPacketLifeTime == UInt16.max { motion = dataChannel }
            else { dataChannel.close(); close(); return }
            dataChannel.delegate = self
            if reliable?.readyState == .open, motion?.readyState == .open { onChannelsReady() }
        }
    }
    nonisolated public func dataChannelDidChangeState(_ dataChannel: RTCDataChannel) {
        Task { @MainActor [weak self] in
            guard let self, !closed else { return }
            if reliable?.readyState == .open, motion?.readyState == .open { onChannelsReady() }
            if dataChannel.readyState == .closed { close() }
        }
    }
    nonisolated public func dataChannel(_ dataChannel: RTCDataChannel, didReceiveMessageWith buffer: RTCDataBuffer) {
        guard buffer.data.count <= 8192 else { dataChannel.close(); return }
        let isMotion = dataChannel.label == "remote-motion-v1"
        Task { @MainActor [weak self] in guard let self, !closed else { return }; onData(buffer.data, isMotion) }
    }
}
