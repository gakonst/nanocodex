import SwiftUI
import NanocodexContext
import UniformTypeIdentifiers

struct ContextInboxView: View {
    @ObservedObject var model: InboxModel
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var query = ""
    @State private var sourceFilter = ""
    @State private var showAdd = false
    @State private var showSetup = false
    @State private var removeAll = false
    private var sources: [String] {
        Array(Set(model.contextItems.map { $0.input.sourceKey }).union(model.contextRoutes.keys)).sorted()
    }
    private var filtered: [CapturedContext] {
        model.contextItems.filter { $0.matches(query) && (sourceFilter.isEmpty || $0.input.sourceKey == sourceFilter) }
    }
    var body: some View {
        NavigationStack {
            List {
                Section {
                    Toggle("Capture from other apps", isOn: Binding(get: { model.contextEnabled }, set: model.enableContext))
                        .tint(.gray)
                        .accessibilityIdentifier("context-enabled")
                    Text(model.isDemo ? "Demo context stays separate from your account." : "Captured messages stay on this device. Agents can search them when your phone is connected as a Hand. Turning capture off pauses capture and message queries.")
                        .font(.footnote).foregroundStyle(.secondary)
                    Button("Set up Shortcuts & sharing") { showSetup = true }
                }
                if let error = model.contextError {
                    Section { Text(error).foregroundStyle(.red); Button("Retry") { model.refreshContext() } }
                }
                Section("Messaging apps") {
                    ForEach(MessagingSource.allCases) { source in
                        NavigationLink {
                            MessagingSetupView(model: model, source: source)
                        } label: {
                            HStack {
                                Text(source.title)
                                Spacer()
                                let count = model.contextItems.filter { $0.input.sourceKey == source.rawValue.lowercased() }.count
                                Text(count == 0 ? "Set up" : "\(count) captured").foregroundStyle(.secondary)
                            }
                        }.accessibilityIdentifier("context-setup-" + source.id)
                    }
                }
                if !sources.isEmpty {
                    Section("Optional prompt attachments") {
                        Picker("Show", selection: $sourceFilter) {
                            Text("All sources").tag("")
                            ForEach(sources, id: \.self) { Text(sourceName($0)).tag($0) }
                        }
                        ForEach(sources, id: \.self) { source in
                            Picker(sourceName(source), selection: Binding(get: { model.contextRoutes[source] ?? "" }, set: { model.routeContext(source: source, agentID: $0.isEmpty ? nil : $0) })) {
                                Text("Do not attach automatically").tag("")
                                if let id = model.contextRoutes[source], !model.cards.contains(where: { $0.id == id }) {
                                    Text("Unavailable agent").tag(id)
                                }
                                ForEach(model.cards) { Text($0.title).tag($0.id) }
                            }.accessibilityIdentifier("context-route-" + source)
                        }
                        Text("Agents can query captures through this phone's Hand without attaching them. Optionally choose an agent here to also include up to 12 unused captures with your next message.")
                            .font(.footnote).foregroundStyle(.secondary)
                    }
                }
                Section("Captured context · \(filtered.count)") {
                    if filtered.isEmpty {
                        ContentUnavailableView(query.isEmpty ? "Your context starts here" : "No matches", systemImage: "tray.and.arrow.down", description: Text("Set up a message automation or capture text from screenshots, shared content, and documents."))
                    }
                    ForEach(filtered) { item in
                        NavigationLink {
                            ContextDetailView(model: model, item: item)
                        } label: {
                            VStack(alignment: .leading, spacing: 6) {
                                HStack {
                                    Text(item.input.source).font(.caption.weight(.semibold))
                                    Spacer()
                                    Text(item.input.occurredAt ?? item.capturedAt, style: .date).font(.caption).foregroundStyle(.secondary)
                                }
                                if !item.input.sender.isEmpty { Text(item.input.sender).font(.subheadline.weight(.medium)) }
                                Text(item.input.text.isEmpty ? item.input.url : item.input.text).lineLimit(3)
                                if !item.input.thread.isEmpty { Text(item.input.thread).font(.caption).foregroundStyle(.secondary) }
                            }.padding(.vertical, 4)
                        }.accessibilityIdentifier("context-item-" + item.id)
                    }.onDelete { offsets in model.removeContext(Set(offsets.map { filtered[$0].id })) }
                }
                if !model.contextItems.isEmpty {
                    Section {
                        Button("Remove all captured context", role: .destructive) { removeAll = true }
                        Text("Removing local context does not remove text already sent in agent conversations.").font(.footnote).foregroundStyle(.secondary)
                    }
                }
            }
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search context")
            .navigationTitle("Context")
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
                ToolbarItem(placement: .primaryAction) {
                    Button { showAdd = true } label: { Image(systemName: "plus") }
                        .disabled(!model.contextEnabled).accessibilityLabel("Add context")
                }
            }
            .sheet(isPresented: $showAdd) { AddContextView(model: model) }
            .sheet(isPresented: $showSetup) { ContextSetupView() }
            .confirmationDialog("Remove all captured context from this device?", isPresented: $removeAll, titleVisibility: .visible) {
                Button("Remove all", role: .destructive) { model.removeContext(Set(model.contextItems.map(\.id))) }
            }
            .onAppear { model.refreshContext() }
            .onChange(of: scenePhase) { _, phase in if phase == .active { model.refreshContext() } }
            .refreshable { model.refreshContext() }
        }.frame(minWidth: 340, minHeight: 500)
    }
    private func sourceName(_ key: String) -> String {
        model.contextItems.first { $0.input.sourceKey == key }?.input.source ?? key
    }
}

private struct MessagingSetupView: View {
    @ObservedObject var model: InboxModel
    let source: MessagingSource
    private var sourceKey: String { source.rawValue.lowercased() }
    var body: some View {
        List {
            if !model.contextEnabled {
                Section {
                    Button("Enable capture") { model.enableContext(true) }
                }
            }
            if source == .messages {
                Section("Incoming messages") {
                    Text("1. Open Shortcuts → Automation and create a Message automation. Choose the senders or message text to match, then choose Run Immediately.")
                    Text("2. Add Nanocodex → Capture iMessage. Set Message to Shortcut Input's message content. Set Sender if the automation provides it.")
                    Text("3. Test with one incoming message, then check for it in Context. This includes iMessage and SMS messages matching your automation.")
                    Link("Open Shortcuts", destination: URL(string: "shortcuts://")!)
                }
            } else {
                Section("Capture availability") {
                    Text("Nanocodex cannot read \(source.title)'s inbox directly. On iOS 26, use the screenshot shortcut below to capture visible messages.")
                    Text("If your version of Shortcuts offers a Notification automation, you can pass notification text to \(source.actionTitle) for automatic capture. Only text provided by that notification is available; hidden previews and messages in an open conversation may be missing.").font(.footnote).foregroundStyle(.secondary)
                }
            }
            Section("Query through your phone") {
                LabeledContent("Phone Hand", value: model.deviceHandStatus)
                Text("Once capture is set up, agents can search \(source.title) messages through this phone's Hand whenever they need them. You don't need to attach messages to individual prompts.")
                Text("For example: ‘What did Alex say about Friday on \(source.title)?’").font(.footnote).foregroundStyle(.secondary)
            }
            Section("Capture a conversation on screen") {
                Text("Share a screenshot to Nanocodex and set Source to \(source.rawValue). Text is extracted on this device.")
                Text("For a shortcut you can run from the Action button: Take Screenshot → Extract Text from Image → \(source.actionTitle). This works for the messages visible on screen.")
                Link("Open Shortcuts", destination: URL(string: "shortcuts://")!)
            }
            Section("Captured messages") {
                let items = model.contextItems.filter { $0.input.sourceKey == sourceKey }
                if items.isEmpty {
                    Text("No messages captured yet. Setting up a shortcut does not import existing conversations.").foregroundStyle(.secondary)
                }
                ForEach(items.prefix(5)) { item in
                    Text(item.input.text).lineLimit(3)
                }
            }
        }.navigationTitle(source.title)
    }
}

private struct ContextDetailView: View {
    @ObservedObject var model: InboxModel
    let item: CapturedContext
    @Environment(\.dismiss) private var dismiss
    @State private var agentID = ""
    @State private var remove = false
    var body: some View {
        List {
            Section("Source") {
                LabeledContent("App", value: item.input.source)
                if !item.input.sender.isEmpty { LabeledContent("Sender", value: item.input.sender) }
                if !item.input.thread.isEmpty { LabeledContent("Conversation", value: item.input.thread) }
                if let date = item.input.occurredAt { LabeledContent("Message date", value: date.formatted()) }
                LabeledContent("Captured", value: item.capturedAt.formatted())
                if !item.input.filename.isEmpty { LabeledContent("Text extracted from", value: item.input.filename) }
                if let url = URL(string: item.input.url), !item.input.url.isEmpty { Link("Open original", destination: url) }
            }
            Section("Use with an agent") {
                Picker("Agent", selection: $agentID) {
                    Text("Choose an agent").tag("")
                    ForEach(model.cards) { Text($0.title).tag($0.id) }
                }.accessibilityIdentifier("context-agent")
                Button("Use in next message") {
                    model.selectContext(item.id, agentID: agentID, selected: true)
                    model.select(agentID); model.showContext = false
                }.disabled(agentID.isEmpty).accessibilityIdentifier("context-use")
                if model.contextForAgent(agentID).contains(where: { $0.id == item.id }) {
                    Button("Remove from next message") { model.selectContext(item.id, agentID: agentID, selected: false) }
                }
                Text("You can write your request before sending. The captured text will be included as reference material.").font(.footnote).foregroundStyle(.secondary)
            }
            if !item.input.text.isEmpty { Section("Content") { Text(item.input.text).textSelection(.enabled) } }
            Section { Button("Remove capture", role: .destructive) { remove = true } }
        }
        .navigationTitle(item.input.source)
        .onAppear { agentID = model.focused?.id ?? "" }
        .confirmationDialog("Remove this captured context?", isPresented: $remove, titleVisibility: .visible) {
            Button("Remove", role: .destructive) { model.removeContext([item.id]); dismiss() }
        }
    }
}

private struct AddContextView: View {
    @ObservedObject var model: InboxModel
    @Environment(\.dismiss) private var dismiss
    @State private var source = "Shared"
    @State private var text = ""
    @State private var sender = ""
    @State private var thread = ""
    @State private var link = ""
    @State private var filename = ""
    @State private var importFile = false
    @State private var importing = false
    @State private var error: String?
    var body: some View {
        NavigationStack {
            Form {
                Section("Capture") {
                    TextField("Source app", text: $source).accessibilityIdentifier("capture-source")
                    TextField("Text", text: $text, axis: .vertical).lineLimit(4...12).accessibilityIdentifier("capture-text")
                    TextField("Web link (optional)", text: $link).accessibilityIdentifier("capture-link")
                    Button("Import text from a file") { importFile = true }.disabled(importing)
                    if importing { ProgressView("Extracting text on this device…") }
                    if !filename.isEmpty { Text("Text from \(filename)").font(.caption).foregroundStyle(.secondary) }
                }
                Section("Optional details") {
                    TextField("Sender", text: $sender).accessibilityIdentifier("capture-sender")
                    TextField("Conversation", text: $thread)
                }
                if let error { Section { Text(error).foregroundStyle(.red) } }
            }
            .navigationTitle("Add context")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        do {
                            try model.captureContext(CaptureInput(source: source, text: text, sender: sender, thread: thread, url: link, filename: filename))
                            dismiss()
                        } catch { self.error = error.localizedDescription }
                    }.disabled(importing || (text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && link.isEmpty)).accessibilityIdentifier("capture-save")
                }
            }
            .fileImporter(isPresented: $importFile, allowedContentTypes: [.image, .pdf, .plainText]) { result in
                importing = true
                Task {
                    defer { importing = false }
                    do {
                        let url = try result.get()
                        let content = try await Task.detached {
                            let access = url.startAccessingSecurityScopedResource()
                            defer { if access { url.stopAccessingSecurityScopedResource() } }
                            let values = try url.resourceValues(forKeys: [.fileSizeKey, .contentTypeKey])
                            guard (values.fileSize ?? Int.max) <= 8 * 1024 * 1024 else { throw CaptureError.tooLarge }
                            guard let type = values.contentType else { throw CaptureError.unsupported }
                            return try ContextImport.text(data: Data(contentsOf: url), type: type)
                        }.value
                        text = content; filename = url.lastPathComponent; error = nil
                    } catch { self.error = error.localizedDescription }
                }
            }
        }.frame(minWidth: 340, minHeight: 500)
    }
}

private struct ContextSetupView: View {
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            List {
                Section {
                    Link("Open Shortcuts", destination: URL(string: "shortcuts://")!)
                }
                Section("Messages & SMS") {
                    Text("In Shortcuts, create a Message automation. Choose the senders or text you want to match and when it should run.")
                    Text("Add Nanocodex → Capture iMessage. Set Message to the incoming message's content. The generic Capture Context action also accepts sender, conversation, date, and item ID when Shortcuts supplies them.")
                    Text("Test it with one message before enabling automatic runs. This captures what Shortcuts passes in; it does not read your Messages database.").foregroundStyle(.secondary)
                }
                Section("WhatsApp, Instagram & Signal") {
                    Text("These apps' inboxes are not directly accessible. On iOS 26, make a shortcut with Take Screenshot → Extract Text from Image → Capture WhatsApp, Capture Instagram, or Capture Signal. Run it from the Action button with a conversation visible.")
                    Text("If Shortcuts offers a Notification trigger on your iOS version, pass its notification text to the corresponding capture action for automatic capture. Test an incoming notification first. Neither path imports complete account history.").foregroundStyle(.secondary)
                }
                Section("Share from any app") {
                    Text("Open the system share sheet and choose Nanocodex. Review the text and source, then save. Use More to enable Nanocodex if it is not visible.")
                    Text("Safari includes selected text or readable page content and the original link. Other apps share the content they provide; a link alone stays a link. Screenshots use on-device text recognition. PDFs and text files provide extracted text; original files are not retained. Scanned PDFs and images without readable text are not supported.").foregroundStyle(.secondary)
                    Text("You can also use Capture Text from File in Shortcuts, or add context directly here.")
                }
                Section("Query from an agent") {
                    Text("While capture is enabled and this phone's Hand is connected, your account's agents can search and read captured text when needed. You do not need to attach it to each prompt or connect the source app with OAuth.")
                    Text("Turning capture off stops new captures and Hand message queries. Disconnecting stops access for that account. Removing local context does not erase text already returned to an agent.").foregroundStyle(.secondary)
                }
            }.navigationTitle("Capture setup")
                .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }.frame(minWidth: 340, minHeight: 500)
    }
}
