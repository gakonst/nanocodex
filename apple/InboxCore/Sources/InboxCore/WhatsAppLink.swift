import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

/// Safe transcript hint only. Private pairing responses must never be serialized here.
public struct WhatsAppLink: Codable, Equatable, Sendable {
    public let agentID: String
    public let operationID: String
    public let expiresAt: Double
    public let phase: String

    public static func parse(_ value: JSON, depth: Int = 0) -> WhatsAppLink? {
        guard depth < 12 else { return nil }
        let value = ToolPresentation.decoded(value)
        if value["type"].string == "whatsapp_link" {
            guard case .object(let fields) = value,
                  Set(fields.keys).isSubset(of: ["ok", "type", "status", "connector", "agent_id", "operation_id", "expires_at", "phase", "message"]),
                  value["ok"] == .bool(true), value["status"].string == "input_required",
                  value["connector"].string == "whatsapp",
                  UUID(uuidString: value["operation_id"].string) != nil,
                  (try? ManagedClient.agentPath(value["agent_id"].string)) != nil,
                  case .number(let expiry) = value["expires_at"], expiry.isFinite, expiry > 0,
                  expiry <= Date().timeIntervalSince1970 * 1000 + 305_000,
                  ["requested", "ready", "unknown", "expired", "paired"].contains(value["phase"].string)
            else { return nil }
            return WhatsAppLink(agentID: value["agent_id"].string, operationID: value["operation_id"].string, expiresAt: expiry, phase: value["phase"].string)
        }
        switch value {
        case .array(let values): return values.lazy.compactMap { parse($0, depth: depth + 1) }.first
        case .object(let fields):
            for key in ["content", "text", "structuredContent", "result", "output"] {
                if let child = fields[key], let hint = parse(child, depth: depth + 1) { return hint }
            }
            return nil
        default: return nil
        }
    }
}

/// Memory-only private response. Deliberately has no Codable or debug description conformance.
public struct WhatsAppPrivateCode: Sendable {
    public let value: String
}

public struct WhatsAppLinkStatus: Equatable, Sendable {
    public let phase: String
    public let connected: Bool
    public let expiresAt: Double
}

extension ManagedClient {
    public func whatsAppLinkStatus(_ link: WhatsAppLink, configuration: URLSessionConfiguration = .ephemeral) async throws -> WhatsAppLinkStatus {
        let response = try await vaultIntakeJSON(path: "/v1/connectors/whatsapp", configuration: configuration)
        let attempt = response["attempt"]
        guard attempt["operation_id"].string == link.operationID,
              case .number(let expiry) = attempt["expires_at"], expiry.isFinite, expiry > 0,
              expiry <= Date().timeIntervalSince1970 * 1000 + 305_000,
              link.phase == "unknown" || expiry == link.expiresAt,
              case .bool(let connected) = response["connected"],
              ["requested", "ready", "unknown", "expired", "paired"].contains(attempt["state"].string),
              !connected || attempt["state"].string == "paired" else { throw APIError.invalidResponse }
        return WhatsAppLinkStatus(phase: attempt["state"].string, connected: connected, expiresAt: expiry)
    }

    public func whatsAppPrivateCode(_ link: WhatsAppLink, expiresAt: Double, configuration: URLSessionConfiguration = .ephemeral) async throws -> WhatsAppPrivateCode {
        let response = try await vaultIntakeJSON(path: "/v1/connectors/whatsapp/pairing?operation_id=" + link.operationID,
                                                configuration: configuration, maximumResponseBytes: 4096)
        guard case .object(let fields) = response,
              Set(fields.keys).isSubset(of: ["operation_id", "expires_at", "code", "pairing_code"]),
              response["operation_id"].string == link.operationID,
              response["expires_at"] == .number(expiresAt),
              case .string(let code) = response["code"],
              code.range(of: #"^[A-Za-z0-9-]{4,32}$"#, options: .regularExpression) != nil,
              fields["pairing_code"] == nil || fields["pairing_code"] == .string(code)
        else { throw APIError.invalidResponse }
        return WhatsAppPrivateCode(value: code)
    }
}

/// The inline native card owns this controller; nothing in it enters transcript/state caches.
/// Generation checks reject late HTTP replies after suspension or account changes.
@MainActor public final class WhatsAppLinkController {
    public enum Phase: Equatable { case waiting, ready, connected, expired, unknown, retrying, unavailable, cancelled }
    public let link: WhatsAppLink
    public let account: UUID
    public private(set) var phase: Phase = .waiting
    public private(set) var code: String?
    public private(set) var active = false
    private var generation = UUID()
    private var refreshing = false
    private var authoritativeExpiry: Double?
    private var reconciliationPending = false
    public var expiresAt: Double { authoritativeExpiry ?? link.expiresAt }
    public var shouldPoll: Bool { active && (reconciliationPending || phase == .waiting || phase == .ready || phase == .unknown || phase == .retrying) }

    public init(link: WhatsAppLink, account: UUID) { self.link = link; self.account = account }

    public func activate(account: UUID, now: Date = Date()) {
        guard account == self.account else { cancel(); return }
        guard phase != .cancelled && phase != .connected else { return }
        guard expiresAt <= now.timeIntervalSince1970 * 1000 + 305_000 else { finish(.unavailable); return }
        active = true
        reconciliationPending = true
        expire(now: now)
    }
    public func suspend() {
        active = false; code = nil; generation = UUID(); refreshing = false; reconciliationPending = false
    }
    public func cancel() { finish(.cancelled) }
    public func expire(now: Date = Date()) {
        if (phase == .waiting || phase == .ready || phase == .unknown || phase == .retrying) && now.timeIntervalSince1970 * 1000 >= expiresAt {
            code = nil
            if reconciliationPending { phase = .expired } else { finish(.expired) }
        }
    }
    private func finish(_ phase: Phase) {
        self.phase = phase; suspend()
    }
    public func remainingSeconds(now: Date = Date()) -> Int {
        Int(min(300, max(0, ceil((expiresAt - now.timeIntervalSince1970 * 1000) / 1000))))
    }
    public var safeReceipt: JSON? {
        guard phase == .connected else { return nil }
        return .object(["type": .string("whatsapp_link_receipt"), "status": .string("connected"),
                        "connector": .string("whatsapp"), "operation_id": .string(link.operationID)])
    }
    private static func isTemporaryTransportFailure(_ error: Error) -> Bool {
        if let apiError = error as? APIError, case .http(let status) = apiError {
            return status == 408 || status == 429 || (500...599).contains(status)
        }
        guard let error = error as? URLError else { return false }
        return [.timedOut, .cannotFindHost, .cannotConnectToHost, .networkConnectionLost,
                .dnsLookupFailed, .notConnectedToInternet, .internationalRoamingOff,
                .callIsActive, .dataNotAllowed].contains(error.code)
    }
    public func refresh(client: ManagedClient, account: UUID, configuration: URLSessionConfiguration = .ephemeral) async {
        guard account == self.account else { cancel(); return }
        expire()
        guard shouldPoll, !refreshing else { return }
        let ticket = generation
        refreshing = true
        defer { if ticket == generation { refreshing = false } }
        do {
            let status = try await client.whatsAppLinkStatus(link, configuration: configuration)
            guard ticket == generation, !Task.isCancelled, shouldPoll else { return }
            if let prior = authoritativeExpiry, prior != status.expiresAt { finish(.unavailable); return }
            authoritativeExpiry = status.expiresAt
            reconciliationPending = false
            // The user may return after expiry having already entered a valid code.
            // Reconcile authorization once, but never fetch/display an expired code.
            if status.phase == "paired" {
                guard status.connected else { finish(.unavailable); return }
                finish(.connected); return
            }
            if Date().timeIntervalSince1970 * 1000 >= expiresAt { finish(.expired); return }
            switch status.phase {
            case "expired": finish(.expired)
            case "unknown": code = nil; phase = .unknown
            case "requested": code = nil; phase = .waiting
            case "ready":
                if code == nil {
                    let secret = try await client.whatsAppPrivateCode(link, expiresAt: expiresAt, configuration: configuration)
                    guard ticket == generation, !Task.isCancelled, shouldPoll else { return }
                    expire()
                    guard shouldPoll else { return }
                    code = secret.value
                }
                phase = .ready
            default: finish(.unavailable)
            }
        } catch {
            guard ticket == generation, !Task.isCancelled else { return }
            // HTTP 409 can mean pairing completed between the status and code reads.
            // Reconcile the same operation through status on the next poll, never create another code.
            reconciliationPending = false
            if Date().timeIntervalSince1970 * 1000 >= expiresAt { finish(.expired) }
            else if error as? APIError == .http(409) { code = nil; phase = .waiting }
            else if Self.isTemporaryTransportFailure(error) { code = nil; phase = .retrying }
            else { finish(.unavailable) }
        }
    }
}
