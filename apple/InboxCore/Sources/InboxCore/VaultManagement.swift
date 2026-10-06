import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

public struct VaultItem: Identifiable, Sendable {
    public let id: String
    public let kind: String
    public let name: String
    public let detail: String
}
public struct SSHVaultIdentity: Identifiable, Sendable {
    public let id: String
    public let hostname: String
    public let publicKey: String
}
public struct VaultOverview: Sendable {
    public let items: [VaultItem]
    public let ssh: [SSHVaultIdentity]
}
public extension ManagedClient {
    func vaultOverview() async throws -> VaultOverview {
        let value = try await vaultIntakeJSON(path: "/v1/credentials")
        guard case .array(let items) = value["vault"], case .array(let ssh) = value["ssh"] else { throw APIError.invalidResponse }
        return try VaultOverview(items: items.map { item in
            try Self.validateVaultID(item["id"].string)
            guard ["login", "api_key", "card", "address", "phone"].contains(item["kind"].string) else { throw APIError.invalidResponse }
            return VaultItem(id: item["id"].string, kind: item["kind"].string, name: item["name"].string,
                detail: item["username"].string + item["last4"].string + item["phone_number"].string)
        }, ssh: ssh.map { item in
            try Self.validateSSHReference(item["reference"].string)
            return SSHVaultIdentity(id: item["reference"].string, hostname: item["hostname"].string, publicKey: item["public_key"].string)
        })
    }
    func deleteVaultItem(_ item: VaultItem) async throws {
        try Self.validateVaultID(item.id)
        guard ["login", "api_key", "card", "address", "phone"].contains(item.kind) else { throw APIError.invalidResponse }
        _ = try await vaultIntakeJSON(path: "/v1/credentials/vault/\(item.kind)/\(item.id)", method: "DELETE")
    }
    func saveSSHIdentity(reference: String, hostname: String, port: Int, username: String, hostKeySHA256: String, privateKey: String? = nil) async throws {
        try Self.validateSSHReference(reference)
        var body: [String: JSON] = ["hostname": .string(hostname), "port": .number(Double(port)), "username": .string(username), "host_key_sha256": .string(hostKeySHA256)]
        if let privateKey { body["private_key"] = .string(privateKey) } else { body["generate"] = .bool(true) }
        _ = try await vaultIntakeJSON(path: "/v1/credentials/ssh/" + reference, method: "PUT", body: .object(body))
    }
    func deleteSSHIdentity(reference: String) async throws {
        try Self.validateSSHReference(reference)
        _ = try await vaultIntakeJSON(path: "/v1/credentials/ssh/" + reference, method: "DELETE")
    }
    func storeProviderCapture(captureID: String, operationID: UUID, name: String? = nil, addressVaultID: String? = nil) async throws -> JSON {
        try Self.validateVaultID(captureID)
        var body: [String: JSON] = ["capture_id": .string(captureID), "operation_id": .string(operationID.uuidString)]
        if let name { body["name"] = .string(name) }
        if let addressVaultID { try Self.validateVaultID(addressVaultID); body["address_vault_id"] = .string(addressVaultID) }
        return try await vaultIntakeJSON(path: "/v1/vault/store", method: "POST", body: .object(body))
    }
    func providerCard(operation: String, vaultID: String? = nil, captureID: String? = nil, operationID: UUID? = nil, configuration: URLSessionConfiguration = .ephemeral) async throws -> JSON {
        guard ["status", "balance", "refresh"].contains(operation), (vaultID == nil) != (captureID == nil), operation != "refresh" || operationID != nil else { throw APIError.invalidResponse }
        try Self.validateVaultID(vaultID ?? captureID ?? "")
        var body: [String: JSON] = ["operation": .string(operation)]
        if let vaultID { body["vault_id"] = .string(vaultID) }
        if let captureID { body["capture_id"] = .string(captureID) }
        if let operationID { body["operation_id"] = .string(operationID.uuidString) }
        return try await vaultIntakeJSON(path: "/v1/vault/card", method: "POST", body: .object(body), configuration: configuration)
    }
    func nativeCredentialOperation(path: String, method: String = "GET", body: JSON? = nil, configuration: URLSessionConfiguration = .ephemeral) async throws -> JSON {
        guard ["/v1/credentials", "/v1/credentials/chatgpt/login", "/v1/credentials/chatgpt", "/v1/credentials/openai", "/v1/connectors/cloudflare"].contains(path) else { throw APIError.invalidResponse }
        return try await vaultIntakeJSON(path: path, method: method, body: body, configuration: configuration)
    }
    private static func validateVaultID(_ id: String) throws {
        guard id.range(of: #"^[A-Za-z0-9_-]{22,64}$"#, options: .regularExpression) != nil else { throw APIError.invalidResponse }
    }
    private static func validateSSHReference(_ ref: String) throws {
        guard ref.range(of: #"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$"#, options: .regularExpression) != nil else { throw APIError.invalidResponse }
    }
}

public struct ChatGPTLoginReceipt: Sendable {
    public let state: String
    public let userCode: String?
    public let verificationURL: URL?
    public init(_ value: JSON) throws {
        state = value["state"].string
        guard ["pending", "authenticated", "not_started", "expired"].contains(state) else { throw APIError.invalidResponse }
        if state == "pending" {
            guard let url = URL(string: value["verification_url"].string), url.scheme == "https", url.host == "auth.openai.com", url.user == nil, url.password == nil,
                  case .number(let expiry) = value["expires_at"], expiry.isFinite,
                  case .number(let poll) = value["poll_after_ms"], poll.isFinite, poll >= 0,
                  !value["user_code"].string.isEmpty else { throw APIError.invalidResponse }
            userCode = value["user_code"].string
            verificationURL = url
        } else { userCode = nil; verificationURL = nil }
    }
}
public struct ProviderCardReceipt: Sendable {
    public let status: String
    public let display: String
    public init(_ value: JSON) throws {
        status = value["status"].string
        guard ["captured", "pending", "awaiting_card", "awaiting_billing_address", "saved", "ready", "balance_pending", "outcome_unknown", "failed", "refresh_pending", "awaiting_issuer_approval"].contains(status) else { throw APIError.invalidResponse }
        var text = status.replacingOccurrences(of: "_", with: " ")
        if case .number(let balance) = value["balance"], balance.isFinite, balance >= 0, value["currency"].string == "USD" {
            text += String(format: " · USD %.2f", balance)
        }
        if case .number(let observed) = value["observed_at"], observed.isFinite, observed >= 0 {
            text += " · Observed " + Date(timeIntervalSince1970: observed / 1000).formatted()
        }
        display = text
    }
}
public extension ManagedClient {
    func disconnectCloudflare(connectionID: String) async throws {
        guard connectionID.range(of: #"^[A-Za-z0-9_-]{22,64}$"#, options: .regularExpression) != nil else { throw APIError.invalidResponse }
        _ = try await vaultIntakeJSON(path: "/v1/connectors/cloudflare/connections/" + connectionID, method: "DELETE")
    }
}
