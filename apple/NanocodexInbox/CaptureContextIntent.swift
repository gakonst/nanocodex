import AppIntents
import NanocodexContext
import UniformTypeIdentifiers

enum MessagingSource: String, CaseIterable, Identifiable {
    case messages = "Messages", whatsapp = "WhatsApp", instagram = "Instagram", signal = "Signal"
    var id: String { rawValue }
    var title: String { self == .messages ? "iMessage" : rawValue }
    var actionTitle: String { "Capture " + title }
}

private func captureMessage(_ text: String, from source: MessagingSource, sender: String = "") throws {
    let store = try ContextStore.shared()
    try store.capture([CaptureInput(source: source.rawValue, text: text, sender: sender)], session: store.captureSession())
}

struct CaptureIMessageIntent: AppIntent {
    static var title: LocalizedStringResource = "Capture iMessage"
    static var description = IntentDescription("Save the message text supplied by a Shortcuts Message automation to Nanocodex. Works with iMessage and SMS.")
    static var openAppWhenRun = false
    @Parameter(title: "Message") var text: String
    @Parameter(title: "Sender", default: "") var sender: String
    static var parameterSummary: some ParameterSummary { Summary("Capture iMessage \(\.$text)") { \.$sender } }
    func perform() async throws -> some IntentResult {
        try captureMessage(text, from: .messages, sender: sender)
        return .result()
    }
}

struct CaptureWhatsAppIntent: AppIntent {
    static var title: LocalizedStringResource = "Capture WhatsApp"
    static var description = IntentDescription("Save WhatsApp text supplied by a shortcut, such as notification text or text extracted from a screenshot, to Nanocodex.")
    static var openAppWhenRun = false
    @Parameter(title: "Message") var text: String
    static var parameterSummary: some ParameterSummary { Summary("Capture WhatsApp \(\.$text)") }
    func perform() async throws -> some IntentResult {
        try captureMessage(text, from: .whatsapp)
        return .result()
    }
}

struct CaptureInstagramIntent: AppIntent {
    static var title: LocalizedStringResource = "Capture Instagram"
    static var description = IntentDescription("Save Instagram text supplied by a shortcut, such as notification text or text extracted from a screenshot, to Nanocodex.")
    static var openAppWhenRun = false
    @Parameter(title: "Message") var text: String
    static var parameterSummary: some ParameterSummary { Summary("Capture Instagram \(\.$text)") }
    func perform() async throws -> some IntentResult {
        try captureMessage(text, from: .instagram)
        return .result()
    }
}

struct CaptureSignalIntent: AppIntent {
    static var title: LocalizedStringResource = "Capture Signal"
    static var description = IntentDescription("Save Signal text supplied by a shortcut, such as notification text or text extracted from a screenshot, for queries through this phone's Hand.")
    static var openAppWhenRun = false
    @Parameter(title: "Message") var text: String
    static var parameterSummary: some ParameterSummary { Summary("Capture Signal \(\.$text)") }
    func perform() async throws -> some IntentResult {
        try captureMessage(text, from: .signal)
        return .result()
    }
}

struct CaptureContextIntent: AppIntent {
    static var title: LocalizedStringResource = "Capture Context"
    static var description = IntentDescription("Save text or a link to your Nanocodex Context inbox for agents to query through this device's Hand. Enable capture in Nanocodex first.")
    static var openAppWhenRun = false
    @Parameter(title: "Text") var text: String
    @Parameter(title: "Source", default: "Shared") var source: String
    @Parameter(title: "Sender", default: "") var sender: String
    @Parameter(title: "Conversation", default: "") var thread: String
    @Parameter(title: "Link") var link: URL?
    @Parameter(title: "Message Date") var date: Date?
    @Parameter(title: "Source Item ID", default: "") var externalID: String
    static var parameterSummary: some ParameterSummary {
        Summary("Capture \(\.$text) from \(\.$source)") {
            \.$sender; \.$thread; \.$link; \.$date; \.$externalID
        }
    }
    func perform() async throws -> some IntentResult {
        let store = try ContextStore.shared()
        let session = try store.captureSession()
        try store.capture([CaptureInput(source: source, text: text, sender: sender, thread: thread,
                                       url: link?.absoluteString ?? "", occurredAt: date, externalID: externalID)], session: session)
        return .result()
    }
}

struct CaptureFileContextIntent: AppIntent {
    static var title: LocalizedStringResource = "Capture Text from File"
    static var description = IntentDescription("Extract text on this device from an image, screenshot, PDF, or text file and save it in Nanocodex. The original file is not saved or uploaded. Images without readable text and scanned PDFs aren't supported.")
    static var openAppWhenRun = false
    @Parameter(title: "File") var file: IntentFile
    @Parameter(title: "Source", default: "Shared") var source: String
    static var parameterSummary: some ParameterSummary { Summary("Capture text from \(\.$file) for \(\.$source)") }
    func perform() async throws -> some IntentResult {
        let store = try ContextStore.shared(), session = try store.captureSession()
        let data = file.data, name = file.filename
        guard let type = file.type ?? UTType(filenameExtension: (name as NSString).pathExtension) else { throw CaptureError.unsupported }
        let content = try await Task.detached { try ContextImport.text(data: data, type: type) }.value
        try store.capture([CaptureInput(source: source, text: content, filename: name)], session: session)
        return .result()
    }
}

struct ContextShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(intent: RunAgentTaskIntent(), phrases: ["Run a task with \(.applicationName)"], shortTitle: "Run Agent Task", systemImageName: "hand.raised")
        AppShortcut(intent: CaptureIMessageIntent(), phrases: ["Capture an iMessage in \(.applicationName)"], shortTitle: "Capture iMessage", systemImageName: "message")
        AppShortcut(intent: CaptureWhatsAppIntent(), phrases: ["Capture WhatsApp in \(.applicationName)"], shortTitle: "Capture WhatsApp", systemImageName: "message")
        AppShortcut(intent: CaptureInstagramIntent(), phrases: ["Capture Instagram in \(.applicationName)"], shortTitle: "Capture Instagram", systemImageName: "message")
        AppShortcut(intent: CaptureSignalIntent(), phrases: ["Capture Signal in \(.applicationName)"], shortTitle: "Capture Signal", systemImageName: "message")
        AppShortcut(intent: CaptureContextIntent(), phrases: ["Capture context in \(.applicationName)"], shortTitle: "Capture Context", systemImageName: "tray.and.arrow.down")
    }
}
