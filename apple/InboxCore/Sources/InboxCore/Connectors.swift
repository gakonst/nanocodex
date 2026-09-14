import Foundation

public struct ConnectorCapabilityDefinition: Identifiable, Equatable, Sendable {
    public let id: String
    public let name: String
}

public struct ConnectorProviderDefinition: Identifiable, Equatable, Sendable {
    public let id: String
    public let name: String
    public let description: String
    public let capabilities: [ConnectorCapabilityDefinition]
}

public struct ConnectorAccountConnection: Identifiable, Equatable, Sendable {
    public let id: String
    public let label: String
    public let accountID: String?
    public let capabilities: [String]
}

public struct ConnectorCapabilityStatus: Equatable, Sendable {
    public let connected: Bool
    public let connections: [ConnectorAccountConnection]
    public let legacyLabel: String?
}

public struct ConnectorOverview: Equatable, Sendable {
    public let providers: [ConnectorProviderDefinition]
    public let statuses: [String: ConnectorCapabilityStatus]

    public func isConnected(_ provider: ConnectorProviderDefinition) -> Bool {
        provider.capabilities.contains { statuses[$0.id]?.connected == true }
    }

    public func connections(for provider: ConnectorProviderDefinition) -> [ConnectorAccountConnection] {
        var connections: [String: ConnectorAccountConnection] = [:]
        for capability in provider.capabilities {
            for connection in statuses[capability.id]?.connections ?? [] {
                let prior = connections[connection.id]
                let capabilities = Set((prior?.capabilities ?? []) + connection.capabilities + [capability.id]).sorted()
                connections[connection.id] = ConnectorAccountConnection(
                    id: connection.id,
                    label: connection.label,
                    accountID: connection.accountID ?? prior?.accountID,
                    capabilities: capabilities
                )
            }
        }
        return connections.values.sorted {
            $0.label.localizedCaseInsensitiveCompare($1.label) == .orderedAscending
        }
    }

    public func capabilityNames(
        for connection: ConnectorAccountConnection,
        provider: ConnectorProviderDefinition
    ) -> [String] {
        let names = Dictionary(uniqueKeysWithValues: provider.capabilities.map { ($0.id, $0.name) })
        return connection.capabilities.compactMap { names[$0] }
    }
}

public enum ConnectorAuthorizationResult: String, Equatable, Sendable {
    case connected, cancelled, failed
}

public struct ConnectorAuthorization: Equatable, Sendable {
    public let provider: String
    public let authorizationURL: URL
    public let callbackURL: URL
    public let attemptID: String

    public func result(from url: URL) throws -> ConnectorAuthorizationResult {
        guard url.scheme == callbackURL.scheme,
              url.host == callbackURL.host,
              url.port == callbackURL.port,
              url.path == callbackURL.path,
              let components = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
            throw APIError.invalidResponse
        }
        let items = components.queryItems ?? []
        guard items.count == 3,
              items.filter({ $0.name == "attempt" }).count == 1,
              items.first(where: { $0.name == "attempt" })?.value == attemptID,
              items.filter({ $0.name == "connector" }).count == 1,
              items.first(where: { $0.name == "connector" })?.value == provider,
              let raw = items.first(where: { $0.name == "connector_result" })?.value,
              items.filter({ $0.name == "connector_result" }).count == 1,
              let result = ConnectorAuthorizationResult(rawValue: raw) else {
            throw APIError.invalidResponse
        }
        return result
    }
}

public extension ManagedClient {
    func connectorOverview() async throws -> ConnectorOverview {
        async let catalog = json(path: "/v1/connectors/catalog")
        async let statuses = json(path: "/v1/connectors")
        let (catalogValue, statusValue) = try await (catalog, statuses)
        return try ConnectorOverview(
            providers: Self.connectorProviders(from: catalogValue),
            statuses: Self.connectorStatuses(from: statusValue)
        )
    }

    func beginConnectorAuthorization(provider: String) async throws -> ConnectorAuthorization {
        let provider = try Self.connectorPathComponent(provider)
        let attemptID = UUID().uuidString.lowercased()
        var callback = URLComponents(string: "nanocodex://connectors/complete")
        callback?.queryItems = [URLQueryItem(name: "attempt", value: attemptID)]
        guard let callbackURL = callback?.url else { throw APIError.invalidResponse }
        let returnTo = "/v1/connectors/mobile-complete?attempt=" + attemptID
        let response = try await json(
            path: "/v1/connectors/\(provider)",
            method: "POST",
            body: .object(["return_to": .string(returnTo)])
        )
        guard let authorizationURL = URL(string: response["authorization_url"].string),
              authorizationURL.scheme == "https",
              authorizationURL.user == nil,
              authorizationURL.password == nil,
              authorizationURL.fragment == nil else {
            throw APIError.invalidResponse
        }
        return ConnectorAuthorization(
            provider: provider,
            authorizationURL: authorizationURL,
            callbackURL: callbackURL,
            attemptID: attemptID
        )
    }

    func disconnectConnector(provider: String, connectionID: String) async throws {
        let provider = try Self.connectorPathComponent(provider)
        guard connectionID.range(of: #"^[A-Za-z0-9_-]{43}$"#, options: .regularExpression) != nil else {
            throw APIError.invalidResponse
        }
        _ = try await json(
            path: "/v1/connectors/\(provider)/connections/\(connectionID)",
            method: "DELETE"
        )
    }

    private static func connectorPathComponent(_ value: String) throws -> String {
        guard value.range(of: #"^[a-z][a-z0-9_-]{0,63}$"#, options: .regularExpression) != nil else {
            throw APIError.invalidResponse
        }
        return value
    }

    private static func connectorProviders(from value: JSON) throws -> [ConnectorProviderDefinition] {
        guard case .array(let rawProviders) = value["providers"],
              !rawProviders.isEmpty, rawProviders.count <= 64 else { throw APIError.invalidResponse }
        var ids = Set<String>()
        let providers = try rawProviders.map { raw -> ConnectorProviderDefinition in
            let id = try connectorIdentifier(raw["id"].string)
            let name = try displayString(raw["name"].string, maximum: 128)
            let description = try displayString(raw["description"].string, maximum: 512)
            guard ids.insert(id).inserted,
                  case .array(let rawCapabilities) = raw["capabilities"],
                  !rawCapabilities.isEmpty, rawCapabilities.count <= 32 else { throw APIError.invalidResponse }
            var capabilityIDs = Set<String>()
            let capabilities = try rawCapabilities.map { capability -> ConnectorCapabilityDefinition in
                let capabilityID = try connectorIdentifier(capability["id"].string)
                guard capabilityIDs.insert(capabilityID).inserted else { throw APIError.invalidResponse }
                return ConnectorCapabilityDefinition(
                    id: capabilityID,
                    name: try displayString(capability["name"].string, maximum: 128)
                )
            }
            return ConnectorProviderDefinition(
                id: id,
                name: name,
                description: description,
                capabilities: capabilities
            )
        }
        return providers
    }

    private static func connectorStatuses(from value: JSON) throws -> [String: ConnectorCapabilityStatus] {
        guard case .object(let rawStatuses) = value["connectors"], rawStatuses.count <= 128 else {
            throw APIError.invalidResponse
        }
        return try Dictionary(uniqueKeysWithValues: rawStatuses.map { id, raw in
            let id = try connectorIdentifier(id)
            let connected = raw["connected"].bool
            var connections: [ConnectorAccountConnection] = []
            if case .array(let rawConnections) = raw["connections"] {
                guard rawConnections.count <= 64 else { throw APIError.invalidResponse }
                var connectionIDs = Set<String>()
                connections = try rawConnections.map { connection in
                    let connectionID = connection["id"].string
                    guard connectionID.range(of: #"^[A-Za-z0-9_-]{43}$"#, options: .regularExpression) != nil,
                          connectionIDs.insert(connectionID).inserted else { throw APIError.invalidResponse }
                    let capabilities: [String]
                    if case .array(let rawCapabilities) = connection["capabilities"] {
                        guard rawCapabilities.count <= 32 else { throw APIError.invalidResponse }
                        capabilities = try rawCapabilities.map { try connectorIdentifier($0.string) }
                        guard Set(capabilities).count == capabilities.count else { throw APIError.invalidResponse }
                    } else { capabilities = [] }
                    let accountID = connection["account_id"].string
                    return ConnectorAccountConnection(
                        id: connectionID,
                        label: try displayString(connection["label"].string, maximum: 256),
                        accountID: accountID.isEmpty ? nil : try displayString(accountID, maximum: 256),
                        capabilities: capabilities
                    )
                }
            }
            let legacyLabel = raw["label"].string.isEmpty ? raw["account"].string : raw["label"].string
            return (id, ConnectorCapabilityStatus(
                connected: connected,
                connections: connections,
                legacyLabel: legacyLabel.isEmpty ? nil : try displayString(legacyLabel, maximum: 256)
            ))
        })
    }

    private static func connectorIdentifier(_ value: String) throws -> String {
        guard value.range(of: #"^[a-z][a-z0-9_-]{0,63}$"#, options: .regularExpression) != nil else {
            throw APIError.invalidResponse
        }
        return value
    }

    private static func displayString(_ value: String, maximum: Int) throws -> String {
        let value = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty, value.count <= maximum,
              value.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) }) else {
            throw APIError.invalidResponse
        }
        return value
    }
}
