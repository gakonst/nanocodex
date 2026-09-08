import Foundation

public struct RemoteHand: Decodable, Identifiable, Sendable {
    public enum Transport: String, Decodable, Sendable { case frames = "frames-v1" }
    public let id: String
    public let name: String
    public let kind: RemoteSurface.Kind
    public let width: Int
    public let height: Int
    public let controllable: Bool
    public let machineID: String
    public let machineName: String
    public let generation: String
    public let transport: Transport?
    public var identity: String { machineID + ":" + id + ":" + generation }
    enum CodingKeys: String, CodingKey {
        case id, name, kind, width, height, controllable, generation, transport
        case machineID = "machine_id", machineName = "machine_name"
    }
}

final class RemoteNoRedirects: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
}

/// App-owned authentication stays in a private closure; remote metadata has no credentials.
public final class RemoteService: @unchecked Sendable {
    private let origin: URL
    private let authorize: @Sendable (inout URLRequest) -> Void
    private let session: URLSession

    public convenience init(origin: URL, authorize: @escaping @Sendable (inout URLRequest) -> Void) throws {
        try self.init(origin: origin, configuration: .ephemeral, authorize: authorize)
    }

    init(origin: URL, configuration: URLSessionConfiguration, authorize: @escaping @Sendable (inout URLRequest) -> Void) throws {
        guard let parts = URLComponents(url: origin, resolvingAgainstBaseURL: false), parts.host != nil,
              parts.user == nil, parts.password == nil, parts.query == nil, parts.fragment == nil,
              parts.path.isEmpty || parts.path == "/",
              parts.scheme == "https" || (parts.scheme == "http" && ["localhost", "127.0.0.1", "::1"].contains(parts.host))
        else { throw RemoteError.invalidMessage }
        self.origin = origin; self.authorize = authorize
        let config = configuration
        config.httpShouldSetCookies = false; config.urlCache = nil
        config.timeoutIntervalForRequest = 10
        session = URLSession(configuration: config, delegate: RemoteNoRedirects(), delegateQueue: nil)
    }

    public func close() { session.invalidateAndCancel() }

    public func list() async throws -> [RemoteHand] {
        struct Response: Decodable { let surfaces: [RemoteHand] }
        return try JSONDecoder().decode(Response.self, from: await request(path: "/screens", method: "GET")).surfaces
    }

    public func ice() async throws -> [RemoteICE] {
        struct Response: Decodable { let iceServers: [RemoteICE] }
        return try JSONDecoder().decode(Response.self, from: await request(path: "/ice", method: "POST")).iceServers
    }

    fileprivate func renew(_ id: String) async throws {
        _ = try await request(path: "/renew", method: "POST", body: JSONEncoder().encode(["connection_id": id]))
    }

    private func request(path: String, method: String, body: Data? = nil) async throws -> Data {
        var request = try makeRequest(path: path)
        request.httpMethod = method; request.httpBody = body
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse else { throw RemoteError.unavailable }
        guard (200..<300).contains(response.statusCode) else {
            throw [401, 403].contains(response.statusCode) ? RemoteError.unauthorized : RemoteError.unavailable
        }
        guard data.count <= 131_072 else { throw RemoteError.invalidMessage }
        return data
    }

    private func makeRequest(path: String) throws -> URLRequest {
        guard let url = URL(string: "/v1/account/hands" + path, relativeTo: origin)?.absoluteURL else { throw RemoteError.invalidMessage }
        var request = URLRequest(url: url, timeoutInterval: 10)
        authorize(&request)
        return request
    }

    fileprivate func socket(hand: RemoteHand?) throws -> URLSessionWebSocketTask {
        var path = "/host"
        if let hand {
            var components = URLComponents()
            components.queryItems = [URLQueryItem(name: "machine_id", value: hand.machineID),
                URLQueryItem(name: "surface_id", value: hand.id), URLQueryItem(name: "generation", value: hand.generation)]
            path = "/view?" + components.percentEncodedQuery!
        }
        var request = try makeRequest(path: path)
        var url = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!
        url.scheme = url.scheme == "https" ? "wss" : "ws"; request.url = url.url
        let socket = session.webSocketTask(with: request)
        socket.maximumMessageSize = hand?.transport == .frames ? 750_000 : 70_000
        return socket
    }
}

public struct RemoteMessage: Codable, Sendable {
    public let type: String
    public var connectionID: String?
    public var generation: String?
    public var viewerID: String?
    public var surfaceID: String?
    public var machineID: String?
    public var machineName: String?
    public var surfaces: [RemoteSurface]?
    public var signal: RemoteSignal?
    var requestID: String?, agentID: String?, deadlineAt: Double?, input: RemoteAgentInput?
    var agentStatus: String?, jpeg: String?, width: Int?, height: Int?
    var data: RemoteRelayData?
    public init(type: String, viewerID: String? = nil, signal: RemoteSignal? = nil,
                machineID: String? = nil, machineName: String? = nil, surfaces: [RemoteSurface]? = nil) {
        self.type = type; self.viewerID = viewerID; self.signal = signal
        self.machineID = machineID; self.machineName = machineName; self.surfaces = surfaces
    }
    enum CodingKeys: String, CodingKey {
        case type, generation, surfaces, signal, data
        case connectionID = "connection_id", viewerID = "viewer_id", surfaceID = "surface_id"
        case machineID = "machine_id", machineName = "machine_name"
        case requestID = "request_id", agentID = "agent_id", deadlineAt = "deadline_at", input
        case agentStatus = "status", jpeg, width, height
    }
}

@MainActor protocol RemoteSignalingTransport: AnyObject {
    var onMessage: (RemoteMessage) -> Void { get set }
    var onClose: (Error?) -> Void { get set }
    func connect(hand: RemoteHand?) throws
    func send(_ message: RemoteMessage)
    func close(error: Error?)
}

@MainActor
public final class RemoteSignaling: RemoteSignalingTransport {
    public var onMessage: (RemoteMessage) -> Void = { _ in }
    public var onClose: (Error?) -> Void = { _ in }
    private let service: RemoteService
    private var socket: URLSessionWebSocketTask?
    private var reader: Task<Void, Never>?
    private var renewal: Task<Void, Never>?
    private var watchdog: Task<Void, Never>?
    private var sender: Task<Void, Never>?
    private var queuedMessages = 0
    private var closed = false
    private var publishing = false

    public init(service: RemoteService) { self.service = service }

    public func connect(hand: RemoteHand? = nil) throws {
        guard socket == nil, !closed else { throw RemoteError.closed }
        let connection = try service.socket(hand: hand)
        publishing = hand == nil
        socket = connection; connection.resume()
        resetWatchdog()
        reader = Task { [weak self] in
            guard let self else { return }
            do {
                while !Task.isCancelled && !closed {
                    let wire = try await connection.receive()
                    guard case .string(let value) = wire, value.utf8.count <= (hand?.transport == .frames ? 750_000 : 70_000) else { throw RemoteError.invalidMessage }
                    let message = try JSONDecoder().decode(RemoteMessage.self, from: Data(value.utf8))
                    if message.type == "ready" {
                        guard let id = message.connectionID, id.count <= 128, renewal == nil else { throw RemoteError.invalidMessage }
                        resetWatchdog(); startRenewal(id)
                    } else if message.type == "renewed" { resetWatchdog() }
                    onMessage(message)
                }
            } catch { if !closed { close(error: error) } }
        }
    }

    public func send(_ message: RemoteMessage) {
        guard let socket, !closed else { return }
        guard queuedMessages < 128 else { close(error: RemoteError.unavailable); return }
        queuedMessages += 1
        let preceding = sender
        sender = Task { [weak self] in
            await preceding?.value
            guard let self else { return }
            defer { queuedMessages -= 1 }
            guard !closed, !Task.isCancelled else { return }
            do {
                let data = try JSONEncoder().encode(message)
                guard data.count <= (message.type == "agent_result" ? 750_000 : 70_000) else { throw RemoteError.invalidMessage }
                try await socket.send(.string(String(decoding: data, as: UTF8.self)))
            } catch { close(error: error) }
        }
    }

    public func close(error: Error? = nil) {
        guard !closed else { return }; closed = true
        let failure = error.map {
            Self.disconnectError($0, publishing: publishing,
                                 code: socket?.closeCode.rawValue ?? 0, reason: socket?.closeReason)
        }
        reader?.cancel(); renewal?.cancel(); watchdog?.cancel(); sender?.cancel()
        socket?.cancel(with: .goingAway, reason: nil); socket = nil
        onClose(failure)
    }

    /// A replaced publisher must retire instead of repeatedly evicting its
    /// replacement. Viewers receive the same close and should reconnect to it.
    static func disconnectError(_ error: Error, publishing: Bool, code: Int, reason: Data?) -> Error {
        if publishing, code == URLSessionWebSocketTask.CloseCode.policyViolation.rawValue,
           reason == Data("Host replaced".utf8) { return RemoteError.hostReplaced }
        return error
    }

    private func startRenewal(_ id: String) {
        renewal = Task { [weak self] in
            guard let self else { return }
            do {
                while !Task.isCancelled && !closed {
                    try await Task.sleep(for: .seconds(10))
                    try await service.renew(id)
                    send(.init(type: "ping"))
                }
            } catch { if !closed { close(error: error) } }
        }
    }

    private func resetWatchdog() {
        watchdog?.cancel()
        watchdog = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(25)) }
            catch { return }
            // Missing renewal can be a network stall. Only an explicit HTTP
            // authorization rejection should disable automatic recovery.
            self?.close(error: RemoteError.unavailable)
        }
    }
}
