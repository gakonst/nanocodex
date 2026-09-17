import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

extension ManagedClient {
    /// Vault forms bypass transcript transport and all persistent HTTP caches.
    /// Never follow redirects or automatically replay credential submissions.
    func vaultIntakeJSON(path: String, method: String = "GET", body: JSON? = nil,
                         configuration: URLSessionConfiguration = .ephemeral) async throws -> JSON {
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
        guard data.count <= 64 * 1024 else { throw APIError.invalidResponse }
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

    public static func parse(_ value: JSON, depth: Int = 0) -> VaultIntake? {
        guard depth < 12 else { return nil }
        let value = ToolPresentation.decoded(value)
        if value["type"].string == "vault_intake", value["status"].string == "input_required",
           ["login", "api_key", "card", "address", "phone"].contains(value["kind"].string) {
            guard case .object(let fields) = value,
                  Set(fields.keys).isSubset(of: ["type", "status", "kind", "name", "origin", "operation", "vault_id"]) else { return nil }
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
            guard operation.isEmpty || operation == "create" || operation == "authorize_origin" else { return nil }
            let vaultID = value["vault_id"].string
            if operation == "authorize_origin" {
                guard value["kind"].string == "login", !origin.isEmpty,
                      vaultID.range(of: #"^[A-Za-z0-9_-]{22,64}$"#, options: .regularExpression) != nil else { return nil }
            }
            guard (operation == "authorize_origin" || vaultID.isEmpty), origin.isEmpty || value["kind"].string == "login" else { return nil }
            return .init(kind: value["kind"].string, name: name, origin: origin.isEmpty ? nil : origin,
                         operation: operation.isEmpty ? nil : operation, vaultID: vaultID.isEmpty ? nil : vaultID)
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
