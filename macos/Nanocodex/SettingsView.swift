import SwiftUI

struct SettingsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var switchingAccount = false
    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            HStack {
                Image(nsImage: NSImage(named: "icon") ?? NSImage()).resizable().frame(width: 34, height: 34)
                Text("Settings").font(.system(size: 23, weight: .semibold))
                Spacer()
                Button("Done") { dismiss() }.keyboardShortcut(.cancelAction)
            }
            Form {
                Section("Account") {
                    if model.state.connected {
                        LabeledContent("Nanocodex account") { Label("Connected", systemImage: "checkmark.circle.fill").foregroundStyle(.green) }
                        Button("Manage account and connections") { model.openAccount() }.buttonStyle(.link)
                        HStack {
                            Button("Switch Account…") { switchingAccount = true }.accessibilityIdentifier("switch-account")
                            Button("Sign Out") { Task { await model.disconnect() } }.foregroundStyle(.secondary)
                        }
                    } else {
                        Button("Sign In…") { switchingAccount = true }.buttonStyle(.borderedProminent).tint(.primary)
                    }
                }
                Section("Appearance") {
                    Picker("Tabs", selection: $model.tabPosition) { Text("Sidebar").tag("left"); Text("Top").tag("top") }.pickerStyle(.segmented).onChange(of: model.tabPosition) { model.persistLayout() }
                    Picker("Theme", selection: $model.theme) { Text("System").tag("system"); Text("Light").tag("light"); Text("Dark").tag("dark") }.onChange(of: model.theme) { model.persistLayout() }
                }
                Section("Keyboard shortcuts") {
                    shortcut("New tab", keys: "⌘ T")
                    shortcut("Find a thread", keys: "⌘ K")
                    shortcut("Send / New line", keys: "↩ / ⇧ ↩")
                    shortcut("Reopen closed tab", keys: "⌘ ⇧ T")
                }
            }.formStyle(.grouped)
            HStack { Text("Nanocodex \(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.1.0")"); Spacer(); Text("Made for macOS") }.font(.system(size: 11)).foregroundStyle(.tertiary)
        }.padding(24).frame(width: 575, height: 560).background(Color(nsColor: .windowBackgroundColor))
            .sheet(isPresented: $switchingAccount) {
                SignInView(isSwitchingAccount: model.state.connected, onClose: { switchingAccount = false })
                    .padding(36).frame(width: 480, height: 640)
            }
    }
    private func shortcut(_ title: String, keys: String) -> some View {
        LabeledContent(title) { Text(keys).font(.system(size: 11, weight: .medium, design: .monospaced)).foregroundStyle(.secondary).padding(.horizontal, 7).padding(.vertical, 4).background(Color.primary.opacity(0.045), in: RoundedRectangle(cornerRadius: 5)) }
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
