import InboxCore
import SwiftUI
import NanocodexRemote

struct SettingsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var switchingAccount = false
    @State var section: SettingsSection = .general
    enum SettingsSection: String, CaseIterable { case general = "General", appearance = "Appearance", shortcuts = "Shortcuts" }
    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Settings").font(.title2.weight(.semibold))
                Spacer()
                Button("Done") { dismiss() }.keyboardShortcut(.cancelAction)
            }.padding(20)
            Picker("Settings section", selection: $section) {
                ForEach(SettingsSection.allCases, id: \.self) { Text($0.rawValue).tag($0) }
            }.pickerStyle(.segmented).labelsHidden().padding(.horizontal, 24)
            Group {
                switch section {
                case .general: general
                case .appearance: appearance
                case .shortcuts: shortcuts
                }
            }.padding(.bottom, 12)
        }.frame(width: 600, height: 580)
            .background(Color(nsColor: .windowBackgroundColor))
            .sheet(isPresented: $switchingAccount) {
                SignInView(isSwitchingAccount: model.state.connected, onClose: { switchingAccount = false })
                    .padding(36).frame(width: 480, height: 640)
            }
    }
    private var general: some View {
        Form {
            Section("Account") {
                if model.state.connected {
                    LabeledContent("Nanocodex account") { Label("Connected", systemImage: "checkmark.circle.fill").foregroundStyle(.green) }
                    Button("Manage account and connections") { model.openAccount() }.buttonStyle(.link)
                    Button("Manage ChatGPT accounts…") { model.openAccount(chatGpt: true) }
                        .buttonStyle(.link).accessibilityIdentifier("chatgpt-accounts")
                    Text("Add accounts and view account status in your browser. Use the same Nanocodex account as this app.")
                        .font(.caption).foregroundStyle(.secondary)
                    HStack {
                        Button("Switch Account…") { switchingAccount = true }.accessibilityIdentifier("switch-account")
                        Button("Sign Out") { Task { await model.disconnect() } }
                    }
                } else {
                    Button("Sign In…") { switchingAccount = true }.buttonStyle(.borderedProminent)
                }
            }
            Section("Background Hands") {
                LaunchAtLoginSettings(launch: model.launchAtLogin)
                Toggle("Make this Mac available as a Hand", isOn: Binding(get: { model.state.defaultHandEnabled != false }, set: { enabled in Task { await model.setDeviceHandEnabled(enabled) } }))
                    .disabled(!model.state.connected).accessibilityIdentifier("device-hand-enabled")
                MacScreenSharingSettings(host: model.remoteMacHost).disabled(!model.state.connected)
                Toggle("Keep Mac awake while Hands are running", isOn: $model.keepMacAwake).accessibilityIdentifier("keep-mac-awake")
                Text("Hands keep running when you close the window. Open Nanocodex or quit from the menu bar.")
                    .font(.caption).foregroundStyle(.secondary)
                Text("Keeping awake prevents idle sleep and uses more battery. The display can turn off; closing the lid or choosing Sleep can still suspend Hands.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }.formStyle(.grouped)
    }
    private var appearance: some View {
        Form {
            Section("Appearance") {
                Picker("Theme", selection: $model.theme) {
                    Text("System").tag("system"); Text("Light").tag("light"); Text("Dark").tag("dark")
                }.pickerStyle(.segmented).onChange(of: model.theme) { model.persistLayout() }
                Text("System follows your Mac’s appearance. Transparency, contrast, and motion follow your accessibility settings.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section("Workspace") {
                Picker("Tab layout", selection: Binding(get: { model.tabPosition }, set: model.setTabPosition)) {
                    Text("Horizontal").tag("top")
                    Text("Vertical sidebar").tag("left")
                }.accessibilityIdentifier("tab-layout-preference")
                LabeledContent("Zoom") {
                    HStack {
                        Button("Zoom out", systemImage: "minus") { model.changeZoom(-1) }.labelStyle(.iconOnly).disabled(model.workspaceZoom <= 0.75)
                        Text(model.workspaceZoom, format: .percent.precision(.fractionLength(0))).monospacedDigit().frame(minWidth: 44)
                        Button("Zoom in", systemImage: "plus") { model.changeZoom(1) }.labelStyle(.iconOnly).disabled(model.workspaceZoom >= 1.5)
                        Button("Reset") { model.resetZoom() }.disabled(model.workspaceZoom == 1)
                    }
                }
            }
        }.formStyle(.grouped)
    }
    private var shortcuts: some View {
        Form {
            Section("Conversations") {
                shortcut("New / Reopen closed tab", keys: "⌘ T / ⌘ ⇧ T")
                shortcut("Back / Forward", keys: "⌘ [ / ⌘ ]")
                shortcut("Tab overview", keys: "⌘ ⇧ O")
                shortcut("Find a conversation", keys: "⌘ K")
                shortcut("Remote screens", keys: "⌘ ⌥ S")
                shortcut("Send / New line", keys: "↩ / ⇧ ↩")
            }
            Section("Workspace navigation") {
                Text("Press Esc to leave the composer. Press Enter to write again.")
                    .font(.caption).foregroundStyle(.secondary)
                shortcut("Split right / below", keys: "v / h")
                shortcut("Move between panes", keys: "Arrows / Tab / ⇧ Tab")
                shortcut("Resize pane", keys: "⇧ H J K L")
                shortcut("Focus / Restore layout", keys: "z")
                shortcut("Close pane", keys: "x")
                shortcut("Move pane left / right", keys: "{ / }")
                shortcut("Seen / Later", keys: "⌘ D / ⌘ ⇧ D")
                shortcut("All workspace shortcuts", keys: "?")
            }
        }.formStyle(.grouped)
    }
    private func shortcut(_ title: String, keys: String) -> some View {
        LabeledContent(title) { Text(keys).font(.system(.caption, design: .monospaced)).foregroundStyle(.secondary) }
    }
}

private struct LaunchAtLoginSettings: View {
    @ObservedObject var launch: LaunchAtLogin
    var body: some View {
        Toggle("Open Nanocodex at login", isOn: Binding(get: { launch.isEnabled }, set: { launch.setEnabled($0) }))
            .disabled(!launch.isAvailable).accessibilityIdentifier("launch-at-login")
            .onAppear { launch.refresh() }
            .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in launch.refresh() }
        Text(launch.statusDescription).font(.caption).foregroundStyle(.secondary)
            .accessibilityIdentifier("launch-at-login-status")
        if let error = launch.error {
            Text(error).font(.caption).foregroundStyle(.red)
        }
        if launch.isAvailable {
            Button(launch.requiresApproval ? "Approve in Login Items…" : "Open Login Items…") { launch.openLoginItems() }
                .accessibilityIdentifier("open-login-items")
        }
    }
}

private struct MacScreenSharingSettings: View {
    @ObservedObject var host: RemoteMacHost
    var body: some View {
        Toggle("Share this Mac’s screen automatically", isOn: Binding(
            get: { host.automaticSharingEnabled },
            set: { enabled in Task { await host.setAutomaticSharingEnabled(enabled) } }
        )).accessibilityIdentifier("automatic-screen-sharing")
        Text("Your selected display is shared when you sign in and stays available after closing the window.")
            .font(.caption).foregroundStyle(.secondary)
        if host.automaticSharingEnabled {
            Text(host.status).font(.caption).foregroundStyle(.secondary)
            Button("Screen and control permissions…") {
                _ = MacScreen.requestScreenPermission()
                _ = MacScreen.requestInputPermission()
            }
        }
    }
}

struct OnboardingView: View {
    var body: some View {
        VStack {
            Spacer(minLength: 25)
            SignInView().frame(width: 360)
            Spacer(minLength: 25)
        }.frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Color(nsColor: .textBackgroundColor))
    }
}

/// Account credentials are handled by AppModel; this form holds only the phone and code.
struct SignInView: View {
    @EnvironmentObject private var model: AppModel
    var isSwitchingAccount = false
    var onClose: (() -> Void)?
    @State private var phone = ""
    @State private var code = ""
    private var challenge: SignInChallenge? { model.phoneSignInChallenge }
    @State private var busy = false
    @State private var operationError: String?
    @State private var advanced = false
    @State private var apiKey = ""
    @State private var baseUrl = ""
    @FocusState private var focused: Field?
    private enum Field { case phone, code }

    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            HStack {
                Image(nsImage: NSImage(named: "icon") ?? NSImage()).resizable().frame(width: 48, height: 48)
                Spacer()
                if let onClose {
                    Button("Cancel") { perform { try await model.cancelPhoneSignIn(); onClose() } }
                        .disabled(busy).keyboardShortcut(.cancelAction)
                }
            }
            VStack(alignment: .leading, spacing: 8) {
                Text(challenge == nil ? (isSwitchingAccount ? "Switch account" : "Welcome to Nanocodex") : "Check your messages")
                    .font(.system(size: 28, weight: .semibold)).tracking(-0.5)
                Text(challenge.map { "Enter the six-digit code sent to \($0.phone)." } ?? "Sign in with your phone number to start building.")
                    .font(.system(size: 14)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            if let challenge {
                VStack(alignment: .leading, spacing: 13) {
                    TextField("6-digit code", text: $code)
                        .textContentType(.oneTimeCode).textFieldStyle(.roundedBorder)
                        .font(.system(size: 23, design: .monospaced)).controlSize(.large)
                        .focused($focused, equals: .code).accessibilityIdentifier("sign-in-code")
                        .onChange(of: code) { code = SignInChallenge.normalizedCode(code) }
                        .onSubmit { verify() }.disabled(busy)
                    primaryButton("Verify and Sign In", enabled: code.count == 6) { verify() }
                    TimelineView(.periodic(from: .now, by: 1)) { context in
                        let remaining = challenge.resendSeconds(at: context.date)
                        HStack {
                            Button("Change phone number") { resetPhone() }.buttonStyle(.link).disabled(busy)
                            Spacer()
                            Button(remaining > 0 ? "Resend in \(remaining)s" : "Resend code") { sendCode() }
                                .buttonStyle(.link).disabled(busy || remaining > 0).accessibilityIdentifier("resend-code")
                        }.font(.system(size: 12))
                        if challenge.isExpired(at: context.date) {
                            Text("Your code may have expired. Request another code if needed.").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            } else {
                VStack(alignment: .leading, spacing: 12) {
                    TextField("Phone number", text: $phone, prompt: Text("+1 415 555 0123"))
                        .textContentType(.telephoneNumber).textFieldStyle(.roundedBorder).controlSize(.large)
                        .focused($focused, equals: .phone).accessibilityIdentifier("sign-in-phone")
                        .onSubmit { sendCode() }.disabled(busy)
                    Text("Include your country code. We’ll text you a one-time code.").font(.caption).foregroundStyle(.secondary)
                    primaryButton("Send Code", enabled: !phone.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty) { sendCode() }
                }
            }
            if let operationError {
                Label(operationError, systemImage: "exclamationmark.circle").font(.system(size: 12)).foregroundStyle(.orange)
                    .textSelection(.enabled).fixedSize(horizontal: false, vertical: true).accessibilityIdentifier("sign-in-error")
            }
            Text("You’ll stay signed in securely with macOS Keychain.").font(.system(size: 12)).foregroundStyle(.secondary)
            if challenge == nil {
                DisclosureGroup("Advanced · Use an API key", isExpanded: $advanced) {
                    VStack(alignment: .leading, spacing: 11) {
                        SecureField("Nanocodex API key", text: $apiKey).accessibilityIdentifier("api-key")
                        TextField("Service URL", text: $baseUrl).textContentType(.URL)
                        Button("Connect with API Key") {
                            perform {
                                try await model.cancelPhoneSignIn()
                                try await model.connect(baseUrl: baseUrl, key: apiKey.trimmingCharacters(in: .whitespacesAndNewlines), remember: true)
                                apiKey = ""; onClose?()
                            }
                        }.disabled(apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || busy).accessibilityIdentifier("connect-account")
                    }.textFieldStyle(.roundedBorder).padding(.top, 9)
                }.font(.system(size: 12)).foregroundStyle(.secondary).disabled(busy)
            }
        }
        .interactiveDismissDisabled(busy || model.phoneSignInActive)
        .onAppear { if baseUrl.isEmpty { baseUrl = model.state.baseUrl }; if let challenge { phone = challenge.phone }; focused = challenge == nil ? .phone : .code }
        .accessibilityIdentifier("sign-in-view")
    }

    private func primaryButton(_ title: String, enabled: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 9) { if busy { ProgressView().controlSize(.small) }; Text(busy ? "Please wait…" : title).font(.system(size: 14, weight: .medium)) }
                .frame(maxWidth: .infinity).padding(.vertical, 6)
        }.buttonStyle(.borderedProminent).tint(.primary).controlSize(.large).disabled(!enabled || busy)
            .keyboardShortcut(.defaultAction).accessibilityIdentifier(challenge == nil ? "send-sign-in-code" : "verify-sign-in-code")
    }
    private func perform(_ operation: @escaping @MainActor () async throws -> Void) {
        guard !busy else { return }
        busy = true; operationError = nil
        Task { @MainActor in
            do { try await operation() }
            catch { operationError = error.localizedDescription }
            busy = false
        }
    }
    private func sendCode() {
        guard !phone.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        perform {
            let next = try await model.startPhoneSignIn(phone: phone, baseUrl: baseUrl)
            phone = next.phone; code = ""; focused = .code
        }
    }
    private func verify() {
        guard code.count == 6 else { return }
        perform { try await model.finishPhoneSignIn(code: code); code = ""; onClose?() }
    }
    private func resetPhone() {
        perform { try await model.cancelPhoneSignIn(); code = ""; focused = .phone }
    }
}

struct MacScheduledJobsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var jobs: [ScheduledJob] = []
    @State private var selected: ScheduledJob?
    @State private var loading = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            List {
                Text("Select a job to edit, pause, or cancel it. Ask an agent in chat to create a new job.")
                    .foregroundStyle(.secondary)
                if loading { ProgressView("Loading scheduled jobs") }
                if let error { Text(error).foregroundStyle(.red) }
                if !loading && jobs.isEmpty && error == nil { Text("No scheduled jobs yet") }
                ForEach(jobs) { job in
                    Button { selected = job } label: {
                        VStack(alignment: .leading) {
                            Text(job.triggerID).font(.headline)
                            Text(job.input).lineLimit(2)
                            Text("Source: " + job.agentID).font(.caption).foregroundStyle(.secondary)
                            Text("\(job.enabled ? "Active" : "Paused") · \(job.cron) · \(job.timezone)").font(.caption)
                        }
                    }.buttonStyle(.plain).disabled(loading)
                }
            }
            .navigationTitle("Scheduled jobs")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } }
                ToolbarItem { Button("Refresh", systemImage: "arrow.clockwise") { Task { await refresh() } }.disabled(loading) }
            }
            .sheet(item: $selected) { job in
                ScheduledJobEditor(job: job) { cron, timezone, input, enabled, startsNew in
                    let client = try model.schedulesClient(); defer { client.close() }
                    let updated = try await client.updateScheduledJob(job, cron: cron, timezone: timezone, input: input,
                                                                     enabled: enabled, startsNewConversation: startsNew)
                    jobs = jobs.map { $0.id == updated.id ? updated : $0 }
                } cancel: {
                    let client = try model.schedulesClient(); defer { client.close() }
                    try await client.cancelScheduledJob(job)
                    jobs.removeAll { $0.id == job.id }
                }
            }
        }.frame(minWidth: 560, minHeight: 480).task { await refresh() }
    }

    @MainActor private func refresh() async {
        guard !loading else { return }
        loading = true; error = nil
        defer { loading = false }
        do {
            let client = try model.schedulesClient(); defer { client.close() }
            let agents = try await client.list()
            try Task.checkCancellation()
            let owners = Set(agents.map(\.id))
            jobs.removeAll { !owners.contains($0.agentID) }
            let cachedOwners = Set(jobs.map(\.agentID))
            let candidates = agents.filter { $0.mayHaveScheduledJobs || cachedOwners.contains($0.id) }
            await client.scheduledJobs(for: candidates.map(\.id)) { owner, result in
                await MainActor.run {
                    switch result {
                    case .success(let received):
                        jobs = (jobs.filter { $0.agentID != owner } + received).sorted { $0.id < $1.id }
                    case .failure:
                        self.error = "Some jobs could not be loaded. Refresh to try again."
                    }
                }
            }
        } catch { self.error = error.localizedDescription }
    }
}
