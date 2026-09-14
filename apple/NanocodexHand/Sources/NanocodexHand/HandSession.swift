import Foundation
import InboxCore

/// Native implementation of the existing account Hosted Tools contract.
/// A session belongs to exactly one account and never follows HTTP redirects.
@MainActor
public final class HandSession {
    public private(set) var connected = false
    public private(set) var lastError: String?
    public var onConnectionChange: ((Bool) -> Void)?
    private let request: URLRequest
    private let workspace: HandWorkspace
    private let session: URLSession
    private var loop: Task<Void, Never>?
    private var socket: URLSessionWebSocketTask?
    private var generation = UUID()
    private var closed = false
    private var receipts: [String: (identity: Data, result: JSON)] = [:]
    private var calls: [String: (identity: Data, task: Task<JSON, Error>)] = [:]
    private final class SocketState { var ready = false; var nonce: String? }

    public init(credential: AccountCredential, workspace: HandWorkspace) throws {
        self.workspace = workspace
        let client = ManagedClient(credential: credential)
        defer { client.close() }
        var request = try client.request(path: "/v1/account/tool-host")
        var components = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!
        components.scheme = components.scheme == "https" ? "wss" : "ws"
        request.url = components.url; request.timeoutInterval = 15
        self.request = request
        let config = URLSessionConfiguration.ephemeral
        config.httpShouldSetCookies = false; config.httpCookieStorage = nil; config.urlCache = nil
        session = URLSession(configuration: config, delegate: HandNoRedirects(), delegateQueue: nil)
    }

    public func start() {
        guard loop == nil, !closed else { return }
        let epoch = generation
        loop = Task { [weak self] in
            var delay = 1
            while !Task.isCancelled {
                guard let self, self.generation == epoch else { return }
                let socket = self.session.webSocketTask(with: self.request)
                // Cloudflare Durable Objects accept WebSocket messages up to 32 MiB.
                socket.maximumMessageSize = 32 * 1024 * 1024
                self.socket = socket; socket.resume()
                do { try await self.run(socket, epoch: epoch); delay = 1 }
                catch {
                    guard self.generation == epoch else { return }
                    if connected { delay = 1 }
                    lastError = "\((error as NSError).domain) \((error as NSError).code); socket \(socket.closeCode.rawValue)"
                }
                guard self.generation == epoch else { return }
                socket.cancel(with: .goingAway, reason: nil); self.setConnected(false)
                do { try await Task.sleep(for: .seconds(delay)) } catch { return }
                delay = min(30, delay * 2)
            }
        }
    }

    public func stop() {
        generation = UUID(); loop?.cancel(); loop = nil
        socket?.cancel(with: .goingAway, reason: nil); socket = nil
        for call in calls.values { call.task.cancel() }
        setConnected(false)
    }
    public func close() { closed = true; stop(); session.invalidateAndCancel(); receipts = [:] }

    private func setConnected(_ value: Bool) {
        guard connected != value else { return }
        connected = value; onConnectionChange?(value)
    }
    private func send(_ frame: JSON, _ socket: URLSessionWebSocketTask) async throws {
        let data = try JSONEncoder().encode(frame)
        guard data.count <= 32 * 1024 * 1024 else { throw HandFailure.protocolViolation }
        try await socket.send(.string(String(decoding: data, as: UTF8.self)))
    }
    private func run(_ socket: URLSessionWebSocketTask, epoch: UUID) async throws {
        let state = SocketState()
        let watchdog = Task {
            do {
                try await Task.sleep(for: .seconds(12))
                guard state.ready else { socket.cancel(with: .goingAway, reason: nil); return }
                while !Task.isCancelled {
                    guard state.nonce == nil else { socket.cancel(with: .goingAway, reason: nil); return }
                    state.nonce = UUID().uuidString
                    try await send(.object(["type": .string("ping"), "nonce": .string(state.nonce!)]), socket)
                    try await Task.sleep(for: .seconds(20))
                }
            } catch { if !Task.isCancelled { socket.cancel(with: .goingAway, reason: nil) } }
        }
        defer { watchdog.cancel() }
        try await send(workspace.catalog, socket)
        while !Task.isCancelled, generation == epoch {
            let message = try await socket.receive()
            let data: Data
            switch message { case .data(let value): data = value; case .string(let value): data = Data(value.utf8); @unknown default: throw HandFailure.protocolViolation }
            guard data.count <= 32 * 1024 * 1024 else { throw HandFailure.protocolViolation }
            let frame = try JSONDecoder().decode(JSON.self, from: data)
            guard generation == epoch, !Task.isCancelled else { return }
            switch frame["type"].string {
            case "ready":
                guard !state.ready else { throw HandFailure.protocolViolation }
                state.ready = true; lastError = nil; setConnected(true)
            case "pong":
                guard state.ready, let expected = state.nonce, frame["nonce"].string == expected else { throw HandFailure.protocolViolation }
                state.nonce = nil
            case "ack": receipts.removeValue(forKey: frame["call_id"].string)
            case "cancel":
                // Workspace operations are short and serialized. A completed
                // call retains its original result until the broker ACKs it.
                if let receipt = receipts[frame["call_id"].string] { try await send(receipt.result, socket) }
            case "call":
                guard state.ready else { throw HandFailure.protocolViolation }
                let result = try await invoke(frame)
                guard generation == epoch, !Task.isCancelled else { return }
                try await send(result, socket)
            default: throw HandFailure.protocolViolation
            }
        }
    }

    func invoke(_ frame: JSON) async throws -> JSON {
        guard !closed else { throw CancellationError() }
        let id = frame["call_id"].string, budget = frame["output_byte_budget"].number
        guard !id.isEmpty, id.utf8.count <= 128, !frame["session_id"].string.isEmpty,
              budget.isFinite, budget >= 0, frame["deadline_at"].number > 0 else { throw HandFailure.protocolViolation }
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys]
        let identity = try encoder.encode(frame)
        if let retained = receipts[id] {
            guard retained.identity == identity else { throw HandFailure.protocolViolation }
            return retained.result
        }
        if let active = calls[id] {
            guard active.identity == identity else { throw HandFailure.protocolViolation }
            return try await active.task.value
        }
        let task = Task { try await self.execute(frame, id: id, budget: budget) }
        calls[id] = (identity, task)
        defer { calls.removeValue(forKey: id) }
        let result = try await task.value
        // Completed receipts survive a suspension/reconnect, even if stopping
        // raced with completion. Closing an account discards them permanently.
        guard !closed else { throw CancellationError() }
        receipts[id] = (identity, result)
        return result
    }
    private func execute(_ frame: JSON, id: String, budget: Double) async throws -> JSON {
        try Task.checkCancellation()
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys]
        let outcome: JSON
        if frame["deadline_at"].number <= Date().timeIntervalSince1970 * 1000 {
            outcome = .object(["status": .string("unavailable"), "message": .string("The call expired before dispatch.")])
        } else {
            let value: JSON, success: Bool
            do { value = try await workspace.call(name: frame["name"].string, input: frame["input"]); success = true }
            catch {
                value = .object(["error": .string((error as? HandFailure)?.localizedDescription ?? "The device could not access this workspace file.")]); success = false
            }
            let data = try encoder.encode(value)
            let output: JSON = .object([
                "output": .string(String(decoding: data, as: UTF8.self)), "success": .bool(success),
                "structured_result": value, "metadata": .null, "process_trace": .null
            ])
            let outputBytes = try encoder.encode(output).count
            if frame["deadline_at"].number <= Date().timeIntervalSince1970 * 1000 || Double(outputBytes) > budget {
                outcome = .object(["status": .string("ambiguous"), "message": .string("The device operation ran, but its result exceeded the deadline or output budget.")])
            } else {
                outcome = .object(["status": .string("completed"), "output": output])
            }
        }
        let result = JSON.object(["type": .string("result"), "call_id": .string(id), "outcome": outcome])
        return result
    }
}

private final class HandNoRedirects: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
}
