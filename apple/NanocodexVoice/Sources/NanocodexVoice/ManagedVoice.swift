import Foundation
import InboxCore

public struct ManagedVoiceCall: Equatable, Sendable {
    public let sdp: String
    public let callID: String
}
public struct ManagedVoiceRoute: Equatable, Sendable {
    public let turnID: String
    public let route: String
}

/// Account-authenticated media and durable voice lifecycle. Provider credentials
/// remain at the managed service. Call stop before close to end the durable mode.
public actor ManagedVoiceTransport {
    // The managed lifecycle belongs to an agent at one service, regardless of
    // which account credential is currently authorized to access it.
    nonisolated let conversationIdentity: String
    private let credential: AccountCredential
    private let agentID: String
    private let http: HTTPTransport
    private let client: ManagedClient
    private var closed = false
    private var socket: URLSessionWebSocketTask?
    private var reader: Task<Void, Never>?
    private var sidebandID = UUID()
    private var continuation: AsyncThrowingStream<JSON, Error>.Continuation?
    private var agentReader: Task<Void, Never>?
    private var agentContinuation: AsyncThrowingStream<AgentEvent, Error>.Continuation?
    private var agentCursor = Cursor.zero
    private var agentStreamID = UUID()
    private var agentBufferedSizes: [Int] = []

    public init(credential: AccountCredential, agentID: String, configuration: URLSessionConfiguration? = nil) throws {
        guard agentID.range(of: "^[0-9a-f]{8}-[0-9a-f]{4}-[78][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$", options: .regularExpression) != nil else {
            throw ManagedError(code: "invalid_agent", message: "Voice needs a durable Nanocodex conversation.")
        }
        self.credential = credential; self.agentID = agentID
        http = try HTTPTransport(origin: URL(string: credential.origin)!, configuration: configuration)
        conversationIdentity = http.origin.absoluteString + "/" + agentID
        client = ManagedClient(credential: credential, configuration: configuration)
    }

    public func start(sessionID: String, operationID: String) async throws -> JSON {
        try await lifecycle("start", sessionID: sessionID, operationID: operationID)["context"]
    }
    public func stop(sessionID: String, operationID: String) async throws -> JSON {
        try await lifecycle("stop", sessionID: sessionID, operationID: operationID)["context"]
    }
    public func delegate(sessionID: String, operationID: String, input: String) async throws -> ManagedVoiceRoute {
        guard !input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, input.utf8.count <= 32_768 else { throw ManagedError.invalidResponse }
        let reply = try await lifecycle("delegate", sessionID: sessionID, operationID: operationID, input: input)
        guard ["started", "steered"].contains(reply["route"].string) else { throw ManagedError.invalidResponse }
        return ManagedVoiceRoute(turnID: try validatedTurnID(reply["turn_id"].string), route: reply["route"].string)
    }
    public func cancel(turnID: String) async throws {
        try checkOpen(); try await client.command(AgentCommand(agentID: agentID, turnID: turnID, kind: .stop)); try checkOpen()
    }

    public func prefetch(sessionID: String, query: String) async throws {
        try checkOpen(); try Self.validateSession(sessionID)
        guard !query.isEmpty, query.utf8.count <= 512 else { throw ManagedError.invalidResponse }
        let body = try JSONEncoder().encode(["voice_session_id": sessionID, "query": query])
        var request = try http.request(path("prefetch"), method: "POST", body: body, key: credential.apiKey)
        request.timeoutInterval = 10
        _ = try await http.data(request)
        try checkOpen()
    }

    public func call(sdp: String, instructions: String, voice: String = "cove", sessionID: String, settings: VoiceSettings? = nil) async throws -> ManagedVoiceCall {
        try checkOpen(); try Self.validateSession(sessionID)
        guard !sdp.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, sdp.utf8.count <= 32_768,
              !instructions.isEmpty, instructions.utf8.count <= 32_768, ManagedVoiceProtocol.voices.contains(voice) else { throw ManagedError.invalidResponse }
        let session = try ManagedVoiceProtocol.session(instructions: instructions, settings: settings ?? VoiceSettings(voice: voice))
        let body: JSON = .object(["sdp": .string(sdp), "session": session])
        let encoded = try JSONEncoder().encode(body)
        guard encoded.count <= 65_536 else { throw ManagedError.invalidResponse }
        var request = try http.request(path("calls"), method: "POST", body: encoded, key: credential.apiKey)
        request.setValue(sessionID, forHTTPHeaderField: "x-nanocodex-voice-session-id")
        let (data, response) = try await http.data(request)
        try checkOpen()
        guard data.count <= 65_536, let answer = String(data: data, encoding: .utf8), !answer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              let location = response.value(forHTTPHeaderField: "x-nanocodex-realtime-location"),
              let callID = location.split(separator: "?", maxSplits: 1).first?.split(separator: "/").reversed().map(String.init).first(where: Self.validCallID) else {
            throw ManagedError.invalidResponse
        }
        return ManagedVoiceCall(sdp: answer, callID: callID)
    }

    /// Reconnection is coordinated by the voice session so queued context frames
    /// are replayed only after a replacement sideband is ready.
    public func sideband(callID: String, sessionID: String) async throws -> AsyncThrowingStream<JSON, Error> {
        try checkOpen(); try Self.validateSession(sessionID)
        guard Self.validCallID(callID) else { throw ManagedError.invalidResponse }
        endSideband()
        var components = URLComponents(url: http.origin.appendingPathComponent(path("sideband")), resolvingAgainstBaseURL: false)!
        components.scheme = http.origin.scheme == "https" ? "wss" : "ws"
        components.queryItems = [URLQueryItem(name: "call_id", value: callID), URLQueryItem(name: "voice_session_id", value: sessionID)]
        var request = URLRequest(url: components.url!); request.timeoutInterval = 20
        request.setValue("Bearer \(credential.apiKey)", forHTTPHeaderField: "Authorization")
        let connection = http.session.webSocketTask(with: request)
        connection.maximumMessageSize = 256 * 1024
        socket = connection; let epoch = sidebandID
        let stream = AsyncThrowingStream<JSON, Error>(bufferingPolicy: .bufferingOldest(128)) { continuation in
            self.continuation = continuation
            self.reader = Task { await self.readSideband(connection, epoch: epoch, continuation: continuation) }
            continuation.onTermination = { [weak self] _ in Task { await self?.endSideband(epoch: epoch) } }
        }
        connection.resume()
        do {
            try await withTaskCancellationHandler {
                try await withThrowingTaskGroup(of: Void.self) { group in
                    group.addTask {
                        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                            connection.sendPing { error in
                                if let error { continuation.resume(throwing: error) }
                                else { continuation.resume() }
                            }
                        }
                    }
                    group.addTask { try await Task.sleep(for: .seconds(20)); connection.cancel(with: .goingAway, reason: nil); throw URLError(.timedOut) }
                    defer { group.cancelAll() }
                    try await group.next()
                }
            } onCancel: { connection.cancel(with: .goingAway, reason: nil) }
            try checkOpen()
            guard epoch == sidebandID else { throw ManagedError.cancelled }
        } catch {
            endSideband(epoch: epoch)
            if let response = connection.response as? HTTPURLResponse, response.statusCode != 101 {
                throw ManagedError(code: "voice_connection", message: "Voice could not connect. Please reconnect.", status: response.statusCode)
            }
            throw safe(error)
        }
        return stream
    }

    public func send(_ frame: JSON) async throws {
        try checkOpen()
        guard let socket else { throw ManagedError(code: "voice_disconnected", message: "Voice is reconnecting.") }
        let data = try JSONEncoder().encode(frame)
        guard data.count <= 65_536, let text = String(data: data, encoding: .utf8) else { throw ManagedError.invalidResponse }
        let epoch = sidebandID
        do { try await socket.send(.string(text)); try checkOpen(); guard epoch == sidebandID else { throw ManagedError.cancelled } }
        catch { throw safe(error) }
    }

    public func events(after cursor: String = "latest") async throws -> AsyncThrowingStream<AgentEvent, Error> {
        try checkOpen()
        let after = cursor == "latest" ? try await client.state(agentID)["latest_event_cursor"].string : cursor
        try checkOpen()
        guard let position = Cursor(rawValue: after) else { throw ManagedError.invalidResponse }
        endAgentEvents()
        agentCursor = position
        let epoch = agentStreamID
        return AsyncThrowingStream(bufferingPolicy: .bufferingOldest(256)) { continuation in
            agentContinuation = continuation
            agentReader = Task { await self.readAgentEvents(epoch: epoch) }
            continuation.onTermination = { [weak self] _ in Task { await self?.endAgentEvents(epoch: epoch) } }
        }
    }
    private func readAgentEvents(epoch: UUID) async {
        var delay = 200
        while !closed, epoch == agentStreamID, !Task.isCancelled {
            do {
                try await client.stream(agentID, after: agentCursor) { [weak self] frame in
                    await self?.receiveAgentEvent(frame, epoch: epoch)
                }
                delay = 200
            } catch is CancellationError { return }
            catch let error as APIError where error == .http(401) || error == .http(403) || error == .http(404) || error == .agentDeleting || error == .invalidResponse {
                failAgentEvents(error, epoch: epoch); return
            } catch is DecodingError {
                failAgentEvents(ManagedError.invalidResponse, epoch: epoch); return
            } catch { /* Resume from the last received cursor after transport failures. */ }
            do { try await Task.sleep(for: .milliseconds(delay)) } catch { return }
            delay = min(delay * 2, 5_000)
        }
    }
    private func receiveAgentEvent(_ frame: SSEFrame, epoch: UUID) {
        guard !closed, epoch == agentStreamID else { return }
        if let event = frame.event, event.cursor > agentCursor {
            if event.type == "stream_failed" {
                failAgentEvents(ManagedError(code: "stream_failed", message: "This thread’s event stream stopped. Reconnect voice to continue."), epoch: epoch)
                return
            }
            guard let continuation = agentContinuation else { return }
            switch continuation.yield(event) {
            case .enqueued(let remaining):
                agentBufferedSizes.append((try? JSONEncoder().encode(event.data).count) ?? 0)
                let buffered = 256 - remaining
                if agentBufferedSizes.count > buffered { agentBufferedSizes.removeFirst(agentBufferedSizes.count - buffered) }
                if agentBufferedSizes.reduce(0, +) > 32 * 1024 * 1024 {
                    failAgentEvents(ManagedError(code: "voice_overflow", message: "Voice fell behind. Reconnect to continue."), epoch: epoch)
                    return
                }
            case .dropped:
                failAgentEvents(ManagedError(code: "voice_overflow", message: "Voice fell behind. Reconnect to continue."), epoch: epoch)
                return
            case .terminated: endAgentEvents(epoch: epoch); return
            @unknown default: endAgentEvents(epoch: epoch); return
            }
        }
        if let cursor = frame.cursor { agentCursor = max(agentCursor, cursor) }
    }
    private func failAgentEvents(_ error: Error, epoch: UUID) {
        guard !closed, epoch == agentStreamID else { return }
        agentContinuation?.finish(throwing: error)
        endAgentEvents(epoch: epoch)
    }
    private func endAgentEvents(epoch: UUID? = nil) {
        guard epoch == nil || epoch == agentStreamID else { return }
        agentStreamID = UUID(); agentReader?.cancel(); agentReader = nil
        agentContinuation?.finish(); agentContinuation = nil
        agentBufferedSizes = []
    }
    public func close() async {
        guard !closed else { return }; closed = true
        endSideband(); endAgentEvents(); http.close(); client.close()
    }

    private func lifecycle(_ action: String, sessionID: String, operationID: String, input: String? = nil) async throws -> JSON {
        try checkOpen(); try Self.validateSession(sessionID); _ = try validatedTurnID(operationID)
        var operationID = operationID
        var body: [String: JSON] = ["voice_session_id": .string(sessionID), "operation_id": .string(operationID)]
        if let input { body["input"] = .string(input) }
        var request = try http.request(path(action), method: "POST", body: JSONEncoder().encode(JSON.object(body)), key: credential.apiKey)
        // Operation identities make lifecycle retries durable and idempotent.
        var reply: JSON = .null
        for attempt in 0..<3 {
            try checkOpen()
            do { reply = try await http.json(request); break }
            catch {
                try checkOpen()
                let failure = error as? ManagedError
                let startupTimedOut = action == "start" && failure?.status == 500
                    && failure?.code == "realtime_start_failed"
                    && failure?.message == "Cloudflare Agent EGRESS startup validation timed out"
                if startupTimedOut {
                    guard attempt < 2 else {
                        throw ManagedError(code: "voice_startup_timeout", message: "The agent is taking too long to connect. Try voice again in a moment.")
                    }
                    // This exact error occurs before realtime.start is invoked.
                    // The service permanently blocks its failed operation receipt,
                    // so retry admission with a fresh operation on the same call.
                    // Never rotate identities for ambiguous or transport failures.
                    operationID = UUID().uuidString.lowercased()
                    body["operation_id"] = .string(operationID)
                    request = try http.request(path(action), method: "POST", body: JSONEncoder().encode(JSON.object(body)), key: credential.apiKey)
                } else {
                    guard attempt < 2, failure?.code == "network_error" || [408, 429, 502, 503, 504].contains(failure?.status ?? 0) else { throw safe(error) }
                }
                try await Task.sleep(for: .milliseconds(250 * (1 << attempt)))
            }
        }
        try checkOpen()
        guard reply["voice_session_id"].string == sessionID, reply["operation_id"].string == operationID else { throw ManagedError.invalidResponse }
        return reply
    }
    private func readSideband(_ socket: URLSessionWebSocketTask, epoch: UUID, continuation: AsyncThrowingStream<JSON, Error>.Continuation) async {
        do {
            while !closed, epoch == sidebandID, !Task.isCancelled {
                let message = try await socket.receive()
                guard !closed, epoch == sidebandID, !Task.isCancelled else { break }
                let data: Data
                switch message { case .string(let text): data = Data(text.utf8); case .data(let bytes): data = bytes; @unknown default: throw ManagedError.invalidResponse }
                guard data.count <= 256 * 1024 else { throw ManagedError.invalidResponse }
                let value = try JSONDecoder().decode(JSON.self, from: data)
                guard case .object = value else { throw ManagedError.invalidResponse }
                if case .dropped = continuation.yield(value) { throw ManagedError(code: "voice_overflow", message: "Voice fell behind. Reconnect to continue.") }
            }
            continuation.finish()
        } catch {
            if !closed, epoch == sidebandID, !Task.isCancelled { continuation.finish(throwing: safe(error)) }
            else { continuation.finish() }
        }
        endSideband(epoch: epoch)
    }
    private func endSideband(epoch: UUID? = nil) {
        guard epoch == nil || epoch == sidebandID else { return }
        sidebandID = UUID(); reader?.cancel(); reader = nil
        socket?.cancel(with: .normalClosure, reason: nil); socket = nil
        continuation?.finish(); continuation = nil
    }
    private func path(_ operation: String) -> String { "/v1/agents/\(agentID)/realtime/\(operation)" }
    private func checkOpen() throws { try Task.checkCancellation(); if closed { throw ManagedError.cancelled } }
    private func safe(_ error: Error) -> Error {
        if error is CancellationError { return error }
        if var value = error as? ManagedError {
            value.message = value.message.replacingOccurrences(of: credential.apiKey, with: "[redacted]")
            return value
        }
        return ManagedError(code: "voice_connection", message: "Voice disconnected. Reconnect to continue.")
    }
    static func validateSession(_ id: String) throws {
        guard id.range(of: "^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$", options: .regularExpression) != nil else { throw ManagedError.invalidResponse }
    }
    static func validCallID(_ id: String) -> Bool {
        id.range(of: "^(rtc_[A-Za-z0-9._:-]{1,196}|[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$", options: .regularExpression) != nil
    }
}
