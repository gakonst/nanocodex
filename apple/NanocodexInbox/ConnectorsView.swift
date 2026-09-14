import AuthenticationServices
import InboxCore
import SwiftUI
import UIKit

struct ConnectorsView: View {
    @ObservedObject var model: InboxModel
    @StateObject private var center = ConnectorCenter()
    @State private var query = ""

    private var providers: [ConnectorProviderDefinition] {
        guard let overview = center.overview else { return [] }
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return overview.providers }
        return overview.providers.filter { provider in
            ([provider.name, provider.description]
                + provider.capabilities.map(\.name)
                + overview.connections(for: provider).map(\.label))
                .contains { $0.localizedCaseInsensitiveContains(query) }
        }
    }
    private var connected: [ConnectorProviderDefinition] {
        guard let overview = center.overview else { return [] }
        return providers.filter(overview.isConnected)
    }
    private var available: [ConnectorProviderDefinition] {
        guard let overview = center.overview else { return providers }
        return providers.filter { !overview.isConnected($0) }
    }

    var body: some View {
        List {
            if center.loading, center.overview == nil {
                HStack { Spacer(); ProgressView("Loading connectors"); Spacer() }
                    .listRowBackground(Color.clear)
            }
            if !connected.isEmpty {
                Section("Connected") {
                    ForEach(connected) { provider in
                        NavigationLink {
                            ConnectorProviderView(model: model, center: center, provider: provider)
                        } label: {
                            ConnectorRow(
                                provider: provider,
                                detail: connectedDetail(provider),
                                action: nil,
                                busy: center.operation == provider.id
                            )
                        }
                        .accessibilityIdentifier("connector-connected:" + provider.id)
                    }
                }
            }
            if !available.isEmpty {
                Section("Available") {
                    ForEach(available) { provider in
                        Button {
                            Task { await center.connect(provider, using: model) }
                        } label: {
                            ConnectorRow(
                                provider: provider,
                                detail: provider.description,
                                action: "Connect",
                                busy: center.operation == provider.id
                            )
                        }
                        .buttonStyle(.plain)
                        .disabled(center.operation != nil)
                        .accessibilityIdentifier("connector-available:" + provider.id)
                    }
                }
            }
            if center.overview != nil, providers.isEmpty {
                ContentUnavailableView.search(text: query)
                    .listRowBackground(Color.clear)
            }
            if let error = center.error {
                Section {
                    VStack(alignment: .leading, spacing: 10) {
                        Text(error).font(.subheadline).foregroundStyle(.secondary)
                        Button("Try again") { Task { await center.load(using: model) } }
                    }
                    .accessibilityIdentifier("connector-error")
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Connectors")
        .navigationBarTitleDisplayMode(.inline)
        .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search connectors")
        .refreshable { await center.load(using: model) }
        .task { if center.overview == nil { await center.load(using: model) } }
        .accessibilityIdentifier("connectors-list")
    }

    private func connectedDetail(_ provider: ConnectorProviderDefinition) -> String {
        guard let overview = center.overview else { return "Connected" }
        let count = overview.connections(for: provider).count
        if count == 0 { return "Connected" }
        return count == 1 ? overview.connections(for: provider)[0].label : "\(count) accounts"
    }
}

private struct ConnectorProviderView: View {
    @ObservedObject var model: InboxModel
    @ObservedObject var center: ConnectorCenter
    let provider: ConnectorProviderDefinition
    @State private var pendingRevocation: ConnectorAccountConnection?

    private var overview: ConnectorOverview? { center.overview }
    private var connections: [ConnectorAccountConnection] { overview?.connections(for: provider) ?? [] }

    var body: some View {
        List {
            Section {
                HStack(alignment: .top, spacing: 14) {
                    ConnectorLogo(provider: provider.id, size: 54)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(provider.name).font(.headline)
                        Text(provider.description).font(.subheadline).foregroundStyle(.secondary)
                    }
                }
                .padding(.vertical, 4)
            }
            if !connections.isEmpty {
                Section(connections.count == 1 ? "Account" : "Accounts") {
                    ForEach(connections) { connection in
                        VStack(alignment: .leading, spacing: 8) {
                            HStack(alignment: .firstTextBaseline) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(connection.label).font(.body.weight(.medium))
                                    let capabilities = overview?.capabilityNames(for: connection, provider: provider) ?? []
                                    if !capabilities.isEmpty {
                                        Text(capabilities.joined(separator: " · "))
                                            .font(.caption).foregroundStyle(.secondary)
                                            .lineLimit(3)
                                    }
                                }
                                Spacer(minLength: 12)
                                Button("Revoke", role: .destructive) { pendingRevocation = connection }
                                    .disabled(center.operation != nil)
                            }
                        }
                        .padding(.vertical, 2)
                        .accessibilityIdentifier("connector-account:" + connection.id)
                    }
                }
            }
            if provider.capabilities.count > 1 {
                Section("Services") {
                    ForEach(provider.capabilities) { capability in
                        HStack(spacing: 12) {
                            Image(systemName: capabilitySymbol(capability.id))
                                .foregroundStyle(.blue)
                                .frame(width: 26)
                            Text(capability.name)
                            Spacer()
                            if overview?.statuses[capability.id]?.connected == true {
                                Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
                            }
                        }
                    }
                }
            }
            Section {
                Button {
                    Task { await center.connect(provider, using: model) }
                } label: {
                    HStack {
                        Label(connections.isEmpty ? "Connect" : "Add another account", systemImage: "plus.circle")
                        Spacer()
                        if center.operation == provider.id { ProgressView() }
                    }
                }
                .disabled(center.operation != nil)
                .accessibilityIdentifier("connector-add-account")
            } footer: {
                Text("Credentials stay in the account broker. Agents receive only the exact connector access you approve.")
            }
            if let error = center.error {
                Section { Text(error).font(.subheadline).foregroundStyle(.secondary) }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(provider.name)
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await center.load(using: model) }
        .confirmationDialog(
            "Revoke \(pendingRevocation?.label ?? "this account")?",
            isPresented: Binding(
                get: { pendingRevocation != nil },
                set: { if !$0 { pendingRevocation = nil } }
            ),
            titleVisibility: .visible
        ) {
            if let connection = pendingRevocation {
                Button("Revoke account", role: .destructive) {
                    pendingRevocation = nil
                    Task { await center.revoke(connection, from: provider, using: model) }
                }
            }
            Button("Cancel", role: .cancel) { pendingRevocation = nil }
        } message: {
            Text("Nanocodex agents will immediately lose access to this exact account. Other \(provider.name) accounts stay connected.")
        }
    }

    private func capabilitySymbol(_ id: String) -> String {
        switch id {
        case "gmail": "envelope.fill"
        case "gcalendar": "calendar"
        case "gcontacts": "person.crop.circle"
        case "gdocs": "doc.text.fill"
        case "gdrive": "externaldrive.fill"
        case "gsheets": "tablecells.fill"
        case "gslides": "rectangle.on.rectangle.angled"
        case "gtasks": "checkmark.circle.fill"
        default: "link"
        }
    }
}

private struct ConnectorRow: View {
    let provider: ConnectorProviderDefinition
    let detail: String
    let action: String?
    let busy: Bool

    var body: some View {
        HStack(spacing: 14) {
            ConnectorLogo(provider: provider.id, size: 42)
            VStack(alignment: .leading, spacing: 2) {
                Text(provider.name).font(.body)
                Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(2)
            }
            Spacer(minLength: 10)
            if busy { ProgressView() }
            else if let action { Text(action).font(.body.weight(.medium)).foregroundStyle(.blue) }
        }
        .contentShape(Rectangle())
        .padding(.vertical, 3)
    }
}

private struct ConnectorLogo: View {
    let provider: String
    let size: CGFloat

    private var symbol: String {
        switch provider {
        case "github": "chevron.left.forwardslash.chevron.right"
        case "slack": "number"
        case "x": "xmark"
        default: "link"
        }
    }
    private var foreground: Color {
        switch provider {
        case "google": .blue
        case "slack": .purple
        default: .primary
        }
    }

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: size * 0.24, style: .continuous)
                .fill(Color(uiColor: .secondarySystemGroupedBackground))
                .shadow(color: .black.opacity(0.07), radius: 5, y: 2)
            if provider == "google" {
                Text("G").font(.system(size: size * 0.54, weight: .bold, design: .rounded))
                    .foregroundStyle(
                        AngularGradient(colors: [.blue, .red, .yellow, .green, .blue], center: .center)
                    )
            } else {
                Image(systemName: symbol).font(.system(size: size * 0.40, weight: .semibold))
                    .foregroundStyle(foreground)
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

@MainActor
private final class ConnectorCenter: NSObject, ObservableObject, ASWebAuthenticationPresentationContextProviding {
    @Published var overview: ConnectorOverview?
    @Published var loading = false
    @Published var operation: String?
    @Published var error: String?
    private var authenticationSession: ASWebAuthenticationSession?

    func load(using model: InboxModel) async {
        guard !loading else { return }
        loading = true
        defer { loading = false }
        do {
            overview = try await model.connectorOverview()
            error = nil
        } catch { self.error = error.localizedDescription }
    }

    func connect(_ provider: ConnectorProviderDefinition, using model: InboxModel) async {
        guard operation == nil else { return }
        operation = provider.id
        error = nil
        defer { operation = nil }
        do {
            let authorization = try await model.beginConnectorAuthorization(provider.id)
            let addingAnother = !(overview?.connections(for: provider).isEmpty ?? true)
            let callbackURL = try await authenticate(authorization, prefersEphemeral: addingAnother)
            switch try authorization.result(from: callbackURL) {
            case .connected:
                overview = try await model.connectorOverview()
            case .cancelled:
                return
            case .failed:
                error = "\(provider.name) couldn’t be connected. Try again."
            }
        } catch let authenticationError as ASWebAuthenticationSessionError
            where authenticationError.code == .canceledLogin {
            return
        } catch { self.error = error.localizedDescription }
    }

    func revoke(
        _ connection: ConnectorAccountConnection,
        from provider: ConnectorProviderDefinition,
        using model: InboxModel
    ) async {
        guard operation == nil else { return }
        operation = provider.id
        error = nil
        defer { operation = nil }
        do {
            try await model.disconnectConnector(provider.id, connectionID: connection.id)
            overview = try await model.connectorOverview()
        } catch { self.error = error.localizedDescription }
    }

    private func authenticate(
        _ authorization: ConnectorAuthorization,
        prefersEphemeral: Bool
    ) async throws -> URL {
        guard let scheme = authorization.callbackURL.scheme else { throw APIError.invalidResponse }
        authenticationSession?.cancel()
        return try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<URL, Error>) in
            let session = ASWebAuthenticationSession(
                url: authorization.authorizationURL,
                callbackURLScheme: scheme
            ) { [weak self] callbackURL, error in
                Task { @MainActor in
                    self?.authenticationSession = nil
                    if let callbackURL { continuation.resume(returning: callbackURL) }
                    else { continuation.resume(throwing: error ?? APIError.invalidResponse) }
                }
            }
            session.presentationContextProvider = self
            // A clean provider session makes “Add another” reliable even for
            // providers that would otherwise silently reuse the current login.
            session.prefersEphemeralWebBrowserSession = prefersEphemeral
            authenticationSession = session
            if !session.start() {
                authenticationSession = nil
                continuation.resume(throwing: APIError.invalidResponse)
            }
        }
    }

    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
            .first(where: \.isKeyWindow) ?? UIWindow()
    }
}
