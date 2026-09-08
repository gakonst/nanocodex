import AVFoundation
import Combine
import Foundation
import InboxCore

/// The account key stays private to the managed service transport; it is never
/// included in provider SDP, audio, visible state, or transcript records.
public struct VoiceConfiguration: Sendable {
    let baseURL: URL
    let apiKey: String
    let agentID: String
    let conversationTitle: String?
    public var voice: String
    public init(baseURL: URL, apiKey: String, agentID: String, conversationTitle: String? = nil, voice: String = "cove") {
        self.baseURL = baseURL; self.apiKey = apiKey; self.agentID = agentID; self.conversationTitle = conversationTitle
        self.voice = voice
    }
}

public struct VoiceTranscript: Identifiable, Equatable, Sendable {
    public let id: UUID
    public let speaker: String
    public let text: String
}

/// Spoken rows change at transcript frequency, independently of the audio meter.
/// Keep unacknowledged speech visible after stopping until durable history owns it.
@MainActor public final class VoiceTranscriptFeed: ObservableObject {
    @Published public private(set) var conversations: [String: [VoiceTranscript]] = [:]
    private var durableIDs: [String: Set<String>] = [:]
    private var startedAfter: [String: Cursor] = [:]
    private var awaiting: [String: [TranscriptRow]] = [:]
    private var acknowledged = Set<UUID>()

    public func begin(conversationID: String, durableRows: [TranscriptRow], after cursor: Cursor? = nil) {
        // A previous call may have finished while this chat was offscreen.
        // Settle its pending rows before installing the new call's boundary.
        if startedAfter[conversationID] != nil { reconcile(conversationID: conversationID, durableRows: durableRows) }
        startedAfter[conversationID] = cursor ?? durableRows.compactMap(\.cursor).max() ?? .zero
        durableIDs[conversationID, default: []].formUnion(durableRows.filter { $0.id.contains(":voice:") }.map(\.id))
        awaiting[conversationID] = []
    }
    public func reconcile(conversationID: String, durableRows: [TranscriptRow]) {
        let spoken = durableRows.filter { $0.id.contains(":voice:") }
        guard let boundary = startedAfter[conversationID] else {
            durableIDs[conversationID, default: []].formUnion(spoken.map(\.id))
            return
        }
        let incoming = spoken.filter { row in
            guard let cursor = row.cursor else { return false }
            return cursor > boundary && durableIDs[conversationID]?.contains(row.id) != true
        }
        durableIDs[conversationID, default: []].formUnion(spoken.map(\.id))
        awaiting[conversationID, default: []].append(contentsOf: incoming)
        settle(conversationID)
    }
    fileprivate func update(_ transcripts: [VoiceTranscript], conversationID: String) {
        var current = conversations[conversationID] ?? []
        for transcript in transcripts where !acknowledged.contains(transcript.id) {
            if let index = current.firstIndex(where: { $0.id == transcript.id }) { current[index] = transcript }
            else { current.append(transcript) }
        }
        current = Array(current.suffix(80))
        if conversations[conversationID] != current { conversations[conversationID] = current }
        settle(conversationID)
    }
    private func settle(_ conversationID: String) {
        var current = conversations[conversationID] ?? []
        var remaining: [TranscriptRow] = []
        for row in awaiting[conversationID] ?? [] {
            if let index = current.firstIndex(where: { ($0.speaker == "user" ? "You" : "Agent") == row.role && $0.text == row.text }) {
                acknowledged.insert(current.remove(at: index).id)
            } else { remaining.append(row) }
        }
        awaiting[conversationID] = Array(remaining.suffix(80))
        if conversations[conversationID] != current { conversations[conversationID] = current }
    }
    public func clear() { conversations = [:]; durableIDs = [:]; startedAfter = [:]; awaiting = [:]; acknowledged = [] }
}

@MainActor public final class VoiceSession: ObservableObject {
    @Published public var settings = VoiceSettings.load() { didSet { settings.save() } }
    public enum Phase: Equatable { case idle, connecting, active, ended, failed }
    @Published public private(set) var phase: Phase = .idle
    @Published public private(set) var isMuted = false
    @Published public private(set) var transcripts: [VoiceTranscript] = []
    @Published public private(set) var errorMessage: String?
    @Published public private(set) var inputLevel: Double = 0
    @Published public private(set) var outputLevel: Double = 0
    @Published public private(set) var isWorking = false
    @Published public private(set) var isReconnecting = false
    @Published public private(set) var audioBytesSent: UInt64 = 0
    @Published public private(set) var audioBytesReceived: UInt64 = 0
    @Published public private(set) var conversationID: String?
    @Published public private(set) var conversationTitle: String?
    public let transcriptFeed = VoiceTranscriptFeed()
    public var isEngaged: Bool { phase == .connecting || phase == .active }
    public var status: String {
        if phase == .connecting { return "Connecting…" }
        if phase == .failed { return "Voice paused" }
        if phase == .ended || phase == .idle { return "Ready to talk" }
        if isReconnecting { return "Reconnecting…" }
        if isMuted { return "Microphone muted" }
        if outputLevel > 0.015 { return "Speaking" }
        if isWorking { return "Working on it" }
        return "Listening"
    }

    private var generation = UUID()
    private var peer: VoicePeer?
    private var transport: ManagedVoiceTransport?
    private var protocolState: ManagedVoiceProtocol?
    private var sessionID: String?
    private var startup: Task<Void, Never>?
    private var startupDeadline: Task<Void, Never>?
    private var negotiation: Task<Void, Never>?
    private var eventPreparation: Task<Void, Never>?
    private var admission: Task<Void, Never>?
    private var cleanup: Task<Void, Never>?
    private var incoming: Task<Void, Never>?
    private var agentEvents: Task<Void, Never>?
    private var meter: Task<Void, Never>?
    private var recovery: Task<Void, Never>?
    private var flushTask: Task<Void, Never>?
    private var prefetchTask: Task<Void, Never>?
    private var writer: Task<Void, Never>?
    private var routing: Task<Void, Never>?
    private var delegationQueue: [ManagedVoiceDelegation] = []
    private var frameQueue: [ManagedVoiceEffects] = []
    private var peerConnected = false
    private var controlConnected = false
    private var conversationReady = false
    private var agentEventsReady = false
    private var startupContextFrames: [JSON] = []
    private var startedTurnID: String?
    private var activeTurnID: String?
    private var routePending = false
    private var bufferedEvents: [AgentEvent] = []
    private var delegationOperations: [String: String] = [:]
    private var observers: [NSObjectProtocol] = []
    private var partialTranscriptIDs: [String: UUID] = [:]

    public init() {
        VoicePeer.warmUp()
        #if os(iOS)
        observers.append(NotificationCenter.default.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: .main) { [weak self] notification in
            guard (notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt) == AVAudioSession.InterruptionType.began.rawValue else { return }
            Task { @MainActor in self?.interrupt() }
        })
        observers.append(NotificationCenter.default.addObserver(forName: AVAudioSession.mediaServicesWereResetNotification, object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.interrupt() }
        })
        observers.append(NotificationCenter.default.addObserver(forName: AVAudioSession.routeChangeNotification, object: nil, queue: .main) { [weak self] notification in
            // Removing a headset must not unexpectedly expose the conversation
            // on the speaker. A deliberate next tap can establish a new route.
            guard (notification.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt) == AVAudioSession.RouteChangeReason.oldDeviceUnavailable.rawValue else { return }
            Task { @MainActor in self?.interrupt() }
        })
        #endif
    }

    deinit {
        observers.forEach(NotificationCenter.default.removeObserver)
        peer?.close()
        startup?.cancel(); startupDeadline?.cancel(); negotiation?.cancel(); eventPreparation?.cancel(); admission?.cancel()
        incoming?.cancel(); agentEvents?.cancel(); meter?.cancel(); recovery?.cancel(); flushTask?.cancel(); prefetchTask?.cancel(); writer?.cancel(); routing?.cancel()
    }

    public func speak(_ text: String) throws {
        guard phase == .active, let protocolState else { throw VoiceFailure.connection }
        apply(try protocolState.appendSpeech(text), token: generation)
    }

    public func appendText(_ text: String, role: String = "user") throws {
        guard phase == .active, let protocolState else { throw VoiceFailure.connection }
        apply(try protocolState.appendText(text, role: role), token: generation)
    }

    public func appendContext(_ text: String) throws {
        guard phase == .active, let protocolState else { throw VoiceFailure.connection }
        apply(try protocolState.appendContext(text), token: generation)
    }

    public func restart(using configuration: @escaping @MainActor () async throws -> VoiceConfiguration) {
        begin(configuration: configuration, captureMicrophone: true)
    }

    public func start(configuration: VoiceConfiguration) {
        begin(configuration: { configuration }, captureMicrophone: true)
    }

    /// The session owns preparation so a minimized panel cannot cancel it.
    /// Validate the conversation before requesting microphone permission.
    public func start(using configuration: @escaping @MainActor () async throws -> VoiceConfiguration) {
        guard !isEngaged else { return }
        begin(configuration: configuration, captureMicrophone: true)
    }

    private func begin(configuration: @escaping @MainActor () async throws -> VoiceConfiguration, captureMicrophone: Bool,
                       timeout: Duration = .seconds(45), transportOverride: ManagedVoiceTransport? = nil) {
        stop()
        let priorCleanup = cleanup
        let token = UUID(); generation = token
        voiceTiming("tap")
        phase = .connecting; errorMessage = nil; transcripts = []; isMuted = false
        #if DEBUG
        receivedRealtimeTypesForTesting = []
        #endif
        // This runs independently of SDK callbacks and configuration work,
        // neither of which is guaranteed to respond to task cancellation.
        startupDeadline = Task { [weak self] in
            do { try await Task.sleep(for: timeout) } catch { return }
            guard let self, self.generation == token, self.phase == .connecting else { return }
            voiceTiming("startup.timeout")
            self.fail(ManagedError(code: "voice_startup_timeout", message: "Voice is taking too long to connect. Please try again."))
        }
        startup = Task { [weak self] in
            guard let self else { return }
            do {
                let prepared = try await configuration()
                voiceTiming("configuration.ready")
                try self.check(token)
                self.conversationID = prepared.agentID
                self.conversationTitle = prepared.conversationTitle?.trimmingCharacters(in: .whitespacesAndNewlines)
                try await self.connect(prepared, captureMicrophone: captureMicrophone, priorCleanup: priorCleanup,
                                       token: token, transportOverride: transportOverride)
            } catch is CancellationError {
                if self.generation == token { self.stop() }
            } catch { if self.generation == token { self.fail(error) } }
        }
    }

    public func reportStartFailure(_ error: Error) {
        stop(); phase = .failed; errorMessage = Self.safeError(error)
    }

    private func connect(_ configuration: VoiceConfiguration, captureMicrophone: Bool, priorCleanup: Task<Void, Never>?, token: UUID,
                         transportOverride: ManagedVoiceTransport?) async throws {
        if captureMicrophone {
            guard await VoicePeer.requestMicrophone() else { throw VoiceFailure.microphone }
        }
        try check(token)
        let voiceTransport = try transportOverride ?? ManagedVoiceTransport(credential: .init(origin: configuration.baseURL.absoluteString, apiKey: configuration.apiKey), agentID: configuration.agentID)
        voiceTiming("transport.prepared")
        transport = voiceTransport
        let id = ManagedVoiceProtocol.sessionID()
        sessionID = id
        var callSettings = settings
        callSettings.voice = configuration.voice
        protocolState = try ManagedVoiceProtocol(settings: callSettings)
        protocolState?.bindSession(id)
        let audio = VoicePeer(captureMicrophone: captureMicrophone) { [weak self] signal in
            Task { @MainActor in self?.receive(signal, token: token) }
        }
        peer = audio
        audio.setMuted(isMuted)
        // Each branch reports failure immediately. Awaiting async-let results in
        // a fixed order used to hide admission/event errors behind a slow SDP call.
        negotiation = Task { [weak self] in
            guard let self else { return }
            do {
                let sdp = try await audio.offer()
                try self.check(token)
                voiceTiming("call.begin")
                let call = try await voiceTransport.call(sdp: sdp, instructions: ManagedVoiceProtocol.instructions(), sessionID: id, settings: callSettings)
                try self.check(token)
                voiceTiming("call.end")
                self.startRealtimeEvents(audio, token: token)
                try await audio.answer(call.sdp)
                try self.check(token)
                voiceTiming("peer.answer.applied")
                self.startMeter(audio, token: token)
            } catch { self.startupFailed(error, token: token) }
        }
        eventPreparation = Task { [weak self] in
            guard let self else { return }
            do {
                try await self.prepareAgentEvents(voiceTransport, token: token)
                try self.check(token)
                self.agentEventsReady = true
                voiceTiming("events.ready")
                self.becomeActiveIfReady()
            } catch { self.startupFailed(error, token: token) }
        }
        admission = Task { [weak self] in
            guard let self else { return }
            do {
                try await self.prepareConversation(voiceTransport, sessionID: id, priorCleanup: priorCleanup, token: token)
                try self.check(token)
                self.conversationReady = true
                self.becomeActiveIfReady()
            } catch { self.startupFailed(error, token: token) }
        }
    }

    private func startupFailed(_ error: Error, token: UUID) {
        guard generation == token else { return }
        voiceTiming("startup.failed")
        fail(error)
    }

    private func prepareConversation(_ transport: ManagedVoiceTransport, sessionID: String, priorCleanup: Task<Void, Never>?, token: UUID) async throws {
        if priorCleanup != nil { voiceTiming("lifecycle.prior-cleanup.wait") }
        try await waitForCleanup(priorCleanup)
        try check(token)
        voiceTiming("lifecycle.start.begin")
        let context = try await transport.start(sessionID: sessionID, operationID: UUID().uuidString.lowercased())
        try check(token)
        voiceTiming("lifecycle.start.end")
        startupContextFrames = ManagedVoiceProtocol.startupContextFrames(context)
    }

    private func waitForCleanup(_ prior: Task<Void, Never>?) async throws {
        guard let prior else { return }
        // Cancel this wait without cancelling the prior session's durable work.
        let (stream, continuation) = AsyncStream<Void>.makeStream(bufferingPolicy: .bufferingOldest(1))
        let waiter = Task { await prior.value; continuation.yield(()); continuation.finish() }
        defer { waiter.cancel(); continuation.finish() }
        try await withTaskCancellationHandler {
            for await _ in stream { try Task.checkCancellation(); return }
            throw CancellationError()
        } onCancel: { continuation.finish() }
    }

    private func startMeter(_ audio: VoicePeer, token: UUID) {
        meter = Task { [weak self, audio] in
            var samples = 0
            var audible = false
            while !Task.isCancelled {
                let stats = await audio.statistics()
                guard let self, self.generation == token else { return }
                self.inputLevel = self.isMuted ? 0 : min(1, max(0, stats.inputLevel))
                self.outputLevel = min(1, max(0, stats.outputLevel))
                self.audioBytesSent = stats.bytesSent; self.audioBytesReceived = stats.bytesReceived
                if voiceTimingEnabled {
                    let outputActive = stats.playbackEnabled && stats.outputLevel > 0.001
                    if outputActive != audible {
                        audible = outputActive
                        voiceTiming(outputActive ? "audio.output.started" : "audio.output.quiet")
                    }
                    if samples.isMultiple(of: 5) {
                        voiceTiming("audio sent=\(stats.bytesSent) received=\(stats.bytesReceived) input=\(stats.inputLevel) playback=\(stats.playbackEnabled) output=\(stats.outputLevel)")
                    }
                }
                samples += 1
                do { try await Task.sleep(for: .milliseconds(200)) } catch { return }
            }
        }
    }

    private func check(_ token: UUID) throws {
        try Task.checkCancellation()
        guard token == generation else { throw CancellationError() }
    }

    private func receive(_ signal: VoicePeerSignal, token: UUID) {
        guard token == generation, isEngaged else { return }
        switch signal {
        case .connected:
            voiceTiming("peer.connected")
            recovery?.cancel(); recovery = nil
            peerConnected = true; becomeActiveIfReady()
        case .controlReady:
            controlConnected = true
            if let effects = protocolState?.sidebandOpened() { apply(effects, token: token) }
            becomeActiveIfReady()
        case .disconnected:
            peer?.pauseMicrophone()
            peerConnected = false; isReconnecting = true
            guard recovery == nil else { return }
            recovery = Task { [weak self] in
                do { try await Task.sleep(for: .seconds(8)) } catch { return }
                guard let self, self.generation == token, !self.peerConnected else { return }
                self.fail(VoiceFailure.interrupted)
            }
        case .failed, .interrupted: fail(VoiceFailure.interrupted)
        }
    }

    private func becomeActiveIfReady() {
        guard peerConnected && controlConnected && conversationReady && agentEventsReady else { return }
        do {
            if !startupContextFrames.isEmpty {
                for frame in startupContextFrames { try peer?.send(frame) }
                startupContextFrames = []
                voiceTiming("context.sent")
            }
            peer?.activateMicrophone(); phase = .active; isReconnecting = false
            startupDeadline?.cancel(); startupDeadline = nil
            voiceTiming("voice.ready")
        } catch { fail(error) }
    }

    private func startRealtimeEvents(_ audio: VoicePeer, token: UUID) {
        incoming = Task { [weak self] in
            do {
                for try await event in audio.realtimeEvents {
                    guard let self else { return }
                    try self.check(token)
                    try self.realtime(event, token: token)
                }
                if let self, self.generation == token, !Task.isCancelled { self.fail(VoiceFailure.interrupted) }
            } catch is CancellationError {} catch {
                if let self, self.generation == token, !Task.isCancelled { self.fail(error) }
            }
        }
    }

    private func realtime(_ event: JSON, token: UUID) throws {
        if event["type"].string == "delegation.created", delegationOperations[event["item"]["id"].string] != nil {
            return // A replay must not consume a new transcript or route twice.
        }
        #if DEBUG
        receivedRealtimeTypesForTesting.insert(event["type"].string)
        #endif
        if voiceTimingEnabled {
            let type = event["type"].string
            if type.range(of: "^[a-z_.]{1,80}$", options: .regularExpression) != nil { voiceTiming("realtime.\(type)") }
        }
        guard let update = protocolState?.realtimeMessage(event) else { return }
        if let prefetch = update.prefetch, let transport, let sessionID {
            prefetchTask?.cancel()
            prefetchTask = Task { [weak self] in
                do {
                    try await Task.sleep(for: .milliseconds(prefetch.debounceMS))
                    guard let self else { return }
                    try self.check(token)
                    try await transport.prefetch(sessionID: sessionID, query: prefetch.query)
                } catch { /* Speculation never blocks the authoritative first turn. */ }
            }
        }
        apply(update.effects, token: token)
        try check(token)
        guard let delegation = update.delegation, transport != nil, sessionID != nil else { return }
        prefetchTask?.cancel(); prefetchTask = nil
        let operation = delegationOperations[delegation.id] ?? UUID().uuidString.lowercased()
        delegationOperations[delegation.id] = operation
        guard delegationOperations.count <= 512, delegationQueue.count < 32 else { throw VoiceFailure.connection }
        delegationQueue.append(delegation)
        startRouting(token: token)
    }

    /// Keep durable admissions ordered without making incoming speech and
    /// transcript deltas wait for their network round trip.
    private func startRouting(token: UUID) {
        guard routing == nil, let transport, let sessionID else { return }
        routing = Task { [weak self] in
            guard let self else { return }
            do {
                while !self.delegationQueue.isEmpty {
                    try self.check(token)
                    let delegation = self.delegationQueue.removeFirst()
                    guard let operation = self.delegationOperations[delegation.id] else { throw VoiceFailure.connection }
                    try await self.route(delegation, operation: operation, transport: transport, sessionID: sessionID, token: token)
                }
                try self.check(token)
                self.routing = nil
            } catch is CancellationError {} catch {
                if !Task.isCancelled, self.generation == token { self.fail(error) }
            }
        }
    }

    private func route(_ delegation: ManagedVoiceDelegation, operation: String, transport: ManagedVoiceTransport, sessionID: String, token: UUID) async throws {
        routePending = true; isWorking = true
        defer { if generation == token { routePending = false } }
        voiceTiming("delegate.begin")
        let route = try await transport.delegate(sessionID: sessionID, operationID: operation, input: delegation.formattedInput)
        try check(token)
        voiceTiming("delegate.end")
        activeTurnID = route.turnID
        if route.route == "started" { startedTurnID = route.turnID }
        let buffered = bufferedEvents; bufferedEvents = []
        for event in buffered { observe(event, token: token) }
    }

    private func prepareAgentEvents(_ transport: ManagedVoiceTransport, token: UUID) async throws {
        let events = try await transport.events()
        try check(token)
        // Consume immediately even during negotiation; unrelated active turns
        // must not fill the bounded stream while voice is still connecting.
        startAgentEvents(events, token: token)
    }

    private func startAgentEvents(_ events: AsyncThrowingStream<AgentEvent, Error>, token: UUID) {
        agentEvents = Task { [weak self] in
            do {
                for try await event in events {
                    guard let self else { return }
                    try self.check(token)
                    if voiceTimingEnabled {
                        let type = event.data["event"]["type"].string
                        if type.range(of: "^[a-z_.]{1,80}$", options: .regularExpression) != nil { voiceTiming("agent.received.\(type)") }
                    }
                    if self.routePending && event.turnID != self.activeTurnID
                        && event.data["event"]["type"].string != "managed.voice.context" {
                        guard self.bufferedEvents.count < 256 else { throw VoiceFailure.connection }
                        self.bufferedEvents.append(event)
                    } else { self.observe(event, token: token) }
                }
            } catch is CancellationError {} catch {
                if !Task.isCancelled, let self, self.generation == token { self.fail(error) }
            }
        }
    }

    private func observe(_ event: AgentEvent, token: UUID) {
        guard event.data["type"].string == "event" else { return }
        let raw = event.data["event"]
        if raw["type"].string == "managed.voice.context" {
            if let effects = protocolState?.managedEvent(raw, cursor: event.cursor.rawValue) { apply(effects, token: token) }
            return
        }
        guard event.turnID == activeTurnID, activeTurnID != nil else { return }
        if voiceTimingEnabled {
            let type = raw["type"].string
            if type.range(of: "^[a-z_.]{1,80}$", options: .regularExpression) != nil { voiceTiming("agent.applied.\(type)") }
        }
        if let effects = protocolState?.agentEvent(raw) { apply(effects, token: token) }
        if ["run.completed", "run.failed", "run.cancelled"].contains(raw["type"].string) {
            if startedTurnID == activeTurnID { startedTurnID = nil }
            activeTurnID = nil; isWorking = false
        }
    }

    private func apply(_ effects: ManagedVoiceEffects, token: UUID) {
        guard token == generation else { return }
        if effects.playbackEnabled == false { peer?.setPlaybackEnabled(false) }
        let visibleTranscripts = effects.transcripts.flatMap { transcript in
            RealtimeTranscript.project(transcript.text, isPartial: !transcript.isFinal)?.map {
                ManagedVoiceTranscript(speaker: $0.speaker, text: $0.text, isFinal: transcript.isFinal)
            } ?? [transcript]
        }
        for transcript in visibleTranscripts {
            let text = String(transcript.text.prefix(8_192))
            if let partialID = partialTranscriptIDs[transcript.speaker],
               let index = transcripts.firstIndex(where: { $0.id == partialID }) {
                transcripts[index] = .init(id: partialID, speaker: transcript.speaker, text: text)
            } else {
                transcripts.append(.init(id: UUID(), speaker: transcript.speaker, text: text))
                partialTranscriptIDs[transcript.speaker] = transcripts.last?.id
            }
            if transcript.isFinal { partialTranscriptIDs.removeValue(forKey: transcript.speaker) }
        }
        if transcripts.count > 80 { transcripts.removeFirst(transcripts.count - 80) }
        if !visibleTranscripts.isEmpty, let conversationID { transcriptFeed.update(transcripts, conversationID: conversationID) }
        if let reason = effects.terminate {
            fail(ManagedError(code: "voice_error", message: reason)); return
        }
        if effects.scheduleFlush && flushTask == nil {
            flushTask = Task { [weak self] in
                do { try await Task.sleep(for: .milliseconds(200)) } catch { return }
                guard let self, self.generation == token else { return }
                self.flushTask = nil
                if let next = self.protocolState?.flush() { self.apply(next, token: token) }
            }
        }
        if !effects.frames.isEmpty && controlConnected {
            guard frameQueue.count < 128 else { fail(VoiceFailure.connection); return }
            frameQueue.append(effects)
            startWriter(token: token)
        } else if effects.frames.isEmpty && effects.playbackEnabled == true {
            peer?.setPlaybackEnabled(true)
        }
    }

    private func startWriter(token: UUID) {
        guard writer == nil, let peer else { return }
        writer = Task { [weak self] in
            guard let self else { return }
            do {
                while !self.frameQueue.isEmpty {
                    try self.check(token)
                    let effects = self.frameQueue.removeFirst()
                    for frame in effects.frames {
                        try peer.send(frame)
                        try self.check(token)
                        if effects.acknowledgeFrames { self.protocolState?.framesSent(1) }
                    }
                    if effects.playbackEnabled == true { peer.setPlaybackEnabled(true) }
                }
                self.writer = nil
            } catch is CancellationError {} catch {
                guard !Task.isCancelled, self.generation == token else { return }
                self.writer = nil; self.frameQueue = []
                self.fail(error)
            }
        }
    }

    public func toggleMute() {
        guard isEngaged else { return }
        isMuted.toggle(); peer?.setMuted(isMuted)
        if isMuted { inputLevel = 0 }
    }

    public func cancelTurn() {
        guard let transport, let turnID = startedTurnID else { return }
        let token = generation
        Task {
            do {
                try await transport.cancel(turnID: turnID)
                if generation == token, startedTurnID == turnID { startedTurnID = nil; isWorking = false }
            } catch { if generation == token { errorMessage = Self.safeError(error) } }
        }
    }

    public func stop() {
        generation = UUID()
        // Audio ends immediately. Durable transcript/lifecycle cleanup remains
        // scoped to this exact session, even if a new account starts meanwhile.
        peer?.close(); peer = nil
        startup?.cancel(); startup = nil
        startupDeadline?.cancel(); startupDeadline = nil
        negotiation?.cancel(); negotiation = nil
        eventPreparation?.cancel(); eventPreparation = nil
        let oldAdmission = admission
        admission?.cancel(); admission = nil
        incoming?.cancel(); incoming = nil
        agentEvents?.cancel(); agentEvents = nil
        meter?.cancel(); meter = nil
        recovery?.cancel(); recovery = nil
        flushTask?.cancel(); flushTask = nil
        prefetchTask?.cancel(); prefetchTask = nil
        writer?.cancel(); writer = nil
        let oldRouting = routing
        let pendingDelegations = delegationQueue.compactMap { delegation in
            delegationOperations[delegation.id].map { (delegation.formattedInput, $0) }
        }
        // A received user request remains durable when the voice panel closes.
        // Let an admitted HTTP request settle; generation checks fence its UI.
        routing = nil; delegationQueue = []
        let oldTransport = transport, oldSessionID = sessionID
        let tail = protocolState?.takeTranscriptTail()
        transport = nil; protocolState = nil; sessionID = nil
        peerConnected = false; controlConnected = false; conversationReady = false; agentEventsReady = false; startupContextFrames = []; isReconnecting = false
        activeTurnID = nil; startedTurnID = nil; isWorking = false; isMuted = false
        inputLevel = 0; outputLevel = 0; audioBytesSent = 0; audioBytesReceived = 0
        transcripts = []; partialTranscriptIDs = [:]; errorMessage = nil
        conversationID = nil; conversationTitle = nil
        routePending = false; bufferedEvents = []; frameQueue = []; delegationOperations = [:]
        if phase != .idle { phase = .ended }
        if let oldTransport, let oldSessionID {
            cleanup = Task {
                // Serialize start/stop receipts, without waiting on microphone,
                // configuration or WebRTC callbacks from an abandoned startup.
                voiceTiming("cleanup.admission.wait")
                await oldAdmission?.value
                voiceTiming("cleanup.routing.wait")
                await oldRouting?.value
                voiceTiming("cleanup.transcript.begin")
                for (input, operation) in pendingDelegations {
                    _ = try? await oldTransport.delegate(sessionID: oldSessionID, operationID: operation, input: input)
                }
                if let tail { _ = try? await oldTransport.delegate(sessionID: oldSessionID, operationID: UUID().uuidString.lowercased(), input: tail) }
                voiceTiming("lifecycle.stop.begin")
                _ = try? await oldTransport.stop(sessionID: oldSessionID, operationID: UUID().uuidString.lowercased())
                voiceTiming("lifecycle.stop.end")
                await oldTransport.close()
                voiceTiming("cleanup.end")
            }
        }
    }

    /// App lifecycle owners can protect this finite network cleanup using their
    /// existing background lease. It never keeps the microphone running.
    public func finishStopping() async { await cleanup?.value }

    #if DEBUG
    /// Explicit demo-only UI fixtures use the real transcript reducer without
    /// opening a microphone, peer connection, or managed conversation.
    public func startTranscriptPreview(agentID: String, conversationTitle: String = "Voice preview", connecting: Bool = false) {
        stop()
        protocolState = try? ManagedVoiceProtocol()
        conversationID = agentID; self.conversationTitle = conversationTitle
        phase = connecting ? .connecting : .active
    }
    public func activateTranscriptPreview() {
        guard phase == .connecting, sessionID == nil, peer == nil else { return }
        phase = .active
    }
    public func receiveTranscriptPreview(_ event: JSON) {
        guard phase == .active, sessionID == nil, peer == nil,
              let update = protocolState?.realtimeMessage(event) else { return }
        apply(update.effects, token: generation)
    }

    func prepareRoutingForTesting(transport: ManagedVoiceTransport, agentID: String) {
        startTranscriptPreview(agentID: agentID)
        self.transport = transport; sessionID = ManagedVoiceProtocol.sessionID()
    }
    func receiveRealtimeForTesting(_ event: JSON) throws { try realtime(event, token: generation) }

    /// Hosted service evidence can negotiate real receive-only WebRTC without
    /// opening the user's microphone or changing its system authorization.
    func startReceivingForTesting(configuration: VoiceConfiguration) {
        begin(configuration: { configuration }, captureMicrophone: false)
    }
    func startPreparingForTesting(timeout: Duration, transport: ManagedVoiceTransport? = nil,
                                  configuration: @escaping @MainActor () async throws -> VoiceConfiguration) {
        begin(configuration: configuration, captureMicrophone: false, timeout: timeout, transportOverride: transport)
    }
    var hasNativePeerForTesting: Bool { peer != nil }
    private(set) var receivedRealtimeTypesForTesting: Set<String> = []
    func sendRealtimeForTesting(_ frame: JSON) throws {
        guard let peer else { throw VoiceFailure.connection }
        try peer.send(frame)
    }
    #endif

    private func interrupt() { if isEngaged { fail(VoiceFailure.interrupted) } }
    private func fail(_ error: Error) { stop(); phase = .failed; errorMessage = Self.safeError(error) }
    private static func safeError(_ error: Error) -> String {
        var text = error.localizedDescription
        for pattern in ["ncx_live_[A-Za-z0-9_-]+", "sk-[A-Za-z0-9_-]+", "Bearer [A-Za-z0-9._-]+"] {
            text = text.replacingOccurrences(of: pattern, with: "[redacted]", options: .regularExpression)
        }
        return String(text.prefix(300))
    }
}
