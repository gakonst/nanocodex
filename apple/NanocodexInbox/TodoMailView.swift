import SwiftUI
import InboxCore
import NanocodexUI
import WebKit
import QuickLook

/// Reusable reader. Pass a synthetic fixture with a nil client for UI journeys.
struct TodoMailThreadView: View {
    @StateObject private var store: TodoMailSession
    private let replyMessageID: String?
    @State private var expanded: Set<String> = []
    @State private var composing = false
    @State private var previewURL: URL?
    @State private var downloading: String?
    @State private var attachmentError: String?

    init(client: ManagedClient?, connectionID: String, threadID: String, replyMessageID: String? = nil, fixture: TodoMailThread? = nil) {
        self.replyMessageID = replyMessageID
        _store = StateObject(wrappedValue: TodoMailSession(client: client, connectionID: connectionID, threadID: threadID, fixture: fixture))
    }
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                if let thread = store.thread {
                    Text(thread.subject.isEmpty ? "No subject" : thread.subject)
                        .font(.system(size: 27, weight: .bold)).tracking(-0.7)
                        .textSelection(.enabled).accessibilityIdentifier("mail-thread-subject")
                    HStack {
                        Text("\(thread.messages.count) messages").font(.caption).foregroundStyle(.secondary)
                        Spacer()
                        Button(expanded.count == thread.messages.count ? "Collapse all" : "Expand all") {
                            expanded = expanded.count == thread.messages.count ? [] : Set(thread.messages.map(\.id))
                        }.font(.caption.weight(.semibold)).accessibilityIdentifier("mail-expand-all")
                    }
                    ForEach(thread.messages) { message in messageCard(message) }
                    if let draft = store.draft, draft.status != "sent" {
                        Button { composing = true } label: {
                            HStack {
                                Image(systemName: "square.and.pencil")
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(draft.isLocked ? "Check send status" : "Continue draft").font(.subheadline.weight(.semibold))
                                    Text(store.saveLabel).font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer(); Image(systemName: "chevron.right").font(.caption)
                            }.padding(14).background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 14))
                        }.buttonStyle(.plain).accessibilityIdentifier("mail-continue-draft")
                    }
                } else if store.loading {
                    ProgressView("Loading conversation…").frame(maxWidth: .infinity).padding(.top, 80)
                } else {
                    ContentUnavailableView("Conversation unavailable", systemImage: "envelope", description: Text(store.error ?? "Try loading this conversation again."))
                    Button("Try again") { Task { await store.load() } }.accessibilityIdentifier("mail-retry")
                }
                if let error = attachmentError { Text(error).font(.footnote).foregroundStyle(.red).accessibilityIdentifier("mail-attachment-error") }
                if let error = store.error, store.thread != nil { Text(error).font(.footnote).foregroundStyle(.red) }
            }.padding(18)
        }
        .background(ChatPalette.background)
        .navigationTitle("Conversation").navigationBarTitleDisplayMode(.inline)
        .accessibilityIdentifier("mail-thread-reader")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button { composing = store.begin(mode: .compose) } label: { Image(systemName: "square.and.pencil") }
                    .accessibilityLabel("Compose email").accessibilityIdentifier("mail-compose")
                    .disabled(!store.canBegin)
            }
        }
        .safeAreaInset(edge: .bottom) {
            if let message = store.thread?.messages.first(where: { $0.id == replyMessageID }) ?? store.thread?.messages.last {
                HStack(spacing: 10) {
                    draftButton(.reply, symbol: "arrowshape.turn.up.left", message: message)
                    draftButton(.replyAll, symbol: "arrowshape.turn.up.left.2", message: message)
                    draftButton(.forward, symbol: "arrowshape.turn.up.right", message: message)
                }.padding(.horizontal, 16).padding(.vertical, 10).background(.ultraThinMaterial)
            }
        }
        .task {
            await store.load()
            if let last = store.thread?.messages.first(where: { $0.id == replyMessageID }) ?? store.thread?.messages.last { expanded.insert(last.id) }
        }
        .refreshable { await store.load() }
        .sheet(isPresented: $composing) { TodoMailEditor(store: store) }
        .quickLookPreview($previewURL)
        .onChange(of: previewURL) { previous, current in
            if let previous, previous != current { try? FileManager.default.removeItem(at: previous.deletingLastPathComponent()) }
        }
    }
    private func draftButton(_ mode: TodoMailDraftMode, symbol: String, message: TodoMailMessage) -> some View {
        Button { composing = store.begin(mode: mode, message: message) } label: {
            Label(mode.title, systemImage: symbol).font(.caption.weight(.semibold)).frame(maxWidth: .infinity, minHeight: 44)
        }.buttonStyle(.bordered).tint(mode == .reply ? .orange : .secondary)
            .disabled(!store.canBegin)
            .accessibilityIdentifier("mail-\(mode.rawValue)")
    }
    private func messageCard(_ message: TodoMailMessage) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Button {
                if expanded.contains(message.id) { expanded.remove(message.id) } else { expanded.insert(message.id) }
            } label: {
                HStack(alignment: .top, spacing: 10) {
                    Text(String(message.from.prefix(1)).uppercased()).font(.headline)
                        .frame(width: 34, height: 34).background(Color.orange.opacity(0.12), in: Circle())
                    VStack(alignment: .leading, spacing: 4) {
                        Text(message.from.isEmpty ? "Unknown sender" : message.from).font(.subheadline.weight(.semibold))
                            .foregroundStyle(.primary).multilineTextAlignment(.leading)
                        Text(message.date).font(.caption2).foregroundStyle(.secondary).multilineTextAlignment(.leading)
                    }
                    Spacer(minLength: 0)
                    Image(systemName: expanded.contains(message.id) ? "chevron.up" : "chevron.down").font(.caption).foregroundStyle(.secondary)
                }.contentShape(Rectangle())
            }.buttonStyle(.plain).accessibilityIdentifier("mail-message:\(message.id)")
            if expanded.contains(message.id) {
                VStack(alignment: .leading, spacing: 3) {
                    addressLine("To", message.to)
                    if !message.cc.isEmpty { addressLine("Cc", message.cc) }
                    if !message.bcc.isEmpty { addressLine("Bcc", message.bcc) }
                }.textSelection(.enabled)
                Divider()
                if !message.bodyText.isEmpty {
                    Text(message.bodyText).font(.body).lineSpacing(4).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading).accessibilityIdentifier("mail-body:\(message.id)")
                } else if !message.bodyHTML.isEmpty {
                    TodoMailSafeHTML(html: message.bodyHTML).frame(minHeight: 380)
                        .accessibilityIdentifier("mail-html:\(message.id)")
                    Text("Remote images and interactive content are blocked.").font(.caption2).foregroundStyle(.secondary)
                } else {
                    Text("This message has no readable body.").font(.body).foregroundStyle(.secondary)
                }
                if message.bodyTruncated {
                    Text("Some message content could not be loaded. Refresh to try again.").font(.footnote).foregroundStyle(.orange)
                        .accessibilityIdentifier("mail-body-incomplete")
                }
                ForEach(message.attachments) { attachment in
                    Button {
                        Task {
                            downloading = attachment.id; attachmentError = nil
                            defer { downloading = nil }
                            do { previewURL = try await store.attachment(attachment, messageID: message.id) }
                            catch { attachmentError = error.localizedDescription }
                        }
                    } label: {
                        HStack {
                            Image(systemName: "paperclip")
                            VStack(alignment: .leading, spacing: 3) {
                                Text(attachment.filename).font(.subheadline).lineLimit(2)
                                Text(ByteCountFormatter.string(fromByteCount: Int64(attachment.size), countStyle: .file)).font(.caption2).foregroundStyle(.secondary)
                            }
                            Spacer()
                            if downloading == attachment.id { ProgressView() } else { Image(systemName: "arrow.down.circle") }
                        }.padding(10).background(ChatPalette.background, in: RoundedRectangle(cornerRadius: 10))
                    }.buttonStyle(.plain).disabled(downloading != nil)
                        .accessibilityLabel("Open or save \(attachment.filename)")
                        .accessibilityIdentifier("mail-attachment:\(attachment.id)")
                }
            } else {
                Text(message.bodyText).font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
            }
        }.padding(14).frame(maxWidth: .infinity, alignment: .leading)
            .background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 16))
    }
    private func addressLine(_ label: String, _ value: String) -> some View {
        Text(label + ": " + value).font(.caption).foregroundStyle(.secondary)
    }
}

struct TodoMailComposeView: View {
    @StateObject private var store: TodoMailSession
    init(client: ManagedClient?, connectionID: String, draftID: String? = nil, fixture: Bool = false) {
        _store = StateObject(wrappedValue: TodoMailSession(client: client, connectionID: connectionID, draftID: draftID, fixtureMode: fixture))
    }
    var body: some View {
        TodoMailEditor(store: store).task {
            await store.load()
            if store.draft == nil || store.draft?.status == "sent" { store.begin(mode: .compose) }
        }
    }
}

private struct TodoMailEditor: View {
    @ObservedObject var store: TodoMailSession
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var reloadConfirmation = false
    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    if let draft = store.draft {
                        if store.thread != nil {
                            Picker("Message type", selection: Binding(get: { draft.mode }, set: { store.changeMode($0) })) {
                                ForEach(TodoMailDraftMode.allCases, id: \.self) { Text($0.title).tag($0) }
                            }.pickerStyle(.menu).font(.subheadline).padding(.vertical, 6)
                                .accessibilityIdentifier("mail-draft-mode")
                        }
                        HStack(alignment: .firstTextBaseline) {
                            Text("From").font(.subheadline).foregroundStyle(.secondary).frame(width: 58, alignment: .leading)
                            Text(store.senderAddress.isEmpty ? "Sender unavailable · refresh the conversation" : store.senderAddress)
                                .font(.subheadline).textSelection(.enabled)
                        }.padding(.vertical, 12).accessibilityIdentifier("mail-draft-from")
                        Divider()
                        recipientField("To", keyPath: \.to)
                        recipientField("Cc", keyPath: \.cc)
                        recipientField("Bcc", keyPath: \.bcc)
                        HStack {
                            Text("Subject").font(.subheadline).foregroundStyle(.secondary).frame(width: 58, alignment: .leading)
                            TextField("Subject", text: binding(\.subject)).font(.subheadline)
                                .accessibilityIdentifier("mail-draft-subject")
                        }.padding(.vertical, 12)
                        Divider()
                        if draft.mode == .reply || draft.mode == .replyAll {
                            Button { Task { await store.suggestReply() } } label: {
                                HStack {
                                    if store.suggesting { ProgressView().controlSize(.mini) }
                                    Label("Draft for me", systemImage: "sparkles")
                                }
                            }.disabled(!store.canSuggest).padding(.top, 12)
                                .accessibilityIdentifier("mail-draft-suggest")
                            Text("Creates an editable suggestion when the message body is empty.")
                                .font(.caption).foregroundStyle(.secondary).padding(.top, 4)
                        }
                        TextEditor(text: binding(\.bodyText)).frame(minHeight: 280)
                            .scrollContentBackground(.hidden).padding(.top, 10)
                            .accessibilityLabel("Message body").accessibilityIdentifier("mail-draft-body")
                        if draft.mode == .forward {
                            Text("Original attachments are available in the conversation. Forwarding includes the message text.")
                                .font(.caption).foregroundStyle(.secondary).padding(.vertical, 8)
                        }
                        HStack(spacing: 6) {
                            if store.saving { ProgressView().controlSize(.mini) }
                            Image(systemName: draft.status == "sent" ? "checkmark.circle.fill" : "lock.shield")
                            Text(store.saveLabel)
                        }.font(.caption).foregroundStyle(.secondary).padding(.vertical, 12)
                            .accessibilityIdentifier("mail-draft-status")
                    } else if store.error == nil {
                        ProgressView("Loading draft…").padding(40)
                    } else {
                        Button("Try again") {
                            Task {
                                await store.load()
                                if store.draft == nil || store.draft?.status == "sent" { store.begin(mode: .compose) }
                            }
                        }.accessibilityIdentifier("mail-retry-draft").padding(.vertical, 12)
                    }
                    if let error = store.error {
                        Text(error).font(.footnote).foregroundStyle(.red).padding(.vertical, 8)
                            .accessibilityIdentifier("mail-draft-error")
                    }
                    if store.conflicted {
                        Button("Reload server draft…") { reloadConfirmation = true }
                            .accessibilityIdentifier("mail-reload-draft").padding(.vertical, 8)
                    }
                }.padding(.horizontal, 18)
                    .disabled(store.sending || store.draft?.isLocked == true)
                // A status check is read-only and must remain available for a locked draft.
                if store.draft?.isLocked == true {
                    Button("Refresh status") { Task { await store.refreshSendStatus() } }
                        .accessibilityIdentifier("mail-refresh-send-status").padding()
                }
            }.background(ChatPalette.background)
                .navigationTitle(store.draft?.mode.title ?? "Draft").navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .topBarLeading) {
                        Button("Done") { Task { await store.flush(); dismiss() } }
                            .disabled(store.sending).accessibilityIdentifier("mail-draft-done")
                    }
                    ToolbarItem(placement: .topBarTrailing) {
                        Button { Task { await store.send() } } label: {
                            if store.sending { ProgressView() } else { Text("Send").fontWeight(.semibold) }
                        }.disabled(!store.canSend).accessibilityLabel("Send email").accessibilityIdentifier("mail-send")
                    }
                }
                .confirmationDialog("Replace this device’s edits with the server draft?", isPresented: $reloadConfirmation, titleVisibility: .visible) {
                    Button("Reload server draft", role: .destructive) { Task { await store.reloadServerDraft() } }
                }
                .interactiveDismissDisabled(store.sending)
                .onChange(of: scenePhase) { _, phase in if phase != .active { Task { await store.flush() } } }
                .onDisappear { Task { await store.flush() } }
        }.tint(.orange).accessibilityIdentifier("mail-draft-editor")
    }
    private func recipientField(_ title: String, keyPath: WritableKeyPath<TodoMailDraft, [String]>) -> some View {
        VStack(spacing: 0) {
            HStack(alignment: .firstTextBaseline) {
                Text(title).font(.subheadline).foregroundStyle(.secondary).frame(width: 58, alignment: .leading)
                TextField("name@example.com", text: Binding(get: { store.draft?[keyPath: keyPath].joined(separator: ", ") ?? "" }, set: { value in
                    store.edit { $0[keyPath: keyPath] = value.components(separatedBy: ",").map { $0.trimmingCharacters(in: .whitespacesAndNewlines) } }
                }), axis: .vertical)
                .font(.subheadline).keyboardType(.emailAddress).textInputAutocapitalization(.never).autocorrectionDisabled()
                .accessibilityIdentifier("mail-draft-\(title.lowercased())")
            }.padding(.vertical, 12)
            Divider()
        }
    }
    private func binding(_ keyPath: WritableKeyPath<TodoMailDraft, String>) -> Binding<String> {
        Binding(get: { store.draft?[keyPath: keyPath] ?? "" }, set: { value in store.edit { $0[keyPath: keyPath] = value } })
    }
}

/// The reader has no bridge, account cookies, JavaScript, external resources, or navigation.
private struct TodoMailSafeHTML: UIViewRepresentable {
    let html: String
    func makeCoordinator() -> Coordinator { Coordinator() }
    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.defaultWebpagePreferences.allowsContentJavaScript = false
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = context.coordinator
        view.isOpaque = false; view.backgroundColor = .clear
        view.scrollView.backgroundColor = .clear
        view.allowsLinkPreview = false
        return view
    }
    func updateUIView(_ view: WKWebView, context: Context) {
        guard context.coordinator.loadedHTML != html else { return }
        context.coordinator.loadedHTML = html
        let content = html
        WKContentRuleListStore.default().compileContentRuleList(forIdentifier: "TodoMailBlockAllNetwork-v1", encodedContentRuleList: "[{\"trigger\":{\"url-filter\":\".*\"},\"action\":{\"type\":\"block\"}}]") { rules, _ in
            guard let rules, context.coordinator.loadedHTML == content else { return }
            view.configuration.userContentController.add(rules)
            view.loadHTMLString("""
            <!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src 'none'; font-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; media-src 'none'; form-action 'none'; base-uri 'none'"><style>:root{color-scheme:light dark}body{font:17px -apple-system;margin:0;overflow-wrap:anywhere}pre{white-space:pre-wrap}img,iframe,object,embed,form{display:none!important}a{pointer-events:none;color:inherit}</style></head><body>\(content)</body></html>
            """, baseURL: nil)
        }
    }
    final class Coordinator: NSObject, WKNavigationDelegate {
        var loadedHTML: String?
        func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction, decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
            // Only the locally supplied initial document is admitted. Redirects,
            // custom schemes, user links, frames, downloads, and form submits fail closed.
            let initial = navigationAction.navigationType == .other && navigationAction.request.url?.absoluteString == "about:blank" && navigationAction.targetFrame?.isMainFrame == true
            decisionHandler(initial ? .allow : .cancel)
        }
    }
}

/// Edits are protected on disk before scheduling network I/O. A server conflict
/// never replaces those edits implicitly. Sending freezes the exact saved version.
@MainActor
private final class TodoMailSession: ObservableObject {
    @Published var thread: TodoMailThread?
    @Published var draft: TodoMailDraft?
    @Published var loading = false
    @Published var saving = false
    @Published var sending = false
    @Published var suggesting = false
    @Published var conflicted = false
    @Published var error: String?
    @Published private var dirty = false
    private let client: ManagedClient?
    private let connectionID: String
    private let threadID: String?
    private let initialDraftID: String?
    private let fixtureMode: Bool
    private let recoveryURL: URL
    private var accountEmail = ""
    private var loaded = false
    private var attemptedMarkRead = false
    private var debounce: Task<Void, Never>?
    private var saveTask: Task<TodoMailDraft, Error>?
    private var sendOperation: UUID?
    private var editRevision = 0
    private var localPersistenceFailed = false
    private struct Recovery: Codable {
        var draft: TodoMailDraft
        var dirty: Bool
        var sendOperation: UUID?
    }

    init(client: ManagedClient?, connectionID: String, threadID: String? = nil, draftID: String? = nil,
         fixture: TodoMailThread? = nil, fixtureMode: Bool = false) {
        self.client = client; self.connectionID = connectionID; self.threadID = threadID
        let isFixture = fixture != nil || fixtureMode
        initialDraftID = draftID; self.fixtureMode = isFixture
        thread = fixture
        let fixtureProfile = Data((ProcessInfo.processInfo.environment["NANOCODEX_DEMO_PROFILE"] ?? "default").utf8).base64EncodedString()
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "+", with: "-")
        let scope = isFixture ? "fixture-" + fixtureProfile : (client?.todoMailStorageScope ?? "offline")
        let key = Data((connectionID + "\n" + (threadID ?? draftID ?? "compose")).utf8).base64EncodedString()
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "+", with: "-")
        let root = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("TodoMailDrafts", isDirectory: true).appendingPathComponent(scope, isDirectory: true)
        recoveryURL = root.appendingPathComponent(key + ".json")
        if let data = try? Data(contentsOf: recoveryURL), let recovery = try? JSONDecoder().decode(Recovery.self, from: data), recovery.draft.connectionID == connectionID {
            draft = recovery.draft; dirty = recovery.dirty; sendOperation = recovery.sendOperation
            if draft?.status == "sending" { draft?.status = "unknown" }
        }
    }
    var saveLabel: String {
        if localPersistenceFailed { return "Draft recovery could not be saved on this device" }
        if sending { return "Sending the reviewed version…" }
        switch draft?.status {
        case "sent": return fixtureMode ? "Fixture send complete · no email sent" : "Sent"
        case "unknown": return "Send outcome unknown · retry blocked"
        case "sending": return "Send pending · check status before continuing"
        default:
            if conflicted { return "Draft changed on another device · your edits are kept here" }
            if saving { return "Saving draft…" }
            if dirty { return "Saved on this device · awaiting server save" }
            return fixtureMode ? "Draft saved · fixture" : "Draft saved to your account"
        }
    }
    var canBegin: Bool { loaded && !loading && (draft?.isLocked != true || draft?.status == "sent") }
    var senderAddress: String { accountEmail }
    var canSend: Bool {
        guard let draft else { return false }
        return loaded && !accountEmail.isEmpty && !loading && !sending && !saving && !suggesting && !conflicted && !draft.isLocked && !localPersistenceFailed
            && !draft.to.filter({ !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }).isEmpty
    }
    var canSuggest: Bool {
        guard let draft else { return false }
        return loaded && !loading && !suggesting && !sending && !draft.isLocked && !conflicted && draft.bodyText.isEmpty
            && (draft.mode == .reply || draft.mode == .replyAll)
            && draft.threadID != nil && draft.replyMessageID != nil
    }
    func suggestReply() async {
        guard canSuggest, let snapshot = draft, let threadID = snapshot.threadID,
              let replyMessageID = snapshot.replyMessageID else { return }
        let revision = editRevision
        suggesting = true; error = nil
        defer { suggesting = false }
        do {
            let proposal: String
            if fixtureMode {
                proposal = "Thanks for the update. I’ll review the launch plan and follow up with my thoughts."
            } else {
                guard let client else { throw APIError.invalidCredential }
                proposal = try await client.suggestTodoMailReply(connectionID: connectionID, threadID: threadID, replyMessageID: replyMessageID)
            }
            guard draft?.id == snapshot.id, editRevision == revision, draft?.bodyText.isEmpty == true,
                  draft?.isLocked == false, !sending else {
                error = "The draft changed while the suggestion was being prepared. Your edits are preserved."
                return
            }
            edit { $0.bodyText = proposal }
        } catch {
            self.error = "A draft suggestion is unavailable right now. You can keep writing your reply. " + error.localizedDescription
        }
    }
    func load() async {
        guard !loading else { return }
        loading = true; defer { loading = false }
        if fixtureMode { accountEmail = "alex@example.com"; loaded = true; return }
        guard let client else { error = "Connect your account to open this message."; return }
        do {
            if let threadID {
                thread = try await client.todoMailThread(connectionID: connectionID, threadID: threadID)
                // This session exists because the user explicitly opened the thread.
                // Record admission before awaiting so refresh never retries the write.
                if !attemptedMarkRead {
                    attemptedMarkRead = true
                    do { try await client.modifyTodoMailThread(connectionID: connectionID, threadID: threadID, unread: false) }
                    catch { self.error = "The conversation opened, but marking it read could not be confirmed. " + error.localizedDescription }
                }
            }
            accountEmail = (try? await client.todoMailAccountEmail(connectionID: connectionID)) ?? ""
            if !loaded {
                let remote: TodoMailDraft?
                if let id = initialDraftID ?? draft?.id {
                    do { remote = try await client.todoMailDraft(id: id) }
                    catch APIError.http(404) where draft?.version == 0 { remote = nil }
                } else if let threadID {
                    remote = try await client.todoMailDrafts(connectionID: connectionID, threadID: threadID)
                        .first(where: { $0.threadID == threadID && $0.status != "sent" })
                } else { remote = nil }
                if let remote { mergeRemote(remote) }
                loaded = true
            }
            if dirty && !conflicted && draft?.isLocked != true { scheduleSave() }
        } catch { self.error = error.localizedDescription }
    }
    @discardableResult func begin(mode: TodoMailDraftMode, message: TodoMailMessage? = nil) -> Bool {
        guard canBegin else { return false }
        // Reopening an existing draft always retains its edits; changing modes
        // must never silently replace a draft the user has already started.
        if let draft, draft.status != "sent" { return true }
        var recipients: [String] = []
        var copied: [String] = []
        var subject = ""
        var body = ""
        if let message {
            if mode == .reply || mode == .replyAll {
                recipients = Self.addresses(message.replyTo.isEmpty ? message.from : message.replyTo)
                if mode == .replyAll {
                    guard !accountEmail.isEmpty else { error = "Your account address could not be loaded. Refresh before replying to everyone."; return false }
                    recipients += Self.addresses(message.to)
                    copied = Self.addresses(message.cc)
                }
                recipients = Self.unique(recipients, excluding: [accountEmail])
                copied = Self.unique(copied, excluding: recipients + [accountEmail])
                subject = message.subject.lowercased().hasPrefix("re:") ? message.subject : "Re: " + message.subject
            } else if mode == .forward {
                subject = message.subject.lowercased().hasPrefix("fwd:") ? message.subject : "Fwd: " + message.subject
                body = "\n\n---------- Forwarded message ----------\nFrom: \(message.from)\nDate: \(message.date)\nSubject: \(message.subject)\nTo: \(message.to)\n\n\(message.bodyText)"
            }
        }
        draft = TodoMailDraft(connectionID: connectionID, threadID: mode == .compose ? nil : threadID,
            replyMessageID: message?.id, mode: mode, to: recipients, cc: copied, subject: subject, bodyText: body)
        dirty = true; sendOperation = nil; error = nil; conflicted = false
        persist(); scheduleSave()
        return true
    }
    func changeMode(_ mode: TodoMailDraftMode) {
        guard let current = draft, current.mode != mode, !current.isLocked, !sending,
              let message = thread?.messages.last else { return }
        if mode == .replyAll && accountEmail.isEmpty {
            error = "Your account address could not be loaded. Refresh before replying to everyone."; return
        }
        edit { value in
            value.mode = mode
            value.threadID = mode == .compose ? nil : threadID
            value.replyMessageID = mode == .compose ? nil : message.id
            if mode == .forward {
                value.to = []; value.cc = []; value.bcc = []
                if !value.bodyText.contains("---------- Forwarded message ----------") {
                    value.bodyText += "\n\n---------- Forwarded message ----------\nFrom: \(message.from)\nDate: \(message.date)\nSubject: \(message.subject)\nTo: \(message.to)\n\n\(message.bodyText)"
                }
            } else if mode == .reply || mode == .replyAll {
                let sender = Self.addresses(message.replyTo.isEmpty ? message.from : message.replyTo)
                if mode == .reply {
                    value.to = Self.unique(sender, excluding: [accountEmail])
                    value.cc = []; value.bcc = []
                } else {
                    value.to = Self.unique(value.to + sender + Self.addresses(message.to), excluding: [accountEmail])
                    value.cc = Self.unique(value.cc + Self.addresses(message.cc), excluding: value.to + [accountEmail])
                }
            }
            let base = message.subject
            if [base, "Re: " + base, "Fwd: " + base].contains(value.subject) {
                value.subject = mode == .forward ? "Fwd: " + base : mode == .compose ? base : "Re: " + base
            }
        }
    }
    func edit(_ change: (inout TodoMailDraft) -> Void) {
        guard var current = draft, !current.isLocked, !sending else { return }
        change(&current); draft = current; dirty = true; editRevision += 1
        persist(); scheduleSave()
    }
    private func scheduleSave() {
        debounce?.cancel()
        debounce = Task { [weak self] in
            do { try await Task.sleep(for: .milliseconds(800)) } catch { return }
            await self?.flush()
        }
    }
    func flush() async {
        guard !sending else { return }
        _ = await saveCurrent()
    }
    @discardableResult private func saveCurrent() async -> Bool {
        // Await an admitted save without replaying it. Its owning invocation
        // merges the new version before another save can be admitted.
        if let saveTask {
            _ = try? await saveTask.value
            while saving { await Task.yield() }
        }
        guard dirty, let snapshot = draft else { return !conflicted }
        guard !snapshot.isLocked, !conflicted else { return false }
        let revision = editRevision
        saving = true
        let task = Task<TodoMailDraft, Error> {
            if fixtureMode {
                var saved = snapshot; saved.version += 1; return saved
            }
            guard let client else { throw APIError.invalidCredential }
            return try await client.saveTodoMailDraft(snapshot)
        }
        saveTask = task
        do {
            let saved = try await task.value
            if editRevision == revision { draft = saved; dirty = false }
            else { draft?.id = saved.id; draft?.version = saved.version }
            error = nil; persist()
        } catch {
            if (error as? APIError) == .http(409) {
                conflicted = true
                self.error = "This draft changed on the server. Your edits remain on this device. Reload the server draft to continue."
            } else { self.error = "Couldn’t save to your account. " + error.localizedDescription }
            persist()
        }
        saveTask = nil; saving = false
        if dirty && !conflicted && error == nil { return await saveCurrent() }
        return !dirty && !conflicted
    }
    func send() async {
        guard canSend else { return }
        // Freeze editing before waiting for the final save, then capture the
        // immutable draft/version and one durable operation ID for this send.
        sending = true; debounce?.cancel()
        guard await saveCurrent(), var snapshot = draft else { sending = false; return }
        let operation = sendOperation ?? UUID(); sendOperation = operation
        snapshot.status = "sending"; draft = snapshot; persist()
        guard !localPersistenceFailed else { draft?.status = "draft"; sending = false; return }
        do {
            if fixtureMode {
                #if DEBUG
                draft?.status = ProcessInfo.processInfo.arguments.contains("--todo-mail-unknown-fixture") ? "unknown" : "sent"
                #else
                draft?.status = "sent"
                #endif
            } else {
                guard let client else { throw APIError.invalidCredential }
                let receipt = try await client.sendTodoMailDraft(snapshot, operationID: operation)
                guard receipt.draftID == snapshot.id, receipt.operationID.lowercased() == operation.uuidString.lowercased() else { throw APIError.invalidResponse }
                draft?.status = receipt.status
            }
            error = draft?.status == "unknown" ? "The server could not confirm delivery. Sending again is blocked. Check the conversation or refresh send status." : nil
        } catch {
            if (error as? APIError) == .http(409) {
                // A conflict response rejects admission. Keep this exact content
                // until the user explicitly loads the current server version.
                draft?.status = "draft"; conflicted = true; sendOperation = nil
                self.error = "This draft changed before Send arrived. No new send was admitted. Reload the server draft to review the current version."
            } else if let api = error as? APIError, [.http(400), .http(401), .http(403), .http(404), .http(413), .http(422)].contains(api) {
                draft?.status = "draft"; sendOperation = nil
                self.error = "The send request was rejected. " + error.localizedDescription
            } else {
                // A transport error after admission is ambiguous. A fresh operation
                // could duplicate delivery, so only a read-only status check follows.
                draft?.status = "unknown"
                self.error = "Send outcome is unknown. Sending again is blocked. Check send status before taking another action."
            }
        }
        dirty = false; sending = false; persist()
    }
    func refreshSendStatus() async {
        guard let current = draft, current.version > 0 else { return }
        if fixtureMode { return }
        guard let client else { return }
        do {
            let remote = try await client.todoMailDraft(id: current.id)
            // A still-editable server draft alone does not disprove an in-flight
            // request. Preserve uncertainty until the server reports a terminal send.
            if remote.status == "sent" || remote.status == "unknown" || remote.status == "sending" { draft = remote }
            error = draft?.status == "sent" ? nil : "Delivery is not confirmed. Retry remains blocked."
            persist()
        } catch { self.error = error.localizedDescription }
    }
    func reloadServerDraft() async {
        guard let current = draft, !current.isLocked, let client else { return }
        debounce?.cancel()
        if let saveTask { _ = try? await saveTask.value; while saving { await Task.yield() } }
        do {
            draft = try await client.todoMailDraft(id: current.id)
            dirty = false; conflicted = false; error = nil; persist()
        } catch { self.error = error.localizedDescription }
    }
    private func mergeRemote(_ remote: TodoMailDraft) {
        if let local = draft {
            if remote.isLocked {
                if dirty {
                    draft?.status = remote.status
                    error = "This draft was sent or locked on another device. Your unsaved text is retained on this device."
                } else { draft = remote; dirty = false }
            }
            else if local.isLocked { return }
            else if dirty {
                var comparable = local
                comparable.version = remote.version
                if comparable.saveJSON == remote.saveJSON {
                    draft = remote; dirty = false; persist(); return
                }
                if local.version != remote.version { conflicted = true; error = "This draft changed on another device. Your edits are kept here." }
                return
            } else { draft = remote }
        } else { draft = remote; dirty = false }
        persist()
    }
    private func persist() {
        guard let draft else { return }
        do {
            let directory = recoveryURL.deletingLastPathComponent()
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            var protectedDirectory = directory
            var values = URLResourceValues(); values.isExcludedFromBackup = true
            try protectedDirectory.setResourceValues(values)
            let data = try JSONEncoder().encode(Recovery(draft: draft, dirty: dirty, sendOperation: sendOperation))
            try data.write(to: recoveryURL, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
            localPersistenceFailed = false
        } catch {
            localPersistenceFailed = true
            self.error = "Couldn’t save recovery on this device. Keep this draft open and save it to your account before leaving."
        }
    }
    func attachment(_ attachment: TodoMailAttachment, messageID: String) async throws -> URL {
        if fixtureMode {
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent("TodoMailFixture", isDirectory: true)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let url = directory.appendingPathComponent("Launch notes.txt")
            try Data("Launch review\n\nThursday at 10. Confirm the launch date and first-round participants.\n".utf8).write(to: url, options: .atomic)
            return url
        }
        guard let client else { throw APIError.invalidCredential }
        return try await client.downloadTodoMailAttachment(connectionID: connectionID, messageID: messageID, attachment: attachment)
    }
    private static func addresses(_ header: String) -> [String] {
        let pattern = #"[A-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Z0-9.-]+\.[A-Z]{2,}"#
        guard let expression = try? NSRegularExpression(pattern: pattern, options: .caseInsensitive) else { return [] }
        return expression.matches(in: header, range: NSRange(header.startIndex..., in: header)).compactMap {
            Range($0.range, in: header).map { String(header[$0]) }
        }
    }
    private static func unique(_ addresses: [String], excluding: [String]) -> [String] {
        var seen = Set(excluding.map { $0.lowercased() })
        return addresses.filter { !$0.isEmpty && seen.insert($0.lowercased()).inserted }
    }
}
