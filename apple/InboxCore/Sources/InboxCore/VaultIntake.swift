import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

extension ManagedClient {
    /// Vault forms bypass transcript transport and all persistent HTTP caches.
    /// Never follow redirects or automatically replay credential submissions.
    func vaultIntakeJSON(path: String, method: String = "GET", body: JSON? = nil,
                         configuration: URLSessionConfiguration = .ephemeral, maximumResponseBytes: Int = 64 * 1024) async throws -> JSON {
        configuration.urlCache = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.httpCookieStorage = nil
        configuration.httpShouldSetCookies = false
        configuration.urlCredentialStorage = nil
        let session = URLSession(configuration: configuration, delegate: NoRedirects(), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        var request = try request(path: path, method: method, body: body)
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
        // Server error bodies can contain arbitrary text. Never display or retain them.
        guard (200..<300).contains(response.statusCode) else { throw APIError.http(response.statusCode) }
        guard data.count <= maximumResponseBytes else { throw APIError.invalidResponse }
        return try JSONDecoder().decode(JSON.self, from: data)
    }
}

/// Only safe presentation hints cross the conversation boundary.
public struct VaultIntake: Codable, Equatable, Sendable {
    public let kind: String
    public let name: String
    public let origin: String?
    public let operation: String?
    public let vaultID: String?
    public let challengeID: String?
    public let agentID: String?
    public var expiresAt: Double? = nil

    public func isCurrentBrowserRequest(agentID: String, now: Date = Date()) -> Bool {
        guard operation == "browser_takeover" || operation == "browser_verification",
              self.agentID == agentID, challengeID != nil, let expiresAt else { return false }
        return expiresAt > now.timeIntervalSince1970 * 1000
    }

    public static func parse(_ value: JSON, depth: Int = 0) -> VaultIntake? {
        guard depth < 12 else { return nil }
        let value = ToolPresentation.decoded(value)
        if ["browser_vault_challenge", "browser_vault_takeover"].contains(value["type"].string), value["status"].string == "input_required" {
            guard case .object(let fields) = value,
                  Set(fields.keys) == Set(["type", "status", "challenge_id", "agent_id", "origin", "expires_at"]),
                  case .number(let expiry) = value["expires_at"], expiry.isFinite, expiry > 0,
                  value["challenge_id"].string.range(of: #"^[A-Za-z0-9_-]{22,256}$"#, options: .regularExpression) != nil,
                  (try? ManagedClient.agentPath(value["agent_id"].string)) != nil else { return nil }
            let origin = value["origin"].string
            guard let validated = parse(.object(["type": .string("vault_intake"), "status": .string("input_required"),
                "kind": .string("login"), "origin": .string(origin)])), validated.origin != nil else { return nil }
            return .init(kind: "login", name: "", origin: origin, operation: value["type"].string == "browser_vault_takeover" ? "browser_takeover" : "browser_verification", vaultID: nil,
                         challengeID: value["challenge_id"].string, agentID: value["agent_id"].string, expiresAt: expiry)
        }
        if value["type"].string == "vault_intake", value["status"].string == "input_required",
           ["login", "api_key", "card", "address", "phone"].contains(value["kind"].string) {
            guard case .object(let fields) = value,
                  Set(fields.keys).isSubset(of: ["type", "status", "kind", "name", "origin", "operation", "vault_id", "challenge_id", "agent_id"]) else { return nil }
            let name = value["name"].string
            let origin = value["origin"].string
            guard name.utf8.count <= 120, origin.utf8.count <= 2048, !name.unicodeScalars.contains(where: { $0.value < 32 || $0.value == 127 }) else { return nil }
            if !origin.isEmpty {
                guard let url = URL(string: origin), url.scheme == "https", url.host != nil,
                      url.user == nil, url.password == nil, url.query == nil, url.fragment == nil,
                      url.path.isEmpty, url.port != 443,
                      let host = url.host, host == host.lowercased(),
                      origin == "https://" + host + (url.port.map { ":" + String($0) } ?? "") else { return nil }
            }
            let operation = value["operation"].string
            guard operation.isEmpty || operation == "create" || operation == "authorize_origin" || operation == "browser_verification" else { return nil }
            let vaultID = value["vault_id"].string
            if operation == "authorize_origin" || operation == "browser_verification" {
                guard value["kind"].string == "login", !origin.isEmpty,
                      vaultID.range(of: #"^[A-Za-z0-9_-]{22,64}$"#, options: .regularExpression) != nil else { return nil }
            }
            guard (operation == "authorize_origin" || operation == "browser_verification" || vaultID.isEmpty), origin.isEmpty || value["kind"].string == "login" else { return nil }
            let challengeID = value["challenge_id"].string
            let agentID = value["agent_id"].string
            if operation == "browser_verification" {
                guard challengeID.range(of: #"^[A-Za-z0-9_-]{22,256}$"#, options: .regularExpression) != nil,
                      (try? ManagedClient.agentPath(agentID)) != nil else { return nil }
            } else if fields["challenge_id"] != nil || fields["agent_id"] != nil { return nil }
            return .init(kind: value["kind"].string, name: name, origin: origin.isEmpty ? nil : origin,
                         operation: operation.isEmpty ? nil : operation, vaultID: vaultID.isEmpty ? nil : vaultID, challengeID: challengeID.isEmpty ? nil : challengeID, agentID: agentID.isEmpty ? nil : agentID)
        }
        switch value {
        case .array(let values):
            return values.lazy.compactMap { parse($0, depth: depth + 1) }.first
        case .object(let fields):
            // Recognized transport envelopes only; do not interpret arbitrary tool data as UI.
            for key in ["content", "text", "structuredContent", "result", "output"] {
                if let child = fields[key], let intake = parse(child, depth: depth + 1) { return intake }
            }
            return nil
        default: return nil
        }
    }
}

public struct VaultIntakeReceipt: Equatable, Sendable {
    public let id: String
    public let kind: String
    public let name: String
}

extension ManagedClient {
    public func vaultLoginMetadata(id: String) async throws -> VaultIntakeReceipt {
        guard id.range(of: #"^[A-Za-z0-9_-]{22,64}$"#, options: .regularExpression) != nil else { throw APIError.invalidResponse }
        let response = try await vaultIntakeJSON(path: "/v1/credentials")
        guard case .array(let entries) = response["vault"],
              let item = entries.first(where: { $0["id"].string == id && $0["kind"].string == "login" }),
              !item["name"].string.isEmpty, item["name"].string.utf8.count <= 120 else { throw APIError.invalidResponse }
        return .init(id: id, kind: "login", name: item["name"].string)
    }
    public func authorizeVaultOrigin(id: String, origin: String, name: String) async throws -> VaultIntakeReceipt {
        guard VaultIntake.parse(.object(["type": .string("vault_intake"), "status": .string("input_required"),
            "kind": .string("login"), "operation": .string("authorize_origin"), "vault_id": .string(id), "origin": .string(origin)])) != nil else { throw APIError.invalidResponse }
        let response = try await vaultIntakeJSON(path: "/v1/credentials/vault/login/" + id + "/origin", method: "PUT",
            body: .object(["browser_origin": .string(origin)]))
        guard response["id"].string == id, response["kind"].string == "login", response["browser_origin"].string == origin else { throw APIError.invalidResponse }
        return .init(id: id, kind: "login", name: name)
    }
    public func saveVaultItem(kind: String, values: [String: String], configuration: URLSessionConfiguration = .ephemeral) async throws -> VaultIntakeReceipt {
        guard ["login", "api_key", "card", "address", "phone"].contains(kind) else { throw APIError.invalidResponse }
        let response = try await vaultIntakeJSON(path: "/v1/credentials/vault/" + kind, method: "POST",
            body: .object(values.mapValues(JSON.string)), configuration: configuration)
        let id = response["id"].string
        guard id.range(of: #"^[A-Za-z0-9_-]{22,64}$"#, options: .regularExpression) != nil,
              response["kind"].string == kind else { throw APIError.invalidResponse }
        return .init(id: id, kind: kind, name: values["name"] ?? "")
    }
}

extension ManagedClient {
    public func submitBrowserVerification(intake: VaultIntake, code: String, configuration: URLSessionConfiguration = .ephemeral) async throws {
        guard intake.operation == "browser_verification", let challenge = intake.challengeID, let agent = intake.agentID,
              code.range(of: #"^[0-9]{4,10}$"#, options: .regularExpression) != nil else { throw APIError.invalidResponse }
        let response = try await vaultIntakeJSON(path: Self.agentPath(agent) + "/browser-vault/challenge", method: "POST",
            body: .object(["challenge_id": .string(challenge), "code": .string(code)]), configuration: configuration)
        guard case .object(let fields) = response, fields.count == 3, response["type"].string == "browser_vault_challenge_receipt", response["challenge_id"].string == challenge, response["status"].string == "submitted" else { throw APIError.invalidResponse }
    }
}

public struct BrowserKeyboardHint: Sendable, Equatable {
    public let type: String
    public let multiline: Bool
}
public struct BrowserInputRegion: Sendable, Equatable {
    public let x: Double
    public let y: Double
    public let width: Double
    public let height: Double
    public let keyboard: BrowserKeyboardHint
}
public enum BrowserTakeoverFrame: Sendable {
    case active(image: Data, width: Int, height: Int)
    case activeWithInput(image: Data, width: Int, height: Int, keyboard: BrowserKeyboardHint?, inputs: [BrowserInputRegion])

    public static func parse(_ response: JSON, finishing: Bool = false) throws -> Self {
        guard case .object(let fields) = response else { throw APIError.invalidResponse }
        if finishing, fields.count == 1, response["status"].string == "finished" { return .finished }
        let prefix = "data:image/png;base64,", encoded = response["image"].string
        guard case .number(let width) = response["width"], case .number(let height) = response["height"],
              !finishing, Set(fields.keys).isSubset(of: ["status", "image", "width", "height", "keyboard", "inputs"]),
              response["status"].string == "active", encoded.hasPrefix(prefix),
              width.isFinite, height.isFinite, width >= 1, width <= 16384, height >= 1, height <= 16384,
              width.rounded() == width, height.rounded() == height,
              let data = Data(base64Encoded: String(encoded.dropFirst(prefix.count))), data.starts(with: [137,80,78,71,13,10,26,10]) else { throw APIError.invalidResponse }
        func hint(_ value: JSON, region: Bool = false) throws -> BrowserKeyboardHint {
            guard case .object(let fields) = value,
                  Set(fields.keys) == Set(region ? ["x", "y", "width", "height", "type", "multiline"] : ["type", "multiline"]),
                  ["text", "email", "url", "tel", "number", "password"].contains(value["type"].string),
                  case .bool(let multiline) = value["multiline"] else { throw APIError.invalidResponse }
            return BrowserKeyboardHint(type: value["type"].string, multiline: multiline)
        }
        let keyboard = try fields["keyboard"].map { try hint($0) }
        var inputs: [BrowserInputRegion] = []
        if let value = fields["inputs"] {
            guard case .array(let regions) = value, regions.count <= 32 else { throw APIError.invalidResponse }
            for region in regions {
                let keyboard = try hint(region, region: true)
                guard case .number(let x) = region["x"], case .number(let y) = region["y"],
                      case .number(let w) = region["width"], case .number(let h) = region["height"],
                      [x,y,w,h].allSatisfy({ $0.isFinite && $0 >= 0 && $0 <= 1 }),
                      w > 0, h > 0, x + w <= 1.000001, y + h <= 1.000001 else { throw APIError.invalidResponse }
                inputs.append(.init(x: x, y: y, width: w, height: h, keyboard: keyboard))
            }
        }
        if keyboard != nil || !inputs.isEmpty {
            return .activeWithInput(image: data, width: Int(width), height: Int(height), keyboard: keyboard, inputs: inputs)
        }
        return .active(image: data, width: Int(width), height: Int(height))
    }
    case finished
}
extension ManagedClient {
    public func browserTakeover(intake: VaultIntake, action: [String: JSON], configuration: URLSessionConfiguration = .ephemeral) async throws -> BrowserTakeoverFrame {
        guard intake.operation == "browser_takeover", let challenge = intake.challengeID, let agent = intake.agentID else { throw APIError.invalidResponse }
        var body = action; body["challenge_id"] = .string(challenge)
        let response = try await vaultIntakeJSON(path: Self.agentPath(agent) + "/browser-vault/takeover", method: "POST", body: .object(body), configuration: configuration, maximumResponseBytes: 16 * 1024 * 1024)
        return try BrowserTakeoverFrame.parse(response, finishing: action["action"] == .string("finish"))
    }
}

/// Transcript display only; the original bound receipt remains intact for delivery.
public enum BrowserReceiptPresentation {
    public static func summary(_ text: String) -> String? {
        guard text.utf8.count <= 1024, let data = text.data(using: .utf8),
              let value = try? JSONDecoder().decode(JSON.self, from: data),
              case .object(let fields) = value,
              Set(fields.keys) == Set(["type", "status", "challenge_id"]),
              value["challenge_id"].string.range(of: #"^[A-Za-z0-9_-]{22,256}$"#, options: .regularExpression) != nil else { return nil }
        switch (value["type"].string, value["status"].string) {
        case ("browser_vault_takeover_receipt", "finished"): return "Private browser control finished"
        case ("browser_vault_challenge_receipt", "submitted"): return "Browser verification code submitted"
        default: return nil
        }
    }
}
