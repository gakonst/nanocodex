import AuthenticationServices
import InboxCore
import SwiftUI
import UIKit

struct ConnectorsView: View {
    @ObservedObject var model: InboxModel
    @StateObject private var center = ConnectorCenter()
    @State private var query = ""
    @State private var showingAddMcp = false
    @State private var showingMusic: MusicLoopbackProvider?

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
    private var mcpConnections: [McpConnection] {
        guard let overview = center.overview else { return [] }
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return overview.mcpConnections }
        return overview.mcpConnections.filter { $0.name.localizedCaseInsensitiveContains(query) }
    }
    private var connectedMcp: [McpConnection] {
        mcpConnections.filter { $0.status == .connected }
    }
    private var availableMcp: [McpConnection] {
        mcpConnections.filter { $0.status != .connected && $0.status != .revoked }
    }
    private var showsAddMcp: Bool {
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return query.isEmpty || "Add MCP server".localizedCaseInsensitiveContains(query)
    }
    private var showsChatGpt: Bool {
        query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || "ChatGPT accounts model subscriptions".localizedCaseInsensitiveContains(query.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    var body: some View {
        List {
            if showsChatGpt, let url = model.chatGptAccountsURL {
                Section {
                    Link(destination: url) {
                        Label("ChatGPT accounts", systemImage: "person.crop.circle.badge.plus")
                    }
                    .accessibilityIdentifier("chatgpt-accounts")
                } header: {
                    Text("Model access")
                } footer: {
                    Text("Add accounts and view account status on the web. Sign in with the same Nanocodex account you use here.")
                }
            }
            if center.loading, center.overview == nil {
                HStack { Spacer(); ProgressView("Loading connectors"); Spacer() }
                    .listRowBackground(Color.clear)
            }
            if !connected.isEmpty || !connectedMcp.isEmpty {
                Section("Connected") {
                    ForEach(connected) { provider in
                        NavigationLink {
                            if let music = MusicLoopbackProvider(rawValue: provider.id) {
                                Form { MusicConnectionView(model: model, provider: music) }
                                    .navigationTitle(music.name)
                                    .onDisappear { Task { await center.load(using: model) } }
                            } else {
                                ConnectorProviderView(model: model, center: center, provider: provider)
                            }
                        } label: {
                            ConnectorRow(
                                provider: provider,
                                action: nil,
                                detail: connectedDetail(provider),
                                busy: center.operation == provider.id
                            )
                        }
                        .accessibilityIdentifier("connector-connected:" + provider.id)
                        .accessibilityValue(connectedDetail(provider))
                    }
                    ForEach(connectedMcp) { connection in
                        NavigationLink {
                            McpConnectionView(model: model, center: center, connection: connection)
                        } label: {
                            McpRow(
                                connection: connection,
                                action: nil,
                                busy: center.operation == "mcp:" + connection.id
                            )
                        }
                        .accessibilityIdentifier("mcp-connected:" + connection.id)
                    }
                }
            }
            if !available.isEmpty || !availableMcp.isEmpty || showsAddMcp {
                Section("Available") {
                    ForEach(available) { provider in
                        Button {
                            if let music = MusicLoopbackProvider(rawValue: provider.id) { showingMusic = music }
                            else { Task { await center.connect(provider, using: model) } }
                        } label: {
                            ConnectorRow(
                                provider: provider,
                                action: "Connect",
                                busy: center.operation == provider.id
                            )
                        }
                        .buttonStyle(.plain)
                        .disabled(center.operation != nil)
                        .accessibilityIdentifier("connector-available:" + provider.id)
                    }
                    ForEach(availableMcp) { connection in
                        Button {
                            Task { await center.connect(connection, using: model) }
                        } label: {
                            McpRow(
                                connection: connection,
                                action: connection.status == .reauthorizationRequired ? "Reconnect" : "Connect",
                                busy: center.operation == "mcp:" + connection.id
                            )
                        }
                        .buttonStyle(.plain)
                        .disabled(center.operation != nil || connection.status == .disabled)
                        .accessibilityIdentifier("mcp-available:" + connection.id)
                    }
                    if showsAddMcp {
                        Button { showingAddMcp = true } label: {
                            HStack(spacing: 12) {
                                ConnectorLogo(provider: "mcp", size: 34)
                                Text("Add MCP server")
                                Spacer()
                                Image(systemName: "plus").foregroundStyle(.blue)
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .disabled(center.operation != nil)
                        .accessibilityIdentifier("mcp-add")
                    }
                }
            }
            if center.overview != nil, providers.isEmpty, mcpConnections.isEmpty, !showsAddMcp, !showsChatGpt {
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
        .task {
            openRequestedMusicConnector()
            await center.load(using: model)
        }
        .onChange(of: model.musicConnectorToOpen) { _, _ in openRequestedMusicConnector() }
        .sheet(isPresented: $showingAddMcp) {
            AddMcpView(model: model, center: center)
        }
        .sheet(item: $showingMusic, onDismiss: { Task { await center.load(using: model) } }) { music in
            NavigationStack {
                Form { MusicConnectionView(model: model, provider: music) }
                    .navigationTitle(music.name)
                    .toolbar { ToolbarItem(placement: .confirmationAction) {
                        Button("Done") { showingMusic = nil }
                    } }
            }
        }
        .accessibilityIdentifier("connectors-list")
    }

    private func openRequestedMusicConnector() {
        guard let provider = model.musicConnectorToOpen else { return }
        showingMusic = provider
        model.musicConnectorToOpen = nil
    }

    private func connectedDetail(_ provider: ConnectorProviderDefinition) -> String {
        guard let overview = center.overview else { return "Connected" }
        let count = overview.connections(for: provider).count
        if count == 0 { return "Connected" }
        return count == 1 ? overview.connections(for: provider)[0].label : "\(count) accounts"
    }
}

private struct AddMcpView: View {
    @ObservedObject var model: InboxModel
    @ObservedObject var center: ConnectorCenter
    @Environment(\.dismiss) private var dismiss
    @State private var target = ""

    var body: some View {
        NavigationStack {
            Form {
                TextField("MCP server URL", text: $target)
                    .textInputAutocapitalization(.never)
                    .keyboardType(.URL)
                    .autocorrectionDisabled()
                    .submitLabel(.go)
                    .onSubmit { add() }
                    .accessibilityIdentifier("mcp-target")
                if let error = center.error {
                    Text(error).font(.subheadline).foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Add MCP server")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if center.operation == "mcp:add" { ProgressView() }
                    else { Button("Add") { add() }.disabled(target.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty) }
                }
            }
        }
    }

    private func add() {
        let target = target.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !target.isEmpty else { return }
        Task { if await center.addMcp(target, using: model) { dismiss() } }
    }
}

private struct McpConnectionView: View {
    @ObservedObject var model: InboxModel
    @ObservedObject var center: ConnectorCenter
    let connection: McpConnection
    @State private var confirmingRevoke = false

    var body: some View {
        List {
            Section {
                HStack(spacing: 12) {
                    ConnectorLogo(provider: "mcp", size: 36)
                    Text(connection.name).font(.body.weight(.medium)).lineLimit(1)
                }
            }
            Section {
                Button("Revoke", role: .destructive) { confirmingRevoke = true }
                    .disabled(center.operation != nil)
                    .accessibilityIdentifier("mcp-revoke")
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(connection.name)
        .navigationBarTitleDisplayMode(.inline)
        .confirmationDialog("Revoke \(connection.name)?", isPresented: $confirmingRevoke) {
            Button("Revoke", role: .destructive) {
                Task { await center.revoke(connection, using: model) }
            }
            Button("Cancel", role: .cancel) {}
        }
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
                HStack(spacing: 12) {
                    ConnectorLogo(provider: provider.id, size: 36)
                    Text(provider.name).font(.body.weight(.medium)).lineLimit(1)
                }
            }
            if !connections.isEmpty {
                Section(connections.count == 1 ? "Account" : "Accounts") {
                    ForEach(connections) { connection in
                        HStack {
                            Text(connection.label).font(.body.weight(.medium)).lineLimit(1)
                            Spacer(minLength: 12)
                            Button("Revoke", role: .destructive) { pendingRevocation = connection }
                                .disabled(center.operation != nil)
                        }
                        .accessibilityIdentifier("connector-account:" + connection.id)
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
    let action: String?
    var detail: String? = nil
    let busy: Bool

    var body: some View {
        HStack(spacing: 12) {
            ConnectorLogo(provider: provider.id, size: 34)
            Text(provider.name).font(.body).lineLimit(1).layoutPriority(1)
            Spacer(minLength: 10)
            if busy { ProgressView() }
            else if let action { Text(action).font(.body.weight(.medium)).foregroundStyle(.blue) }
            else {
                if let detail { Text(detail).font(.subheadline).foregroundStyle(.secondary).lineLimit(1) }
                Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
            }
        }
        .contentShape(Rectangle())
    }
}

private struct McpRow: View {
    let connection: McpConnection
    let action: String?
    let busy: Bool

    var body: some View {
        HStack(spacing: 12) {
            ConnectorLogo(provider: "mcp", size: 34)
            Text(connection.name).font(.body).lineLimit(1)
            Spacer(minLength: 10)
            if busy { ProgressView() }
            else if connection.status == .disabled { Text("Disabled").foregroundStyle(.secondary) }
            else if let action { Text(action).font(.body.weight(.medium)).foregroundStyle(.blue) }
        }
        .contentShape(Rectangle())
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
        case "spotify": "music.note"
        case "soundcloud": "cloud.fill"
        case "mcp": "network"
        default: "link"
        }
    }
    private var foreground: Color {
        switch provider {
        case "google": .blue
        case "slack": .purple
        case "spotify": .green
        case "soundcloud": .orange
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

    func addMcp(_ target: String, using model: InboxModel) async -> Bool {
        guard operation == nil else { return false }
        operation = "mcp:add"
        error = nil
        defer { operation = nil }
        do {
            let connection = try await model.addMcpConnection(target)
            let completed = try await completeMcpConnection(connection, using: model)
            overview = try await model.connectorOverview()
            return completed
        } catch let authenticationError as ASWebAuthenticationSessionError
            where authenticationError.code == .canceledLogin {
            overview = try? await model.connectorOverview()
            return true
        } catch {
            self.error = error.localizedDescription
            overview = try? await model.connectorOverview()
            return false
        }
    }

    func connect(_ connection: McpConnection, using model: InboxModel) async {
        guard operation == nil else { return }
        operation = "mcp:" + connection.id
        error = nil
        defer { operation = nil }
        do {
            _ = try await completeMcpConnection(connection, using: model)
            overview = try await model.connectorOverview()
        } catch let authenticationError as ASWebAuthenticationSessionError
            where authenticationError.code == .canceledLogin {
            return
        } catch { self.error = error.localizedDescription }
    }

    func revoke(_ connection: McpConnection, using model: InboxModel) async {
        guard operation == nil else { return }
        operation = "mcp:" + connection.id
        error = nil
        defer { operation = nil }
        do {
            try await model.disconnectMcpConnection(connection.id)
            overview = try await model.connectorOverview()
        } catch { self.error = error.localizedDescription }
    }

    private func completeMcpConnection(
        _ connection: McpConnection,
        using model: InboxModel
    ) async throws -> Bool {
        switch try await model.beginMcpAuthorization(connection.id) {
        case .connected:
            return true
        case .authorization(let authorization):
            let callbackURL = try await authenticate(
                authorizationURL: authorization.authorizationURL,
                callbackURL: authorization.callbackURL,
                prefersEphemeral: false
            )
            switch try authorization.result(from: callbackURL) {
            case .connected:
                return true
            case .cancelled:
                return false
            case .failed:
                error = "\(connection.name) couldn’t be connected. Try again."
                return false
            }
        }
    }

    private func authenticate(
        _ authorization: ConnectorAuthorization,
        prefersEphemeral: Bool
    ) async throws -> URL {
        try await authenticate(
            authorizationURL: authorization.authorizationURL,
            callbackURL: authorization.callbackURL,
            prefersEphemeral: prefersEphemeral
        )
    }

    private func authenticate(
        authorizationURL: URL,
        callbackURL: URL,
        prefersEphemeral: Bool
    ) async throws -> URL {
        guard let scheme = callbackURL.scheme else { throw APIError.invalidResponse }
        authenticationSession?.cancel()
        return try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<URL, Error>) in
            let session = ASWebAuthenticationSession(
                url: authorizationURL,
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
