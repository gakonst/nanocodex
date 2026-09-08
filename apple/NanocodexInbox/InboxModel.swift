import SwiftUI
import PhotosUI
import UniformTypeIdentifiers
import NanocodexRemote
import Security
import CryptoKit
import os
import InboxCore
import NanocodexVoice
import NanocodexContext
import NanocodexHand
#if os(iOS)
import UIKit
import BackgroundTasks
#endif

private let accountPerformanceLog = OSLog(subsystem: "xyz.paradigm.centaur", category: "Performance")

@MainActor
final class InboxModel: ObservableObject {
    // App Intents and windows share the same account and Hand connection.
    static let shared = InboxModel()
    enum Filter: String, CaseIterable { case inbox = "Inbox", running = "Running", all = "All" }
    @Published var cards: [AgentCard] = []
    @Published var deck = InboxDeck()
    @Published var filter: Filter = .all { didSet { reconcile() } }
    @Published var drafts: [String: String] = [:]
    @Published var rows: [TranscriptRow] = []
    @Published var threadLoading = false
    @Published var threadError: String?
    private var observedAgentID: String?
    private var tabOrder: [String] = []
    private struct TabHistory {
        var events: [AgentEvent]
        var cursor: Cursor
        var hasOlder: Bool
    }
    private var tabHistories: [String: TabHistory] = [:]
    private var recentTabs: [String] = []
    @Published private var overviewTranscripts: [String: [TranscriptRow]] = [:]
    private var overviewVisible = Set<String>()
    private var overviewTasks: [String: Task<Void, Never>] = [:]
    private var overviewTokens: [String: UUID] = [:]
    private var overviewEvents: [String: [AgentEvent]] = [:]
    private var overviewBytes: [String: [Int]] = [:]
    private var overviewByteCounts: [String: Int] = [:]
    private var overviewProjections: [String: Task<Void, Never>] = [:]
    @Published var busy = Set<String>()
    @Published var connection = "Disconnected"
    @Published var error: String?
    @Published var notice: String?
    @Published var connected = false
    @Published private(set) var restoringAccount = true
    @Published private(set) var restorationError: String?
    @Published private(set) var isDemo = false
    @Published var refreshing = false
    @Published private(set) var scheduledJobs: [ScheduledJob] = []
    @Published private(set) var scheduledJobAgents: [String: String] = [:]
    @Published private(set) var schedulesLoading = false
    @Published private(set) var schedulesLoaded = false
    @Published private(set) var schedulesError: String?
    private var schedulesTask: Task<Void, Never>?
    private var schedulesFailures: [String: String] = [:]
    @Published var hasOlder = false
    @Published var loadingOlder = false
    @Published var selectedTurn = ""
    @Published private var pendingCreations = Set<String>()
    @Published private var creationErrors: [String: String] = [:]
    private var creationTasks: [String: Task<String, Error>] = [:]
    private var createdAgentIDs: [String: String] = [:]
    @Published var pending: [PendingMessage] = []
    @Published private(set) var cancellations: [PendingTurnCancellation] = []
    private let cancellationTasks = TurnCancellationTasks()
    @Published private(set) var attachmentDrafts: [String: [MessageAttachment]] = [:]
    private var attachmentURLs: [String: URL] = [:]
    private var attachmentMovieURLs: [String: URL] = [:]
    @Published private var attachmentImports = Set<String>()
    @Published private var attachmentErrors: [String: String] = [:]
    @Published var contextItems: [CapturedContext] = []
    @Published var contextEnabled = false
    @Published var contextRoutes: [String: String] = [:]
    @Published var selectedContext: [String: [String]] = [:]
    @Published var excludedContext: [String: [String]] = [:]
    @Published var contextError: String?
    @Published var showContext = false
    private var automaticContext: [String: [CapturedContext]] = [:]
    @Published private(set) var challenge: SMSChallenge?
    @Published private(set) var signingIn = false
    @Published private(set) var signInError: String?
    @Published private(set) var signInRetryAt: Date?
    private var smsAuth: SMSAuth?
    private var smsOrigin: String?
    private var authGeneration = UUID()
    private var pinnedThreadID: String?
    private var demoRows: [String: [TranscriptRow]] = [:]
    private var demoFaults = Set<String>()
    #if DEBUG
    private var demoVoice: Task<Void, Never>?
    #endif
    private var client: ManagedClient?
    let voice = VoiceSession()
    private var accountCredential: AccountCredential?
    private var deviceHand: HandSession?
    @Published private(set) var deviceHandConnected = false
    @Published var deviceHandEnabled = UserDefaults.standard.object(forKey: "inbox.hand.enabled") as? Bool ?? true {
        didSet {
            guard deviceHandEnabled != oldValue else { return }
            UserDefaults.standard.set(deviceHandEnabled, forKey: "inbox.hand.enabled")
            if !deviceHandEnabled { handTasks.cancelAll(stopTurns: false); endHandBackgroundTime() }
            updateDeviceHand(); scheduleHandRefresh()
        }
    }
    @Published private(set) var handBackgroundError: String?
    private lazy var handTasks = HandTaskExecution(changed: { [weak self] in
        self?.objectWillChange.send(); self?.updateDeviceHand()
    }, failed: { [weak self] message in self?.handBackgroundError = message })
    #if os(iOS)
    static let handRefreshIdentifier = "xyz.paradigm.centaur.hand.refresh"
    private var handBackgroundTask: UIBackgroundTaskIdentifier = .invalid
    private var handBackgroundDeadline: Task<Void, Never>?
    private var handRefreshing = false
    #endif
    @Published private(set) var remoteService: RemoteService?
    private var polling: Task<Void, Never>?
    private var streaming: Task<Void, Never>?
    private var streamReceivedFrame = false
    private var generation = UUID()
    private var observation = UUID()
    private var events: [AgentEvent] = []
    private var eventBytes: [Int] = []
    private var retainedBytes = 0
    private var projection: Task<Void, Never>?
    @Published private var navigation: [(id: String, seen: String?, deferred: Cursor?, filter: Filter)] = []
    private var deferred: [String: Cursor] = [:]
    private var cursor = Cursor.zero
    private var olderBefore: Cursor?
    private var seen: [String: String] = [:]
    private var scope = ""
    private var unlistedAgents = Set<String>()
    private var unavailableAgents = Set<String>()
    private var historyCursors: [String: Cursor] = [:]
    private var retries: [String: AgentCommand] = [:]
    private var isActive = UIApplication.shared.applicationState != .background
    private var didStart = false
    private var connectionAttempt = UUID()

    var focused: AgentCard? { cards.first { $0.id == deck.focusedID } }
    var focusedConversationIdentity: String? {
        focused.map { card in createdAgentIDs.first(where: { $0.value == card.id })?.key ?? card.id }
    }
    var tabCards: [AgentCard] {
        let byID = Dictionary(uniqueKeysWithValues: cards.map { ($0.id, $0) })
        return tabOrder.compactMap { byID[$0] }
    }
    var creationError: String? { focused.flatMap { creationErrors[$0.id] } }
    private func resolvedAgentID(_ id: String) -> String { createdAgentIDs[id] ?? id }
    var attentionCount: Int { cards.filter { $0.isInInbox(seen: seenCursor($0.id), deferred: deferred[$0.id]) && $0.needsAttention(seen: seenCursor($0.id)) }.count }
    var runningCount: Int { cards.filter(\.isRunning).count }
    var controllableTurns: [String] { (focused?.activeTurns ?? []).filter { id in !pending.contains { $0.agentID == focused?.id && $0.id == id } } }
    var focusedTurn: String { controllableTurns.contains(selectedTurn) ? selectedTurn : controllableTurns.first ?? "" }
    var stopTarget: String { focusedTurn.isEmpty ? focusedPending.first?.id ?? "" : focusedTurn }
    func cancellation(agentID: String, turnID: String) -> PendingTurnCancellation? {
        cancellations.first { $0.agentID == agentID && $0.turnID == turnID }
    }
    func steeringTarget(_ message: PendingMessage) -> AgentCommand? {
        guard focusedPending.first?.id == message.id,
              let card = cards.first(where: { $0.id == message.agentID }) else { return nil }
        let available = card.activeTurns.filter { turnID in
            !cancellations.contains { $0.agentID == card.id && $0.turnID == turnID && $0.acknowledged && $0.error == nil }
        }
        return message.interruption(activeTurns: available)
    }
    var hasUnconfirmedMessage: Bool { focusedPending.contains { $0.phase == .failed } }
    var focusedPending: [PendingMessage] {
        pending.filter { message in
            guard message.agentID == focused?.id else { return false }
            let stop = cancellation(agentID: message.agentID, turnID: message.id)
            // A cancelled queued turn may wait behind running work before its
            // terminal event. Its durable cancellation must not block the queue.
            return !(message.phase == .cancelling && stop?.acknowledged == true && stop?.error == nil)
        }
    }
    func openThread() {
        pinnedThreadID = focused?.id
        if isDemo, ProcessInfo.processInfo.environment["NANOCODEX_DEMO_LONG_THREAD"] == "1", let id = focused?.id {
            hasOlder = true
            let epoch = generation
            Task {
                try? await Task.sleep(for: .seconds(12))
                guard generation == epoch, focused?.id == id, !rows.contains(where: { $0.id == "live-tail" }) else { return }
                rows.append(.init(id: "live-tail", role: "Agent", text: "I found one more edge case in the retry path."))
                demoRows[id] = rows
            }
        }
        if isDemo, ProcessInfo.processInfo.environment["NANOCODEX_DEMO_FINISH_IN_THREAD"] == "1",
           let id = focused?.id, !focusedTurn.isEmpty {
            let turn = focusedTurn, epoch = generation
            Task {
                try? await Task.sleep(for: .milliseconds(1000))
                guard generation == epoch else { return }
                demoFinish(agentID: id, turnID: turn)
            }
        }
    }
    func closeThread() { pinnedThreadID = nil; reconcile() }
    var canGoBack: Bool { navigation.contains { previous in previous.id != focused?.id && cards.contains { $0.id == previous.id } } }
    var canRetry: Bool { focused.flatMap { retries[$0.id] }?.kind == .followUp }
    var draft: String {
        get { focused.flatMap { drafts[$0.id] } ?? "" }
        set {
            guard let id = focused?.id, drafts[id] != newValue else { return }
            drafts[id] = newValue
            // Persist each edit without re-encoding pending turns or rewriting
            // unrelated account state on every keystroke.
            if !scope.isEmpty { UserDefaults.standard.set(drafts, forKey: "inbox.drafts." + scope) }
        }
    }

    struct AttachmentTarget: Sendable {
        fileprivate let agentID: String
        fileprivate let generation: UUID
        fileprivate let scope: String
    }
    var focusedAttachments: [MessageAttachment] { attachmentDrafts[focused?.id ?? ""] ?? [] }
    var preparingAttachments: Bool { attachmentImports.contains(focused?.id ?? "") }
    var attachmentError: String? { attachmentErrors[focused?.id ?? ""] }
    var canSend: Bool {
        focused != nil && !hasUnconfirmedMessage && !preparingAttachments
            && !busy.contains(focused?.id ?? "")
            && (!draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !focusedAttachments.isEmpty)
    }
    func captureAttachmentTarget() -> AttachmentTarget? {
        guard let id = focused?.id, connected, !isDemo, !preparingAttachments else { return nil }
        attachmentErrors[id] = nil
        return AttachmentTarget(agentID: id, generation: generation, scope: scope)
    }
    func attachmentURL(_ attachment: MessageAttachment) -> URL? {
        attachmentURLs[attachment.id]
    }
    func attachmentMovieURL(_ attachment: MessageAttachment) -> URL? { attachmentMovieURLs[attachment.id] }
    func downloadVideo(_ video: TranscriptVideo, agentID: String) async throws -> URL {
        guard let client else { throw APIError.invalidCredential }
        let epoch = generation
        let url = try await client.downloadVideo(agentID: agentID, video: video)
        guard epoch == generation, !Task.isCancelled else {
            try? FileManager.default.removeItem(at: url)
            throw CancellationError()
        }
        return url
    }
    private func cacheAttachment(_ attachment: MessageAttachment, scope: String) {
        guard let store = try? AttachmentStore(scope: scope) else { return }
        attachmentURLs[attachment.id] = try? store.previewURL(for: attachment)
        if attachment.isVideo { attachmentMovieURLs[attachment.id] = try? store.url(for: attachment) }
    }
    func removeAttachment(_ id: String) {
        guard let agentID = focused?.id, let attachment = attachmentDrafts[agentID]?.first(where: { $0.id == id }) else { return }
        attachmentDrafts[agentID]?.removeAll { $0.id == id }; attachmentErrors[agentID] = nil
        attachmentURLs[attachment.id] = nil
        attachmentMovieURLs[attachment.id] = nil
        persist(); releaseAttachments([attachment])
    }
    private func beginAttachmentImport(count: Int, target: AttachmentTarget) -> Bool {
        guard generation == target.generation, !attachmentImports.contains(resolvedAgentID(target.agentID)) else { return false }
        guard count > 0 else { return false }
        guard (attachmentDrafts[resolvedAgentID(target.agentID)]?.count ?? 0) + count <= 4 else {
            attachmentErrors[resolvedAgentID(target.agentID)] = "You can attach up to 4 photos or videos to a message."
            return false
        }
        attachmentImports.insert(resolvedAgentID(target.agentID)); attachmentErrors[resolvedAgentID(target.agentID)] = nil
        return true
    }
    func importAttachmentFiles(_ urls: [URL], target: AttachmentTarget) {
        guard beginAttachmentImport(count: urls.count, target: target) else { return }
        Task {
            defer { if generation == target.generation { attachmentImports.remove(resolvedAgentID(target.agentID)) } }
            for url in urls {
                do {
                    let attachment = try await Task.detached(priority: .userInitiated) {
                        if UTType(filenameExtension: url.pathExtension)?.conforms(to: .movie) == true {
                            let prepared = try await VideoAttachmentPreparation.prepare(url: url)
                            try AttachmentStore(scope: target.scope).save(prepared)
                            return prepared.attachment
                        }
                        let prepared = try AttachmentPreparation.prepare(url: url)
                        try AttachmentStore(scope: target.scope).save(prepared)
                        return prepared.attachment
                    }.value
                    guard generation == target.generation else {
                        try? AttachmentStore(scope: target.scope).remove(attachment)
                        return
                    }
                    cacheAttachment(attachment, scope: target.scope)
                    attachmentDrafts[resolvedAgentID(target.agentID), default: []].append(attachment); persist()
                } catch {
                    guard generation == target.generation else { return }
                    attachmentErrors[resolvedAgentID(target.agentID)] = error.localizedDescription
                }
            }
        }
    }
    func importAttachmentPhotos(_ items: [PhotosPickerItem], target: AttachmentTarget) {
        guard beginAttachmentImport(count: items.count, target: target) else { return }
        Task {
            defer { if generation == target.generation { attachmentImports.remove(resolvedAgentID(target.agentID)) } }
            for (index, item) in items.enumerated() {
                do {
                    let attachment: MessageAttachment
                    if item.supportedContentTypes.contains(where: { $0.conforms(to: .movie) }) && !item.supportedContentTypes.contains(where: { $0.conforms(to: .image) }) {
                        guard let picked = try await item.loadTransferable(type: PickedVideo.self) else { throw VideoAttachmentError.unsupported }
                        defer { try? FileManager.default.removeItem(at: picked.url) }
                        guard generation == target.generation else { return }
                        attachment = try await Task.detached(priority: .userInitiated) {
                            let prepared = try await VideoAttachmentPreparation.prepare(url: picked.url, name: "Video \(index + 1)." + picked.url.pathExtension)
                            try AttachmentStore(scope: target.scope).save(prepared)
                            return prepared.attachment
                        }.value
                    } else {
                        guard let data = try await item.loadTransferable(type: Data.self) else { throw APIError.invalidResponse }
                        guard generation == target.generation else { return }
                        attachment = try await Task.detached(priority: .userInitiated) {
                            let prepared = try AttachmentPreparation.prepare(data: data, name: "Photo \(index + 1)", mediaType: "image/jpeg")
                            try AttachmentStore(scope: target.scope).save(prepared)
                            return prepared.attachment
                        }.value
                    }
                    guard generation == target.generation else {
                        try? AttachmentStore(scope: target.scope).remove(attachment)
                        return
                    }
                    cacheAttachment(attachment, scope: target.scope)
                    attachmentDrafts[resolvedAgentID(target.agentID), default: []].append(attachment); persist()
                } catch {
                    guard generation == target.generation else { return }
                    attachmentErrors[resolvedAgentID(target.agentID)] = error.localizedDescription
                }
            }
        }
    }
    #if os(iOS)
    func importCameraPhoto(_ image: UIImage, target: AttachmentTarget) {
        guard beginAttachmentImport(count: 1, target: target) else { return }
        Task {
            defer { if generation == target.generation { attachmentImports.remove(resolvedAgentID(target.agentID)) } }
            do {
                let attachment = try await Task.detached(priority: .userInitiated) {
                    guard let data = image.jpegData(compressionQuality: 0.95) else { throw AttachmentError.unsupportedImage }
                    let prepared = try AttachmentPreparation.prepare(data: data, name: "Camera photo.jpg", mediaType: "image/jpeg")
                    try AttachmentStore(scope: target.scope).save(prepared)
                    return prepared.attachment
                }.value
                guard generation == target.generation else {
                    try? AttachmentStore(scope: target.scope).remove(attachment)
                    return
                }
                cacheAttachment(attachment, scope: target.scope)
                attachmentDrafts[resolvedAgentID(target.agentID), default: []].append(attachment); persist()
            } catch {
                guard generation == target.generation else { return }
                attachmentErrors[resolvedAgentID(target.agentID)] = error.localizedDescription
            }
        }
    }
    #endif
    private func releaseAttachments(_ attachments: [MessageAttachment]) {
        guard !attachments.isEmpty, let store = try? AttachmentStore(scope: scope) else { return }
        for attachment in attachments { attachmentURLs[attachment.id] = nil; attachmentMovieURLs[attachment.id] = nil }
        Task.detached(priority: .utility) {
            for attachment in attachments { try? store.remove(attachment) }
        }
    }

    func start() async {
        guard !didStart else { return }; didStart = true
        do { try ContextStore.shared().activate(nil) } catch { contextError = error.localizedDescription }
        #if DEBUG
        if ProcessInfo.processInfo.arguments.contains("--demo") { demo(); return }
        #endif
        await restoreSavedAccount()
    }
    func restoreSavedAccount() async {
        guard restoringAccount, !signingIn else { return }
        let signpostID = OSSignpostID(log: accountPerformanceLog)
        os_signpost(.begin, log: accountPerformanceLog, name: "RestoreAccount", signpostID: signpostID)
        defer { os_signpost(.end, log: accountPerformanceLog, name: "RestoreAccount", signpostID: signpostID) }
        signingIn = true; restorationError = nil
        defer { signingIn = false }
        do {
            guard let saved = try KeychainAccount.read() else {
                restoringAccount = false
                return
            }
            // A saved credential has already been persisted. Restoring it must
            // not depend on another Keychain write or show the SMS screen while
            // the service is loading or temporarily unreachable.
            try await connect(origin: saved.origin, key: saved.apiKey, saveCredential: false)
        } catch {
            if let apiError = error as? APIError, apiError == .http(401) || apiError == .http(403) {
                restoringAccount = false
                signInError = "Your saved sign-in has expired. Sign in again to continue."
            } else {
                restorationError = error.localizedDescription
            }
        }
    }
    func startSignIn(phone: String, origin: String) async {
        guard !signingIn else { return }
        signingIn = true; signInError = nil
        let epoch = authGeneration
        defer { if epoch == authGeneration { signingIn = false } }
        do {
            let origin = origin.trimmingCharacters(in: .whitespacesAndNewlines)
            if smsAuth == nil || smsOrigin != origin {
                try await smsAuth?.cancel()
                smsAuth = try SMSAuth(origin: origin, deviceName: "Nanocodex")
                smsOrigin = origin
            }
            let next = try await smsAuth?.start(phone: phone)
            guard epoch == authGeneration else { return }
            challenge = next; signInRetryAt = nil; error = nil
        } catch {
            if epoch == authGeneration {
                signInError = error.localizedDescription
                signInRetryAt = (error as? SMSAuthError)?.retryAt
            }
        }
    }
    func verifySignIn(code: String) async {
        guard !signingIn, let auth = smsAuth else { return }
        signingIn = true; signInError = nil
        let epoch = authGeneration
        defer { if epoch == authGeneration { signingIn = false } }
        do {
            let credential = try await auth.verify(code: code)
            guard epoch == authGeneration else { return }
            // connect checks the account, saves to Keychain, then adopts it.
            // Retain the private SMS session/key on failure so retry is safe.
            try await connect(origin: credential.origin, key: credential.apiKey)
            smsAuth = nil; smsOrigin = nil; challenge = nil; signInRetryAt = nil
            try? await auth.complete()
        } catch {
            if epoch == authGeneration {
                signInError = error.localizedDescription
                signInRetryAt = (error as? SMSAuthError)?.retryAt
            }
        }
    }
    @discardableResult
    func cancelSignIn() async -> Bool {
        guard !signingIn else { return false }
        authGeneration = UUID(); connectionAttempt = UUID()
        signingIn = true; signInError = nil
        defer { signingIn = false }
        do {
            try await smsAuth?.cancel()
            smsAuth = nil; smsOrigin = nil; challenge = nil; signInRetryAt = nil
            return true
        } catch {
            // Keep the actor reachable to retry cleanup before another login.
            signInError = error.localizedDescription
            return false
        }
    }
    func connect(origin: String, key: String, saveCredential: Bool = true) async throws {
        let attempt = UUID(); connectionAttempt = attempt
        let credential = try AccountCredential(origin: origin.trimmingCharacters(in: .whitespacesAndNewlines), apiKey: key.trimmingCharacters(in: .whitespacesAndNewlines))
        let candidate = ManagedClient(credential: credential)
        let initial: [AgentCard]
        do {
            initial = try await candidate.list()
            guard connectionAttempt == attempt, !Task.isCancelled else { candidate.close(); throw CancellationError() }
            if saveCredential { try KeychainAccount.save(credential) }
        } catch { candidate.close(); throw error }
        reset()
        client = candidate
        accountCredential = credential
        remoteService = try RemoteService(origin: URL(string: credential.origin)!) { request in
            request.setValue("Bearer " + credential.apiKey, forHTTPHeaderField: "Authorization")
        }
        scope = SHA256.hash(data: Data((credential.origin + ":" + String(credential.apiKey.prefix(21))).utf8)).map { String(format: "%02x", $0) }.joined()
        configureDeviceHand(credential)
        activateContext()
        drafts = UserDefaults.standard.dictionary(forKey: "inbox.drafts." + scope) as? [String: String] ?? [:]
        seen = UserDefaults.standard.dictionary(forKey: "inbox.seen." + scope) as? [String: String] ?? [:]
        restorePending()
        if let data = UserDefaults.standard.data(forKey: "inbox.attachments." + scope) {
            attachmentDrafts = (try? JSONDecoder().decode([String: [MessageAttachment]].self, from: data)) ?? [:]
        }
        let retainedImages = Set((Array(attachmentDrafts.values).flatMap { $0 } + pending.flatMap { $0.attachments ?? [] }).map(\.id))
        if let store = try? AttachmentStore(scope: scope) {
            try? store.prune(keeping: retainedImages)
            for attachment in Array(attachmentDrafts.values).flatMap({ $0 }) + pending.flatMap({ $0.attachments ?? [] }) {
                cacheAttachment(attachment, scope: scope)
            }
        }
        cards = initial
        restoreCreations()
        connected = true; connection = "Connecting"; reconcile(); resume(initialListing: initial)
        // Reuse the authenticated roster and warm schedules before the clock is tapped.
        startScheduledJobsRefresh(initialListing: initial)
        updateDeviceHand(); scheduleHandRefresh()
    }
    func disconnect() throws {
        try ContextStore.shared().activate(nil)
        // Leaving sample agents must not touch a saved account or require Keychain access.
        if !isDemo { try KeychainAccount.remove() }
        reset()
    }
    private func reset() {
        stopOverview()
        overviewTranscripts = [:]; tabHistories = [:]; recentTabs = []; tabOrder = []
        handTasks.cancelAll(stopTurns: false)
        schedulesTask?.cancel(); schedulesTask = nil; schedulesFailures = [:]
        scheduledJobs = []; scheduledJobAgents = [:]; schedulesLoading = false; schedulesLoaded = false; schedulesError = nil
        for task in creationTasks.values { task.cancel() }
        creationTasks = [:]; pendingCreations = []; creationErrors = [:]; createdAgentIDs = [:]
        cancellationTasks.cancelAll(); cancellations = []
        deviceHand?.close(); deviceHand = nil; deviceHandConnected = false
        endHandBackgroundTime()
        #if os(iOS)
        BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: Self.handRefreshIdentifier)
        #endif
        #if DEBUG
        demoVoice?.cancel(); demoVoice = nil
        #endif
        do { try ContextStore.shared().activate(nil) } catch { contextError = error.localizedDescription }
        contextItems = []; contextRoutes = [:]; contextEnabled = false; selectedContext = [:]; excludedContext = [:]; showContext = false; automaticContext = [:]
        voice.stop(); voice.transcriptFeed.clear(); accountCredential = nil; unlistedAgents = []; unavailableAgents = []; historyCursors = [:]
        remoteService?.close(); remoteService = nil
        connectionAttempt = UUID(); generation = UUID(); observation = UUID(); polling?.cancel(); streaming?.cancel(); client?.close(); client = nil
        projection?.cancel(); projection = nil; eventBytes = []; retainedBytes = 0; navigation = []; deferred = [:]
        observedAgentID = nil; threadLoading = false; threadError = nil
        connected = false; restoringAccount = false; restorationError = nil
        isDemo = false; cards = []; deck = InboxDeck(); rows = []; events = []; drafts = [:]; seen = [:]
        attachmentDrafts = [:]; attachmentURLs = [:]; attachmentMovieURLs = [:]; attachmentImports = []; attachmentErrors = [:]
        scope = ""; error = nil; notice = nil; busy = []; retries = [:]; refreshing = false
        hasOlder = false; loadingOlder = false; connection = "Disconnected"; pending = []; pinnedThreadID = nil; demoRows = [:]; demoFaults = []
    }
    func setActive(_ active: Bool) {
        let wasActive = isActive
        isActive = active
        if active { endHandBackgroundTime() }
        guard wasActive != active else { return }
        updateDeviceHand()
        #if os(iOS)
        if !active, handBackgroundTask != .invalid {
            handBackgroundDeadline = Task { [weak self] in
                do { try await Task.sleep(for: .seconds(25)) } catch { return }
                self?.endHandBackgroundTime(); self?.updateDeviceHand()
            }
        }
        #endif
        if active && restoringAccount && restorationError != nil {
            Task { await restoreSavedAccount() }
        }
        if active { refreshContext() }
        if active { if isDemo { connection = "Demo" } else { resume() }; resumeOverview() }
        else { suspendOverview(); scheduleHandRefresh(); polling?.cancel(); streaming?.cancel(); streaming = nil; observation = UUID(); connection = "Paused" }
    }
    private func resume(initialListing: [AgentCard]? = nil) {
        guard connected, !isDemo, isActive else { return }
        for id in pendingCreations where creationErrors[id] == nil { prepareAgent(id) }
        resumeCancellations(restart: true)
        updateDeviceHand()
        let previousPolling = polling
        previousPolling?.cancel()
        let epoch = generation
        polling = Task { [weak self] in
            // Let a cancelled refresh release its in-flight guard before the
            // foreground refresh starts; otherwise it waits another 15 seconds.
            await previousPolling?.value
            var listing = initialListing
            while !Task.isCancelled {
                guard let self, self.generation == epoch else { return }
                await self.refresh(initialListing: listing)
                listing = nil
                do { try await Task.sleep(for: .seconds(15)) } catch { return }
            }
        }
        // connect's reconciliation already selected and started the observer.
        observeFocused(restart: initialListing == nil)
    }
    private func configureDeviceHand(_ credential: AccountCredential) {
        let key = "inbox.hand.id." + scope
        let id = UserDefaults.standard.string(forKey: key) ?? "ios-" + UUID().uuidString.lowercased()
        UserDefaults.standard.set(id, forKey: key)
        do {
            let documents = try FileManager.default.url(for: .documentDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
            let root = documents.appendingPathComponent("Nanocodex", isDirectory: true).appendingPathComponent(scope, isDirectory: true)
            let name = UIDevice.current.model
            let platform = "ios"
            let messageContext = (try? ContextStore.shared()).map { ContextQuery(store: $0, scope: scope) }
            let workspace = try HandWorkspace(id: id, name: name, root: root, platform: platform, messageContext: messageContext)
            let hand = try HandSession(credential: credential, workspace: workspace)
            let epoch = generation
            hand.onConnectionChange = { [weak self] value in
                guard let self, self.generation == epoch else { return }
                self.deviceHandConnected = value
            }
            deviceHand = hand
        } catch { deviceHandConnected = false }
    }

    var deviceHandStatus: String {
        if !deviceHandEnabled { return "Hand disabled" }
        if !connected { return "Sign in to connect this Hand" }
        if deviceHandConnected { return handTasks.hasBackgroundRuntime ? "Hand working in background" : "Hand connected" }
        #if os(iOS)
        if !isActive { return "Waiting for iOS background time" }
        #endif
        return "Hand reconnecting…"
    }
    private func updateDeviceHand() {
        guard connected, !isDemo, deviceHandEnabled else {
            deviceHand?.stop()
            return
        }
        if deviceHand == nil, let credential = accountCredential { configureDeviceHand(credential) }
        guard let deviceHand else { return }
        guard hasHandExecutionTime else { deviceHand.stop(); return }
        deviceHand.start()
    }
    // Called before backgrounding, including when the screen locks. iOS owns
    // the deadline; never keep a stale socket advertised after time expires.
    func prepareHandForBackground() {
        #if os(iOS)
        guard connected, deviceHandEnabled, !isDemo, handBackgroundTask == .invalid else { return }
        handBackgroundTask = UIApplication.shared.beginBackgroundTask(withName: "Finish Hand work") { [weak self] in
            self?.endHandBackgroundTime(); self?.updateDeviceHand()
        }
        #endif
    }
    private func endHandBackgroundTime() {
        #if os(iOS)
        handBackgroundDeadline?.cancel(); handBackgroundDeadline = nil
        let identifier = handBackgroundTask; handBackgroundTask = .invalid
        if identifier != .invalid { UIApplication.shared.endBackgroundTask(identifier) }
        if !isActive { handTasks.suspendWithoutRuntime() }
        #endif
    }
    private var hasHandExecutionTime: Bool {
        isActive || handBackgroundTask != .invalid || handRefreshing || handTasks.hasBackgroundRuntime
    }
    private func scheduleHandRefresh() {
        #if os(iOS)
        guard (connected || restoringAccount && restorationError != nil), !isDemo, deviceHandEnabled else {
            BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: Self.handRefreshIdentifier)
            handBackgroundError = nil
            return
        }
        let request = BGAppRefreshTaskRequest(identifier: Self.handRefreshIdentifier)
        request.earliestBeginDate = Date(timeIntervalSinceNow: 15 * 60)
        do { try BGTaskScheduler.shared.submit(request); handBackgroundError = nil }
        catch { handBackgroundError = "Background refresh is unavailable. Open Nanocodex to reconnect this Hand." }
        #endif
    }
    #if os(iOS)
    func refreshHandInBackground() async {
        guard deviceHandEnabled, !isDemo, !Task.isCancelled else { return }
        if UIApplication.shared.applicationState == .background { setActive(false) }
        handRefreshing = true
        defer { handRefreshing = false; updateDeviceHand(); scheduleHandRefresh() }
        await start()
        if !connected, restoringAccount, !signingIn, !Task.isCancelled { await restoreSavedAccount() }
        guard connected, deviceHandEnabled, !Task.isCancelled else { return }
        updateDeviceHand()
        await refresh()
        // A short service window accompanies the content refresh. Cancellation
        // from SwiftUI's backgroundTask ends it as soon as iOS expires the task.
        do { try await Task.sleep(for: .seconds(20)) } catch { }
    }
    #endif
    func retryConnection() {
        guard connected, !isDemo, isActive else { return }
        if focused != nil { observeFocused(restart: true) }
        else { Task { await refresh() } }
    }
    func refresh(initialListing: [AgentCard]? = nil) async {
        guard let client, !refreshing else { return }
        let epoch = generation
        refreshing = true
        defer { if generation == epoch { refreshing = false } }
        do {
            let received: [AgentCard]
            if let initialListing { received = initialListing }
            else { received = try await client.list() }
            let listing = received.filter { !unavailableAgents.contains($0.id) }
            guard generation == epoch, !Task.isCancelled else { return }
            let retained = Dictionary(uniqueKeysWithValues: cards.map { ($0.id, $0) })
            let listedIDs = Set(listing.map(\.id))
            unlistedAgents.subtract(listedIDs)
            historyCursors = historyCursors.filter { listedIDs.contains($0.key) || unlistedAgents.contains($0.key) }
            // A list request already in flight when Create finishes can omit the
            // new agent. Retain it until the service has listed it at least once.
            let created = cards.filter { unlistedAgents.contains($0.id) || pendingCreations.contains($0.id) }
            let merged = listing.map { summary in
                var card = retained[summary.id] ?? summary
                card.title = summary.title; card.updatedAt = max(card.updatedAt, summary.updatedAt); card.turnCount = summary.turnCount
                card.mayHaveScheduledJobs = summary.mayHaveScheduledJobs
                return card
            } + created
            if cards != merged { cards = merged }
            reconcile()
            // Keep all agents covered: roster timestamps do not version replies
            // or settings. Prioritize interactive work without waiting for an
            // entire four-agent batch before publishing completed results.
            let pendingIDs = Set(pending.map(\.agentID) + cancellations.map(\.agentID)).union(busy)
            let byID = Dictionary(uniqueKeysWithValues: cards.map { ($0.id, $0) })
            let updateCards = listing.map { byID[$0.id] ?? $0 }
                .filter { initialListing == nil || $0.id != observedAgentID }
            let updateIDs = AgentCard.refreshOrder(updateCards, focusedID: observedAgentID,
                                                  voiceID: voice.conversationID, pendingIDs: pendingIDs)
            await client.refreshAgents(updateIDs, history: { [weak self] id in
                await self?.refreshHistory(id, epoch: epoch)
            }, onResult: { [weak self] id, result in
                await self?.applyRefresh(result, id: id, epoch: epoch)
            })
            guard generation == epoch, !Task.isCancelled else { return }
            prioritizeNext()
        } catch {
            guard generation == epoch, !Task.isCancelled else { return }
            self.error = error.localizedDescription
        }
    }
    private func refreshHistory(_ id: String, epoch: UUID) -> AgentRefreshHistory? {
        guard generation == epoch, !Task.isCancelled, let card = cards.first(where: { $0.id == id }) else { return nil }
        // The focused observer owns history. Evaluate this when the operation
        // starts rather than capturing the old tab at the start of the sweep.
        if id == observedAgentID && streaming != nil { return AgentRefreshHistory.stateOnly }
        return card.checked && card.error == nil ? .changed(after: historyCursors[id] ?? .zero) : .initial
    }
    private func applyRefresh(_ result: Result<AgentRefreshResult, Error>, id: String, epoch: UUID) {
        guard generation == epoch, !Task.isCancelled else { return }
        if case .failure(let error) = result,
           let apiError = error as? APIError, apiError == .agentDeleting || apiError == .http(404) {
            forgetUnavailableAgent(id); return
        }
        guard let index = cards.firstIndex(where: { $0.id == id }) else { return }
        do {
            let update = try result.get()
            var card = cards[index]
            try card.apply(state: update.state)
            if let page = update.page {
                card.apply(events: page.events)
                historyCursors[id] = max(historyCursors[id] ?? .zero, page.latest)
            }
            if cards[index] != card { cards[index] = card }
            reconcilePending(id: id, events: update.page?.events ?? [], state: card)
        } catch { cards[index].error = error.localizedDescription }
        reconcile()
    }
    private func seenCursor(_ id: String) -> Cursor? { seen[id].flatMap { Cursor(rawValue: $0) } }
    private func forgetUnavailableAgent(_ id: String) {
        // Deletion fences can outlive the account listing while owned resources
        // are cleaned up. These conversations cannot be read or resumed.
        unavailableAgents.insert(id); unlistedAgents.remove(id)
        historyCursors.removeValue(forKey: id)
        if voice.conversationID == id { voice.stop() }
        cards.removeAll { $0.id == id }
        setOverviewVisible(id, visible: false)
        overviewTranscripts[id] = nil; tabHistories[id] = nil; recentTabs.removeAll { $0 == id }
        if pinnedThreadID == id { pinnedThreadID = nil }
        reconcile()
    }
    private func reconcile() {
        let available = Set(cards.map(\.id))
        var unique = Set<String>()
        tabOrder.removeAll { !available.contains($0) || !unique.insert($0).inserted }
        let known = Set(tabOrder)
        tabOrder.append(contentsOf: cards.map(\.id).filter { !known.contains($0) })
        let previous = deck.focusedID
        let eligible = cards.filter { card in
            if card.id == pinnedThreadID { return true }
            switch filter {
            case .inbox: return card.isInInbox(seen: seenCursor(card.id), deferred: deferred[card.id])
            case .running: return card.isRunning
            case .all: return true
            }
        }.sorted { a, b in
            let ar = a.needsAttention(seen: seenCursor(a.id)), br = b.needsAttention(seen: seenCursor(b.id))
            return ar != br ? ar : a.updatedAt != b.updatedAt ? a.updatedAt > b.updatedAt : a.id < b.id
        }
        deck.reconcile(eligible.map(\.id))
        if previous != deck.focusedID { observeFocused() }
    }
    private func prioritizeNext() {
        let visible = Set(deck.order)
        let ranked = cards.filter { visible.contains($0.id) }.sorted { a, b in
            let ad = deferred[a.id] == a.latestCursor, bd = deferred[b.id] == b.latestCursor
            if ad != bd { return !ad }
            let ar = a.needsAttention(seen: seenCursor(a.id)), br = b.needsAttention(seen: seenCursor(b.id))
            if ar != br { return ar }
            let ai = deck.order.firstIndex(of: a.id) ?? 0, bi = deck.order.firstIndex(of: b.id) ?? 0
            return ai < bi
        }
        deck.prioritize(ranked.map(\.id))
    }
    func advance(reviewed: Bool) {
        guard let card = focused else { return }
        navigation.append((card.id, seen[card.id], deferred[card.id], filter)); if navigation.count > 50 { navigation.removeFirst() }
        if reviewed { seen[card.id] = card.latestCursor.rawValue; persist() }
        deferred[card.id] = card.latestCursor
        prioritizeNext()
        deck.advance(reviewed: reviewed ? card.latestCursor : nil)
        reconcile(); observeFocused()
        notice = nil
    }
    func back() {
        while let previous = navigation.popLast() {
            guard previous.id != focused?.id, cards.contains(where: { $0.id == previous.id }) else { continue }
            pinnedThreadID = previous.id
            seen[previous.id] = previous.seen; deferred[previous.id] = previous.deferred
            persist(); filter = previous.filter
            // A running agent may have finished since the last visit. Still bring it back.
            if !deck.order.contains(previous.id) { filter = .all }
            deck.focus(previous.id); observeFocused()
            return
        }
    }
    func refreshScheduledJobs() async {
        await startScheduledJobsRefresh()?.value
    }

    @discardableResult private func startScheduledJobsRefresh(initialListing: [AgentCard]? = nil) -> Task<Void, Never>? {
        guard connected else { return nil }
        // The model owns the read: opening the screen joins an existing prefetch,
        // and pushing a detail view does not cancel useful work for this account.
        if let schedulesTask { return schedulesTask }
        #if DEBUG
        if isDemo {
            scheduledJobs = DemoContent.scheduledJobs()
            scheduledJobAgents = Dictionary(uniqueKeysWithValues: cards.map { ($0.id, $0.title) })
            schedulesLoaded = true; schedulesError = nil
            return nil
        }
        #endif
        guard let client else { return nil }
        let epoch = generation
        schedulesLoading = true; schedulesError = nil; schedulesFailures = [:]
        let task = Task { [weak self] in
            guard let self else { return }
            let signpostID = OSSignpostID(log: accountPerformanceLog)
            os_signpost(.begin, log: accountPerformanceLog, name: "ScheduledJobsRefresh", signpostID: signpostID)
            defer {
                os_signpost(.end, log: accountPerformanceLog, name: "ScheduledJobsRefresh", signpostID: signpostID)
                if self.generation == epoch { self.schedulesLoading = false; self.schedulesTask = nil }
            }
            do {
                let agents: [AgentCard]
                if let initialListing { agents = initialListing }
                else { agents = try await client.list() }
                guard self.generation == epoch, !Task.isCancelled else { return }
                self.scheduledJobAgents = Dictionary(uniqueKeysWithValues: agents.map { ($0.id, $0.title) })
                // Keep cached jobs until their owner's read succeeds; a transient
                // failure must not make an existing schedule disappear.
                let ownerIDs = Set(agents.map(\.id))
                let retained = self.scheduledJobs.filter { ownerIDs.contains($0.agentID) }
                if self.scheduledJobs != retained { self.scheduledJobs = retained }
                let cachedOwners = Set(retained.map(\.agentID))
                var priority = cachedOwners
                if let focused = self.deck.focusedID { priority.insert(focused) }
                // Cached owners remain candidates even if a roster read raced a
                // new schedule; absent hints on older servers always mean read.
                let candidates = agents.filter { $0.mayHaveScheduledJobs || cachedOwners.contains($0.id) }
                let orderedIDs = candidates.filter { priority.contains($0.id) }.map(\.id)
                    + candidates.filter { !priority.contains($0.id) }.map(\.id)
                await client.scheduledJobs(for: orderedIDs) { [weak self] id, result in
                    await self?.receiveScheduledJobs(result, agentID: id, epoch: epoch)
                }
                guard self.generation == epoch, !Task.isCancelled else { return }
                self.schedulesLoaded = true
            } catch {
                guard self.generation == epoch, !Task.isCancelled else { return }
                self.schedulesError = error.localizedDescription
            }
        }
        schedulesTask = task
        return task
    }

    private func receiveScheduledJobs(_ result: Result<[ScheduledJob], Error>, agentID: String, epoch: UUID) {
        guard generation == epoch, !Task.isCancelled else { return }
        switch result {
        case .success(let received):
            let jobs = (scheduledJobs.filter { $0.agentID != agentID } + received).sorted {
                if $0.enabled != $1.enabled { return $0.enabled }
                if $0.nextRun != $1.nextRun { return ($0.nextRun ?? .distantFuture) < ($1.nextRun ?? .distantFuture) }
                return $0.id < $1.id
            }
            if scheduledJobs != jobs {
                if scheduledJobs.isEmpty && !jobs.isEmpty {
                    os_signpost(.event, log: accountPerformanceLog, name: "ScheduledJobsVisible")
                }
                scheduledJobs = jobs
            }
        case .failure(let error):
            schedulesFailures[agentID] = (scheduledJobAgents[agentID] ?? "Agent") + ": " + error.localizedDescription
            schedulesError = "Couldn’t load jobs for \(schedulesFailures.count) of \(scheduledJobAgents.count) agents.\n"
                + schedulesFailures.values.sorted().joined(separator: "\n")
        }
    }

    func selectScheduledChat(_ id: String) async throws {
        try Task.checkCancellation()
        let epoch = generation
        if !cards.contains(where: { $0.id == id }), let client {
            let listing = try await client.list()
            guard generation == epoch, !Task.isCancelled else { throw CancellationError() }
            guard let card = listing.first(where: { $0.id == id }) else { throw APIError.http(404) }
            if !cards.contains(where: { $0.id == id }) { cards.append(card) }
        }
        try Task.checkCancellation()
        guard generation == epoch, connected, cards.contains(where: { $0.id == id }) else { throw APIError.http(404) }
        select(id)
    }

    func select(_ id: String) {
        guard cards.contains(where: { $0.id == id }) else { return }
        if let card = focused, card.id != id {
            navigation.append((card.id, seen[card.id], deferred[card.id], filter))
            if navigation.count > 50 { navigation.removeFirst() }
        }
        pinnedThreadID = id
        filter = .all; deck.focus(id); observeFocused()
    }
    private func observeFocused(restart: Bool = false) {
        let changed = observedAgentID != deck.focusedID
        guard changed || restart else { return }
        if changed, let previous = observedAgentID, !isDemo, !events.isEmpty {
            // Preserve loaded history along with each tab's draft.
            tabHistories[previous] = TabHistory(events: events, cursor: cursor, hasOlder: hasOlder)
            recentTabs.removeAll { $0 == previous }; recentTabs.append(previous)
            while recentTabs.count > 8 { tabHistories[recentTabs.removeFirst()] = nil }
        }
        observedAgentID = deck.focusedID
        if let id = observedAgentID { cancelOverview(id) }
        streaming?.cancel(); streaming = nil; projection?.cancel(); projection = nil; observation = UUID(); loadingOlder = false
        threadError = nil
        if changed {
            rows = []; events = []; eventBytes = []; retainedBytes = 0; cursor = .zero
            olderBefore = nil; hasOlder = false; selectedTurn = ""
        }
        resumeOverview()
        guard let id = deck.focusedID else {
            threadLoading = false
            if connected && isActive { connection = isDemo ? "Demo" : "Connected" }
            return
        }
        if pendingCreations.contains(id) { threadLoading = false; return }
        if isDemo { rows = demoRows[id] ?? DemoContent.rows(id); connection = "Demo"; threadLoading = false; return }
        if changed, let cached = tabHistories[id] {
            events = cached.events; cursor = cached.cursor; hasOlder = cached.hasOlder
            measureEvents(); olderBefore = events.first?.cursor; rows = transcript(events)
        }
        threadLoading = rows.isEmpty
        guard let client, isActive else { return }
        let epoch = generation, token = observation
        // A frame received just before suspension may not have reached the
        // batched projection yet. Keep its text visible when observation resumes.
        if !changed && !events.isEmpty { projectEvents(id: id) }
        connection = "Connecting"
        streaming = Task { [weak self] in
            guard let self else { return }
            defer { if self.observation == token { self.streaming = nil } }
            var delay = 1
            var loaded = !self.events.isEmpty
            while !Task.isCancelled, self.generation == epoch, self.observation == token {
                let startedAt = Date()
                do {
                    if !loaded {
                        async let history = client.history(id)
                        async let state = client.state(id)
                        let (page, currentState) = try await (history, state)
                        guard self.generation == epoch, self.observation == token, !Task.isCancelled else { return }
                        self.events = page.events; self.hasOlder = page.hasMore; self.measureEvents(); self.cursor = page.latest; self.olderBefore = self.events.first?.cursor
                        self.rows = transcript(self.events)
                        if let index = self.cards.firstIndex(where: { $0.id == id }) {
                            var card = self.cards[index]
                            try card.apply(state: currentState); card.apply(events: self.events, transcriptRows: self.rows)
                            self.historyCursors[id] = max(self.historyCursors[id] ?? .zero, page.latest)
                            if self.cards[index] != card { self.cards[index] = card }
                            self.reconcilePending(id: id, events: self.events, state: card)
                        }
                        self.threadLoading = false; loaded = true
                    }
                    self.streamReceivedFrame = false
                    try await client.stream(id, after: self.cursor) { [weak self] frame in
                        await self?.receive(frame, id: id, epoch: epoch, token: token)
                    }
                } catch {
                    guard !Task.isCancelled, self.generation == epoch, self.observation == token else { return }
                    if let apiError = error as? APIError, apiError == .agentDeleting || apiError == .http(404) {
                        self.forgetUnavailableAgent(id); return
                    }
                    if let apiError = error as? APIError, apiError == .http(401) || apiError == .http(403) {
                        self.connection = "Sign in again"; self.error = apiError.localizedDescription; self.threadError = apiError.localizedDescription; self.threadLoading = false; return
                    }
                }
                guard !Task.isCancelled else { return }
                // EOF also loses observation. Short-lived handshakes must back
                // off just like failed requests, rather than spin every second.
                if Date().timeIntervalSince(startedAt) >= 30 { delay = 1 }
                self.threadLoading = false
                if !loaded { self.threadError = "Could not load this conversation. Reconnecting…" }
                self.connection = "Reconnecting"
                do { try await Task.sleep(for: .seconds(delay)) } catch { return }
                delay = min(delay * 2, 15)
            }
        }
    }
    func overviewRows(for id: String) -> [TranscriptRow] {
        if focused?.id == id, !rows.isEmpty { return rows }
        if isDemo { return demoRows[id] ?? DemoContent.rows(id) }
        return overviewTranscripts[id] ?? cards.first(where: { $0.id == id })?.previewRows ?? []
    }
    func setOverviewVisible(_ id: String, visible: Bool) {
        if visible { overviewVisible.insert(id); startOverview(id) }
        else { overviewVisible.remove(id); cancelOverview(id); overviewTranscripts[id] = nil }
    }
    func stopOverview() {
        overviewVisible.removeAll(); suspendOverview(); overviewTranscripts = [:]
    }
    private func suspendOverview() {
        for id in Array(overviewTasks.keys) { cancelOverview(id) }
    }
    private func cancelOverview(_ id: String) {
        overviewTasks.removeValue(forKey: id)?.cancel(); overviewTokens[id] = nil
        overviewProjections.removeValue(forKey: id)?.cancel()
        overviewEvents[id] = nil; overviewBytes[id] = nil; overviewByteCounts[id] = nil
    }
    private func resumeOverview() {
        for id in overviewVisible { startOverview(id) }
    }
    private func startOverview(_ id: String) {
        guard !isDemo, connected, isActive, id != observedAgentID,
              !pendingCreations.contains(id), overviewTasks[id] == nil,
              cards.contains(where: { $0.id == id }), let client else { return }
        let epoch = generation, token = UUID()
        overviewTokens[id] = token
        overviewTasks[id] = Task { [weak self] in
            guard let self else { return }
            var delay = 1, loaded = false
            var position = Cursor.zero
            while !Task.isCancelled, self.generation == epoch, self.overviewTokens[id] == token {
                let started = Date()
                do {
                    if !loaded {
                        async let history = client.history(id)
                        async let state = client.state(id)
                        let (page, current) = try await (history, state)
                        guard self.generation == epoch, self.overviewTokens[id] == token, !Task.isCancelled else { return }
                        self.overviewEvents[id] = page.events
                        self.overviewBytes[id] = page.events.map { (try? JSONEncoder().encode($0.data).count) ?? 0 }
                        self.overviewByteCounts[id] = self.overviewBytes[id]?.reduce(0, +) ?? 0
                        self.trimOverview(id)
                        if let index = self.cards.firstIndex(where: { $0.id == id }) { try self.cards[index].apply(state: current) }
                        self.projectOverview(id); position = page.latest; loaded = true
                    }
                    try await client.stream(id, after: position) { [weak self] frame in
                        await self?.receiveOverview(frame, id: id, epoch: epoch, token: token)
                    }
                } catch {
                    guard self.generation == epoch, self.overviewTokens[id] == token, !Task.isCancelled else { return }
                    if let apiError = error as? APIError, apiError == .agentDeleting || apiError == .http(404) {
                        self.forgetUnavailableAgent(id); return
                    }
                    if let index = self.cards.firstIndex(where: { $0.id == id }) { self.cards[index].error = error.localizedDescription }
                    if let apiError = error as? APIError, apiError == .http(401) || apiError == .http(403) { return }
                }
                position = max(position, self.overviewEvents[id]?.last?.cursor ?? .zero)
                if Date().timeIntervalSince(started) >= 30 { delay = 1 }
                do { try await Task.sleep(for: .seconds(delay)) } catch { return }
                delay = min(delay * 2, 15)
            }
        }
    }
    private func receiveOverview(_ frame: SSEFrame, id: String, epoch: UUID, token: UUID) {
        guard generation == epoch, overviewTokens[id] == token,
              let event = frame.event, event.cursor > (overviewEvents[id]?.last?.cursor ?? .zero) else { return }
        overviewEvents[id, default: []].append(event)
        overviewBytes[id, default: []].append(frame.payloadBytes)
        overviewByteCounts[id, default: 0] += frame.payloadBytes
        trimOverview(id)
        if overviewProjections[id] == nil {
            overviewProjections[id] = Task { [weak self] in
                do { try await Task.sleep(for: .milliseconds(100)) } catch { return }
                guard let self, self.generation == epoch, self.overviewTokens[id] == token else { return }
                self.projectOverview(id); self.overviewProjections[id] = nil
            }
        }
    }
    private func trimOverview(_ id: String) {
        while (overviewEvents[id]?.count ?? 0) > 1,
              (overviewEvents[id]?.count ?? 0) > 512 || (overviewByteCounts[id] ?? 0) > 8 * 1024 * 1024 {
            overviewEvents[id]?.removeFirst()
            overviewByteCounts[id, default: 0] -= overviewBytes[id]?.removeFirst() ?? 0
        }
    }
    private func projectOverview(_ id: String) {
        let history = overviewEvents[id] ?? [], projected = transcript(overviewEvents[id] ?? [])
        if overviewTranscripts[id] != projected { overviewTranscripts[id] = projected }
        if let index = cards.firstIndex(where: { $0.id == id }) {
            var card = cards[index]
            card.apply(events: history, transcriptRows: projected); card.error = nil
            historyCursors[id] = max(historyCursors[id] ?? .zero, card.appliedHistoryCursor)
            if cards[index] != card { cards[index] = card }
        }
    }
    private func receive(_ frame: SSEFrame, id: String, epoch: UUID, token: UUID) {
        guard generation == epoch, observation == token else { return }
        if let event = frame.event, event.cursor > cursor {
            events.append(event)
            reconcilePending(id: id, events: [event])
            let bytes = frame.payloadBytes
            eventBytes.append(bytes); retainedBytes += bytes
            // Token arrival never re-encodes the complete transcript on the main actor.
            while events.count > 1 && (events.count > 512 || retainedBytes > 16 * 1024 * 1024) {
                events.removeFirst(); retainedBytes -= eventBytes.removeFirst(); hasOlder = true
            }
            olderBefore = events.first?.cursor
            if projection == nil {
                projection = Task { [weak self] in
                    do { try await Task.sleep(for: .milliseconds(100)) } catch { return }
                    guard let self, self.generation == epoch, self.observation == token else { return }
                    self.projectEvents(id: id)
                    self.projection = nil
                }
            }
        }
        if let position = frame.cursor { cursor = max(cursor, position) }
        // After a failure, the initial cursor alone does not establish a healthy
        // stream. Its next event/keepalive confirms recovery without flickering
        // Live for every short-lived reconnect handshake.
        if connection != "Live", streamReceivedFrame || connection != "Reconnecting" { connection = "Live" }
        streamReceivedFrame = true
        if threadError != nil { threadError = nil }
        if threadLoading { threadLoading = false }
    }
    private func projectEvents(id: String) {
        let projected = transcript(events)
        if rows != projected { rows = projected }
        if let index = cards.firstIndex(where: { $0.id == id }) {
            var card = cards[index]
            card.apply(events: events, transcriptRows: projected)
            historyCursors[id] = max(historyCursors[id] ?? .zero, events.last?.cursor ?? .zero)
            if cards[index] != card { cards[index] = card }
        }
    }
    private func measureEvents() {
        eventBytes = events.map { (try? JSONEncoder().encode($0.data).count) ?? 0 }
        retainedBytes = eventBytes.reduce(0, +)
        while events.count > 1 && retainedBytes > 16 * 1024 * 1024 {
            events.removeFirst(); retainedBytes -= eventBytes.removeFirst(); hasOlder = true
        }
    }
    func loadOlder() async {
        if isDemo, ProcessInfo.processInfo.environment["NANOCODEX_DEMO_LONG_THREAD"] == "1", let id = focused?.id, hasOlder {
            guard !loadingOlder else { return }
            loadingOlder = true
            let delay = Int(ProcessInfo.processInfo.environment["NANOCODEX_DEMO_HISTORY_DELAY_MS"] ?? "600") ?? 600
            try? await Task.sleep(for: .milliseconds(max(0, delay)))
            guard focused?.id == id else { return }
            rows.insert(contentsOf: (-12..<0).map { .init(id: "older-\($0)", role: "Agent", text: "Earlier note \($0 + 13). Context retained before the current work.") }, at: 0)
            demoRows[id] = rows; hasOlder = false; loadingOlder = false
            return
        }
        guard let client, let id = focused?.id, let before = olderBefore, hasOlder, !loadingOlder else { return }
        let token = observation
        loadingOlder = true
        defer { if token == observation { loadingOlder = false } }
        do {
            let page = try await client.history(id, before: before)
            guard token == observation else { return }
            let known = Set(events.map(\.cursor.rawValue))
            events.insert(contentsOf: page.events.filter { !known.contains($0.cursor.rawValue) }, at: 0)
            // Older history is explicitly loaded; cap at 2,048 events and explain the limit.
            if events.count > 2048 { events = Array(events.suffix(2048)); notice = "History limit reached. Open the web conversation for earlier messages."; hasOlder = false }
            else { hasOlder = page.hasMore }
            measureEvents(); olderBefore = events.first?.cursor; rows = transcript(events)
            if olderBefore == before { hasOlder = false; notice = "History limit reached. Earlier messages remain available on the service." }
        } catch { if token == observation { self.threadError = error.localizedDescription } }
    }
    func voiceConfiguration(agentID: String) async throws -> VoiceConfiguration {
        guard connected, !isDemo else { throw ManagedError(code: "account_required", message: "Sign in to use interactive voice.") }
        let agentID = try await readyAgent(agentID)
        try Task.checkCancellation()
        guard let card = cards.first(where: { $0.id == agentID }),
              let credential = accountCredential, let url = URL(string: credential.origin) else { throw APIError.invalidResponse }
        voice.transcriptFeed.begin(conversationID: agentID, durableRows: focused?.id == agentID ? rows : [], after: max(cursor, card.latestCursor))
        return VoiceConfiguration(baseURL: url, apiKey: credential.apiKey, agentID: agentID, conversationTitle: card.title)
    }
    // Reserve identity and the busy slot synchronously at the tap, before a swipe
    // or another tap can change focus. The server owns the queued follow-up.
    func send() -> Bool {
        guard let card = focused, canSend else { return false }
        let request = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !request.isEmpty || !focusedAttachments.isEmpty else { return false }
        refreshContext()
        if let contextError, contextEnabled || !(selectedContext[card.id] ?? []).isEmpty { error = contextError; return false }
        let captured = contextForAgent(card.id)
        let input: String
        do {
            let context = try ContextPrompt.render(captured)
            guard captured.count <= 24, context.utf8.count <= 96 * 1024 else {
                error = "Too much context selected. Remove some captures from this message and try again."
                return false
            }
            input = captured.isEmpty ? request : context + "\n\nMy request:\n" + request
        }
        catch { self.error = error.localizedDescription; return false }
        let attachments = focusedAttachments
        let textBytes = (try? JSONEncoder().encode(input).count) ?? Int.max
        let imageBytes = attachments.reduce(0) { $0 + $1.promptByteCount }
        guard textBytes < 1024 * 1024 - imageBytes - 256 else {
            error = APIError.messageTooLarge.localizedDescription; return false
        }
        let predecessor = focusedPending.last?.id ?? focusedTurn
        let message = PendingMessage(agentID: card.id, input: input, predecessor: predecessor, contextIDs: captured.map(\.id), attachments: attachments.isEmpty ? nil : attachments)
        attachmentDrafts[card.id] = nil; attachmentErrors[card.id] = nil
        pending.append(message); drafts[card.id] = ""; selectedContext[card.id] = nil; excludedContext[card.id] = nil; busy.insert(card.id); notice = nil; persist()
        let epoch = generation
        if deviceHandEnabled, !isDemo { startHandTask(message, epoch: epoch) }
        else { Task { await submit(message, epoch: epoch) } }
        return true
    }
    func retryPending(_ id: String) {
        guard let index = pending.firstIndex(where: { $0.id == id }), pending[index].phase == .failed,
              !busy.contains(pending[index].agentID) else { return }
        pending[index].phase = .submitting; pending[index].error = nil
        let message = pending[index], epoch = generation
        busy.insert(message.agentID); persist()
        if deviceHandEnabled, !isDemo { startHandTask(message, epoch: epoch) }
        else { Task { await submit(message, epoch: epoch) } }
    }

    @discardableResult
    private func startHandTask(_ message: PendingMessage, epoch: UUID,
                               progress: Progress = Progress(totalUnitCount: 1),
                               runtimeProvided: Bool = false) -> Task<String, Error> {
        handTasks.start(id: message.id, title: cards.first(where: { $0.id == message.agentID })?.title ?? "Agent working",
                        progress: progress, runtimeProvided: runtimeProvided, cancel: { [weak self] in
            guard let self, self.generation == epoch else { return }
            self.prepareHandForBackground()
            self.stop(agentID: self.resolvedAgentID(message.agentID), turnID: message.id)
        }) { [weak self] progress in
            guard let self, self.generation == epoch else { throw CancellationError() }
            await self.submit(message, epoch: epoch)
            try Task.checkCancellation()
            guard self.generation == epoch, let client = self.client else { throw CancellationError() }
            if let failed = self.pending.first(where: { $0.id == message.id && $0.phase == .failed }) {
                throw HandTaskError.delivery(failed.error ?? "Delivery unconfirmed. Retry in Nanocodex.")
            }
            let agentID = self.resolvedAgentID(message.agentID)
            // Start at this turn's admission, not the conversation's entire history.
            let admission = try await client.turn(agentID: agentID, turnID: message.id)
            guard self.generation == epoch, !Task.isCancelled else { throw CancellationError() }
            guard let accepted = Cursor(rawValue: admission["accepted_cursor"].string) else { throw APIError.invalidResponse }
            self.handTasks.beginObservation(id: message.id, after: accepted)
            let stream = Task { [weak self] in
                while !Task.isCancelled {
                    guard let self, self.generation == epoch else { return }
                    do {
                        try await client.stream(agentID, after: self.handTasks.cursor(id: message.id)) { [weak self] frame in
                            if let event = frame.event { await self?.receiveHandTaskEvent(event, id: message.id, epoch: epoch) }
                        }
                    } catch { if Task.isCancelled { return } }
                    do { try await Task.sleep(for: .seconds(2)) } catch { return }
                }
            }
            defer { stream.cancel() }
            while self.generation == epoch {
                try Task.checkCancellation()
                do {
                    let turn = try await client.turn(agentID: agentID, turnID: message.id)
                    guard self.generation == epoch else { throw CancellationError() }
                    switch turn["state"].string {
                    case "completed": return turn["terminal"]["final_message"].string
                    case "cancelled": throw CancellationError()
                    case "failed": throw HandTaskError.delivery("The agent task failed. Open the conversation in Nanocodex for details.")
                    default: break
                    }
                } catch let error as APIError {
                    // Transient network failures do not resubmit the turn.
                    if [.http(401), .http(403), .http(404), .agentDeleting].contains(error) { throw error }
                } catch let error as URLError {
                    if error.code == .cancelled { throw CancellationError() }
                }
                try await Task.sleep(for: .seconds(2))
            }
            throw CancellationError()
        }
    }

    private func receiveHandTaskEvent(_ event: AgentEvent, id: String, epoch: UUID) {
        guard generation == epoch else { return }
        handTasks.receive(event, id: id)
    }

    func shortcutAgents() async throws -> [HandAgentEntity] {
        await start()
        while signingIn { try await Task.sleep(for: .milliseconds(100)) }
        guard connected, !isDemo, let client else { throw HandTaskError.signIn }
        let epoch = generation, account = scope
        let agents = try await client.list()
        guard generation == epoch else { throw CancellationError() }
        return agents.map { HandAgentEntity(agentID: $0.id, account: account, title: $0.title) }
    }

    func runShortcutTask(agent: HandAgentEntity, input: String, id: String,
                         progress: Progress = Progress(totalUnitCount: 1),
                         runtimeProvided: Bool = false,
                         isCancelled: () -> Bool = { false }) async throws -> Task<String, Error> {
        _ = try await shortcutAgents()
        try Task.checkCancellation()
        guard !isCancelled() else { throw CancellationError() }
        guard deviceHandEnabled else { throw HandTaskError.disabled }
        guard agent.account == scope else { throw HandTaskError.accountChanged }
        let input = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !input.isEmpty else { throw HandTaskError.emptyRequest }
        let message = PendingMessage(agentID: agent.agentID, input: input, predecessor: "", id: id)
        _ = try message.submission.requestSpec()
        pending.append(message); busy.insert(message.agentID); persist()
        return startHandTask(message, epoch: generation, progress: progress, runtimeProvided: runtimeProvided)
    }

    func cancelShortcutTask(id: String, agent: HandAgentEntity, stopTurn: Bool) {
        // The OS can deliver Swift cancellation before the reason callback.
        // A later explicit Stop must still fence the exact remote turn.
        if stopTurn, connected, scope == agent.account { stop(agentID: agent.agentID, turnID: id) }
        handTasks.cancel(id: id, stopTurn: false)
    }

    private func submit(_ message: PendingMessage, epoch: UUID) async {
        defer {
            let agentID = resolvedAgentID(message.agentID)
            if generation == epoch, !pending.contains(where: { $0.agentID == agentID && $0.id != message.id && $0.phase == .submitting }) {
                busy.remove(agentID)
            }
        }
        do {
            _ = try await readyAgent(message.agentID)
            guard generation == epoch, let message = pending.first(where: { $0.id == message.id }), message.phase != .cancelling else { return }
            var command = message.submission
            if let attachments = message.attachments, !attachments.isEmpty {
                let store = try AttachmentStore(scope: scope)
                guard let client else { throw APIError.invalidCredential }
                // Verify saved drafts before uploading, including legacy drafts.
                _ = try await Task.detached(priority: .userInitiated) { try store.content(for: attachments) }.value
                for attachment in attachments {
                    if attachment.isVideo {
                        let source = try store.url(for: attachment)
                        let path = try await client.uploadVideo(agentID: message.agentID, attachment: attachment, source: source) { [weak self] in
                            await MainActor.run {
                                guard let self else { return true }
                                return self.generation != epoch || self.pending.first(where: { $0.id == message.id }).map { $0.phase == .cancelling } != false
                            }
                        }
                        command.images += try attachment.originalVideoContent(path: path)
                    } else {
                        command.images += try await Task.detached(priority: .userInitiated) { try store.content(for: [attachment]) }.value
                    }
                }
            }
            guard generation == epoch, let current = pending.first(where: { $0.id == message.id }), current.phase != .cancelling else { return }
            let receipt = try await execute(command)
            guard generation == epoch else { return }
            guard receipt["turn_id"].string == message.id else { throw APIError.invalidResponse }
            guard pending.contains(where: { $0.id == message.id }) else { return }
            try recordContextDelivery(message)
            if let index = pending.firstIndex(where: { $0.id == message.id }) {
                try pending[index].acknowledge(receipt)
            }
            if ["completed", "cancelled", "failed"].contains(receipt["state"].string) {
                pending.removeAll { $0.id == message.id }; releaseAttachments(message.attachments ?? [])
            }
            persist()
            if isDemo {
                demoAdmit(message)
                notice = nil
            } else { Task { await refreshControlledAgent(message.agentID, epoch: epoch) } }
        } catch {
            guard generation == epoch else { return }
            if let index = pending.firstIndex(where: { $0.id == message.id }) {
                guard pending[index].phase != .cancelling else { return }
                pending[index].phase = .failed
                pending[index].error = error.localizedDescription + " Retry keeps the same message and attachments."
                persist()
            }
        }
    }
    func steerNow(_ id: String) {
        guard let message = pending.first(where: { $0.id == id }),
              let command = steeringTarget(message),
              let index = pending.firstIndex(where: { $0.id == id }) else { return }
        pending[index].predecessor = command.turnID
        pending[index].phase = .starting; pending[index].error = nil
        requestCancellation(agentID: message.agentID, turnID: command.turnID)
    }
    func cancelPending(_ id: String) {
        guard let index = pending.firstIndex(where: { $0.id == id }) else { return }
        // Nothing can reach the service before creation resolves.
        if pendingCreations.contains(pending[index].agentID) {
            busy.remove(pending[index].agentID)
            removeCancelledPending(id); persist(); return
        }
        pending[index].phase = .cancelling; pending[index].error = nil
        requestCancellation(agentID: pending[index].agentID, turnID: id)
    }
    private func requestCancellation(agentID: String, turnID: String) {
        guard connected, !turnID.isEmpty else { return }
        prepareHandForBackground()
        handTasks.cancel(id: turnID, stopTurn: false)
        let intent = PendingTurnCancellation(agentID: agentID, turnID: turnID)
        if let index = cancellations.firstIndex(where: { $0.id == intent.id }) {
            cancellations[index].error = nil
        } else { cancellations.append(intent) }
        persist(); startCancellation(intent)
    }
    private func resumeCancellations(restart: Bool = false) {
        for intent in cancellations where intent.error == nil { startCancellation(intent, restart: restart) }
    }
    private func startCancellation(_ intent: PendingTurnCancellation, restart: Bool = false) {
        guard hasHandExecutionTime else { return }
        let epoch = generation
        cancellationTasks.start(intent.id, restart: restart) { [self] in
            guard generation == epoch, !Task.isCancelled else { return }
            do {
                let receipt = try await execute(intent.command)
                guard generation == epoch, !Task.isCancelled else { return }
                guard receipt["turn_id"].string == intent.turnID else { throw APIError.invalidResponse }
                guard let index = cancellations.firstIndex(where: { $0.id == intent.id }) else { return }
                cancellations[index].acknowledged = true
                if let cancelled = pending.first(where: { $0.agentID == intent.agentID && $0.id == intent.turnID }) {
                    rebaseSuccessors(of: cancelled)
                }
                persist()
                if isDemo {
                    finishCancellation(intent); return
                }
                if finishCancellationIfTerminal(intent, receipt: receipt) { return }
                var schedule = TurnCancellationPollSchedule()
                // Poll this exact turn. An account-wide refresh may take many
                // seconds and active_turns cannot identify a pre-admission stop.
                while generation == epoch, hasHandExecutionTime, cancellations.contains(where: { $0.id == intent.id }) {
                    try Task.checkCancellation()
                    guard let client else { return }
                    do {
                        let current = try await client.turn(agentID: intent.agentID, turnID: intent.turnID)
                        guard generation == epoch, !Task.isCancelled else { return }
                        guard current["turn_id"].string == intent.turnID else { throw APIError.invalidResponse }
                        if finishCancellationIfTerminal(intent, receipt: current) { return }
                        // Terminal stream/history events still finish immediately.
                        // Unchanged durable cancellation need not wake HTTP every second.
                        try await Task.sleep(for: schedule.delay(after: current))
                    } catch APIError.http(404) {
                        // /cancel durably fences this ID even if /turns never
                        // admitted it. A later in-flight POST cannot run it.
                        guard generation == epoch, !Task.isCancelled else { return }
                        finishCancellation(intent); return
                    }
                }
            } catch {
                guard generation == epoch, !Task.isCancelled else { return }
                guard let index = cancellations.firstIndex(where: { $0.id == intent.id }) else { return }
                cancellations[index].error = "Stop unconfirmed. Tap to retry."
                for index in pending.indices where pending[index].agentID == intent.agentID {
                    if pending[index].id == intent.turnID { pending[index].error = "Stop unconfirmed. Tap × to retry." }
                    else if pending[index].predecessor == intent.turnID, pending[index].phase == .starting {
                        pending[index].phase = .queued
                        pending[index].error = "Cancellation unconfirmed. The queued message is retained; try again."
                    }
                }
                persist()
            }
        }
    }
    @discardableResult
    private func finishCancellationIfTerminal(_ intent: PendingTurnCancellation, receipt: JSON) -> Bool {
        guard intent.isTerminal(receipt: receipt) else { return false }
        let event = try? AgentEvent(receipt["terminal"], cursor: receipt["terminal_cursor"].string)
        finishCancellation(intent, event: event)
        return true
    }
    private func finishCancellation(_ intent: PendingTurnCancellation, event: AgentEvent? = nil) {
        guard cancellations.contains(where: { $0.id == intent.id }) else { return }
        cancellations.removeAll { $0.id == intent.id }
        cancellationTasks.cancel(intent.id)
        if isDemo { demoFinish(agentID: intent.agentID, turnID: intent.turnID) }
        else {
            if let index = cards.firstIndex(where: { $0.id == intent.agentID }) {
                if let event { cards[index].apply(events: [event]) }
                else {
                    cards[index].activeTurns.removeAll { $0 == intent.turnID }
                    cards[index].status = cards[index].isRunning ? "Running" : "Stopped"
                }
            }
            if pending.contains(where: { $0.agentID == intent.agentID && $0.id == intent.turnID }) {
                removeCancelledPending(intent.turnID)
            }
        }
        for index in pending.indices where pending[index].agentID == intent.agentID && pending[index].predecessor == intent.turnID && pending[index].phase == .starting {
            pending[index].phase = .queued; pending[index].error = nil
        }
        persist()
    }
    private func refreshControlledAgent(_ id: String, epoch: UUID) async {
        guard let client, generation == epoch else { return }
        do {
            async let state = client.state(id)
            async let history = client.history(id)
            let (current, page) = try await (state, history)
            guard generation == epoch, let index = cards.firstIndex(where: { $0.id == id }) else { return }
            var card = cards[index]
            try card.apply(state: current); card.apply(events: page.events)
            cards[index] = card
            reconcilePending(id: id, events: page.events, state: card)
        } catch { /* The observer/poll loop retains recovery ownership. */ }
    }
    private func removeCancelledPending(_ id: String) {
        guard let cancelled = pending.first(where: { $0.id == id }) else { return }
        rebaseSuccessors(of: cancelled)
        pending.removeAll { $0.id == id }; releaseAttachments(cancelled.attachments ?? [])
    }
    private func rebaseSuccessors(of cancelled: PendingMessage) {
        for index in pending.indices where pending[index].agentID == cancelled.agentID && pending[index].predecessor == cancelled.id {
            pending[index].predecessor = cancelled.predecessor
        }
    }
    private func reconcilePending(id: String, events: [AgentEvent], state: AgentCard? = nil) {
        let previousCount = pending.count
        for event in events where ["turn_completed", "turn_cancelled", "turn_failed"].contains(event.type) {
            if let intent = cancellation(agentID: id, turnID: event.turnID) { finishCancellation(intent, event: event) }
        }
        for event in events where event.type == "turn_cancelled" {
            if pending.contains(where: { $0.agentID == id && $0.id == event.turnID }) { removeCancelledPending(event.turnID) }
        }
        let finished = pending.filter { message in
            message.agentID == id && (message.hasStarted(in: events)
                || state.map { message.hasFinished(activeTurns: $0.activeTurns, stateCursor: $0.stateCursor) } == true)
        }
        for message in finished {
            do {
                try recordContextDelivery(message)
                pending.removeAll { $0.id == message.id }; releaseAttachments(message.attachments ?? [])
            } catch { contextError = error.localizedDescription }
        }
        if pending.count != previousCount { persist() }
    }
    private func restorePending() {
        if let data = UserDefaults.standard.data(forKey: "inbox.pending." + scope),
           let saved = try? JSONDecoder().decode([PendingMessage].self, from: data) {
            pending = saved
            for index in pending.indices { pending[index].restore() }
        }
        if let data = UserDefaults.standard.data(forKey: "inbox.cancellations." + scope) {
            cancellations = (try? JSONDecoder().decode([PendingTurnCancellation].self, from: data)) ?? []
            for index in cancellations.indices { cancellations[index].error = nil }
        }
        // Migrate older saved in-flight controls without losing the user's Stop.
        for message in pending where message.phase == .starting || message.phase == .cancelling {
            let turnID = message.phase == .cancelling ? message.id : message.predecessor
            let intent = PendingTurnCancellation(agentID: message.agentID, turnID: turnID)
            if !turnID.isEmpty, !cancellations.contains(where: { $0.id == intent.id }) { cancellations.append(intent) }
        }
    }
    private func execute(_ command: AgentCommand) async throws -> JSON {
        if isDemo {
            let delayKey = command.kind == .stop ? "NANOCODEX_DEMO_CANCEL_DELAY_MS" : "NANOCODEX_DEMO_DELAY_MS"
            let delay = Int(ProcessInfo.processInfo.environment[delayKey] ?? ProcessInfo.processInfo.environment["NANOCODEX_DEMO_DELAY_MS"] ?? "200") ?? 200
            try await Task.sleep(for: .milliseconds(delay))
            let fault = command.kind == .stop ? "cancel" : "submit"
            if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_FAIL_ONCE"] == fault, demoFaults.insert(fault).inserted {
                throw APIError.http(503)
            }
            return .object(["turn_id": .string(command.kind == .followUp ? command.requestID : command.turnID), "state": .string(command.kind == .stop ? "cancelling" : "accepted")])
        }
        guard let client else { throw APIError.invalidResponse }
        return try await client.command(command)
    }
    private func demoAdmit(_ message: PendingMessage) {
        guard let index = cards.firstIndex(where: { $0.id == message.agentID }) else { return }
        let waiting = !message.predecessor.isEmpty && cards[index].activeTurns.contains(message.predecessor)
        if !cards[index].activeTurns.contains(message.id) { cards[index].activeTurns.append(message.id) }
        cards[index].status = "Running"
        cards[index].updatedAt = Date().timeIntervalSince1970 * 1000
        var history = demoRows[message.agentID] ?? DemoContent.rows(message.agentID)
        if !history.contains(where: { $0.id == message.id }) { history.append(.init(id: message.id, role: "You", text: message.input)) }
        demoRows[message.agentID] = history
        if focused?.id == message.agentID { rows = history }
        if !waiting { pending.removeAll { $0.id == message.id } }
        else {
            let epoch = generation
            let delay = Int(ProcessInfo.processInfo.environment["NANOCODEX_DEMO_COMPLETE_AFTER_MS"] ?? "15000") ?? 15000
            Task {
                try? await Task.sleep(for: .milliseconds(delay))
                guard generation == epoch, cards.contains(where: { $0.id == message.agentID && $0.activeTurns.contains(message.predecessor) }) else { return }
                demoFinish(agentID: message.agentID, turnID: message.predecessor)
            }
        }
        persist()
    }
    private func demoFinish(agentID: String, turnID: String) {
        guard let index = cards.firstIndex(where: { $0.id == agentID }) else { return }
        cards[index].activeTurns.removeAll { $0 == turnID }
        cards[index].updatedAt = Date().timeIntervalSince1970 * 1000
        // Cancelling a queued item must not start its successor while an older
        // turn is still running. Rebase the successor onto that older turn.
        let wasQueued = pending.contains { $0.agentID == agentID && $0.id == turnID }
        if wasQueued { removeCancelledPending(turnID) }
        if let next = pending.first(where: { $0.agentID == agentID && $0.predecessor == turnID && $0.phase != .failed && $0.phase != .submitting }) {
            pending.removeAll { $0.id == next.id }
            var history = demoRows[agentID] ?? DemoContent.rows(agentID)
            history.append(.init(id: "started-" + next.id, role: "Agent", text: "Working on: " + next.input, running: true))
            demoRows[agentID] = history; cards[index].preview = "Working on: " + next.input
            if focused?.id == agentID { rows = history }
        }
        cards[index].status = cards[index].isRunning ? "Running" : "Stopped"
        persist(); reconcile()
    }
    func stop(agentID: String, turnID: String) {
        guard !turnID.isEmpty else { return }
        if pending.contains(where: { $0.agentID == agentID && $0.id == turnID }) { cancelPending(turnID) }
        else { requestCancellation(agentID: agentID, turnID: turnID) }
    }
    func retry() async { if let id = focused?.id, let command = retries[id], command.kind == .followUp { await perform(command) } }
    private func perform(_ command: AgentCommand) async {
        guard !busy.contains(command.agentID) else { return }
        let epoch = generation
        busy.insert(command.agentID); error = nil
        defer { if generation == epoch { busy.remove(command.agentID) } }
        do {
            _ = try await execute(command)
            guard generation == epoch else { return }
            if isDemo, command.kind == .stop { demoFinish(agentID: command.agentID, turnID: command.turnID) }
            if command.kind != .stop, drafts[command.agentID] == command.input { drafts[command.agentID] = ""; persist() }
            retries.removeValue(forKey: command.agentID)
            notice = command.kind == .steer ? "Direction sent" : command.kind == .stop ? "Stop requested" : "Follow-up accepted"
            await refresh()
        } catch {
            guard generation == epoch else { return }
            if command.kind == .followUp { retries[command.agentID] = command }
            self.error = error.localizedDescription + (command.kind == .followUp ? " Retry the same follow-up to avoid sending it twice." : " The action was not confirmed; check the latest state before trying again.")
        }
    }
    func newAgent() {
        guard connected else { return }
        let id = "draft-" + UUID().uuidString
        pendingCreations.insert(id)
        cards.insert(newConversationCard(id), at: 0)
        if isDemo { demoRows[id] = [] }
        error = nil; notice = nil
        select(id); persist()
        prepareAgent(id)
    }
    private func newConversationCard(_ id: String) -> AgentCard {
        var card = AgentCard(id: id, title: "New agent", updatedAt: Date().timeIntervalSince1970 * 1000)
        card.checked = true; card.status = "Idle"; card.preview = "Send a message to begin."
        return card
    }
    func retryCreation() {
        guard let id = focused?.id, pendingCreations.contains(id) else { return }
        prepareAgent(id)
    }
    private func prepareAgent(_ id: String) {
        Task { _ = try? await readyAgent(id) }
    }
    private func readyAgent(_ localID: String) async throws -> String {
        if let id = createdAgentIDs[localID] { return id }
        guard pendingCreations.contains(localID) else { return localID }
        if let task = creationTasks[localID] { return try await task.value }
        let epoch = generation, client = client, demo = isDemo
        creationErrors[localID] = nil
        let task = Task { @MainActor () async throws -> String in
            do {
                let id: String
                if demo {
                    #if DEBUG
                    let delay = Int(ProcessInfo.processInfo.environment["NANOCODEX_DEMO_CREATE_DELAY_MS"] ?? "0") ?? 0
                    try await Task.sleep(for: .milliseconds(delay))
                    if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_FAIL_ONCE"] == "create", demoFaults.insert("create").inserted { throw APIError.http(503) }
                    #endif
                    id = "demo-" + localID
                } else {
                    guard let client else { throw APIError.invalidResponse }
                    id = try await client.create(requestID: localID)
                }
                guard generation == epoch else { throw CancellationError() }
                bindCreatedAgent(localID, to: id)
                creationTasks[localID] = nil
                return id
            } catch {
                if generation == epoch {
                    creationTasks[localID] = nil
                    creationErrors[localID] = error.localizedDescription
                }
                throw error
            }
        }
        creationTasks[localID] = task
        return try await task.value
    }
    private func bindCreatedAgent(_ localID: String, to id: String) {
        let wasFocused = deck.focusedID == localID
        createdAgentIDs[localID] = id
        // A concurrent roster can list the real agent before create returns.
        // Keep the placeholder's position and avoid duplicate SwiftUI identities.
        tabOrder = tabOrder.filter { $0 != id }.map { $0 == localID ? id : $0 }
        if overviewVisible.remove(localID) != nil { overviewVisible.insert(id) }
        cancelOverview(localID)
        if let value = drafts.removeValue(forKey: localID) { drafts[id] = value }
        if let value = attachmentDrafts.removeValue(forKey: localID) { attachmentDrafts[id] = value }
        if let value = attachmentErrors.removeValue(forKey: localID) { attachmentErrors[id] = value }
        if attachmentImports.remove(localID) != nil { attachmentImports.insert(id) }
        if let value = selectedContext.removeValue(forKey: localID) { selectedContext[id] = value }
        if let value = excludedContext.removeValue(forKey: localID) { excludedContext[id] = value }
        if busy.remove(localID) != nil { busy.insert(id) }
        for index in pending.indices where pending[index].agentID == localID {
            let old = pending[index]
            var rebound = PendingMessage(agentID: id, input: old.input, predecessor: old.predecessor, id: old.id, contextIDs: old.contextIDs, attachments: old.attachments)
            rebound.phase = old.phase; rebound.acceptedCursor = old.acceptedCursor; rebound.error = old.error
            pending[index] = rebound
        }
        if isDemo { demoRows[id] = demoRows.removeValue(forKey: localID) ?? [] }
        if pinnedThreadID == localID { pinnedThreadID = id }
        navigation = navigation.map { ($0.id == localID ? id : $0.id, $0.seen, $0.deferred, $0.filter) }
        cards = cards.filter { $0.id != id }.map { $0.id == localID ? newConversationCard(id) : $0 }
        for source in contextRoutes.keys where contextRoutes[source] == localID {
            do { try ContextStore.shared().route(source: source, agentID: id, scope: scope) }
            catch { contextError = error.localizedDescription }
        }
        refreshContext()
        pendingCreations.remove(localID); creationErrors[localID] = nil
        unlistedAgents.insert(id)
        // Rebind without navigating: a late response must never steal focus.
        deck.reconcile(deck.order.map { $0 == localID ? id : $0 })
        if wasFocused { deck.focus(id) }
        persist(); observeFocused()
    }
    private func restoreCreations() {
        pendingCreations = Set(UserDefaults.standard.stringArray(forKey: "inbox.creations." + scope) ?? [])
        cards.insert(contentsOf: pendingCreations.sorted().map(newConversationCard), at: 0)
    }
    private func persist() {
        guard !scope.isEmpty else { return }
        UserDefaults.standard.set(drafts, forKey: "inbox.drafts." + scope)
        if let data = try? JSONEncoder().encode(attachmentDrafts) { UserDefaults.standard.set(data, forKey: "inbox.attachments." + scope) }
        UserDefaults.standard.set(seen, forKey: "inbox.seen." + scope)
        UserDefaults.standard.set(selectedContext, forKey: "inbox.contextSelection." + scope)
        UserDefaults.standard.set(excludedContext, forKey: "inbox.contextExclusions." + scope)
        if let data = try? JSONEncoder().encode(pending) { UserDefaults.standard.set(data, forKey: "inbox.pending." + scope) }
        if let data = try? JSONEncoder().encode(cancellations) { UserDefaults.standard.set(data, forKey: "inbox.cancellations." + scope) }
        UserDefaults.standard.set(Array(pendingCreations), forKey: "inbox.creations." + scope)
        if isDemo {
            if let data = try? JSONEncoder().encode(demoRows) { UserDefaults.standard.set(data, forKey: "inbox.demoRows." + scope) }
            UserDefaults.standard.set(Dictionary(uniqueKeysWithValues: cards.map { ($0.id, $0.activeTurns) }), forKey: "inbox.demoTurns." + scope)
        }
    }
    #if DEBUG
    func demo() {
        reset(); isDemo = true; connected = true; connection = "Demo"
        scope = "demo." + (ProcessInfo.processInfo.environment["NANOCODEX_DEMO_PROFILE"] ?? "default")
        cards = DemoContent.cards()
        if let profile = ProcessInfo.processInfo.environment["NANOCODEX_DEMO_PROFILE"] {
            scope = "demo." + profile
            restorePending()
            drafts = UserDefaults.standard.dictionary(forKey: "inbox.drafts." + scope) as? [String: String] ?? [:]
            if let data = UserDefaults.standard.data(forKey: "inbox.demoRows." + scope) { demoRows = (try? JSONDecoder().decode([String: [TranscriptRow]].self, from: data)) ?? [:] }
            if let turns = UserDefaults.standard.dictionary(forKey: "inbox.demoTurns." + scope) as? [String: [String]] {
                for index in cards.indices { cards[index].activeTurns = turns[cards[index].id] ?? cards[index].activeTurns }
            }
        }
        activateContext()
        restoreCreations()
        reconcile(); observeFocused()
        for id in pendingCreations { prepareAgent(id) }
        resumeCancellations()
        if ProcessInfo.processInfo.arguments.contains("--demo"),
           ProcessInfo.processInfo.environment["NANOCODEX_DEMO_VOICE"] == "1", let card = focused {
            voice.transcriptFeed.begin(conversationID: card.id, durableRows: rows, after: card.latestCursor)
            voice.startTranscriptPreview(agentID: card.id, conversationTitle: card.title, connecting: true)
            let epoch = generation
            demoVoice = Task { [weak self] in
                do { try await Task.sleep(for: .seconds(12)) } catch { return }
                guard let self, self.isDemo, self.generation == epoch, self.voice.isEngaged else { return }
                self.voice.activateTranscriptPreview()
                for (delay, event) in DemoContent.voiceTranscript {
                    do { try await Task.sleep(for: delay) } catch { return }
                    guard self.isDemo, self.voice.isEngaged, self.voice.conversationID == card.id else { return }
                    self.voice.receiveTranscriptPreview(event)
                }
                do { try await Task.sleep(for: .seconds(4)) } catch { return }
                guard self.isDemo, self.generation == epoch else { return }
                var history = self.demoRows[card.id] ?? (self.focused?.id == card.id ? self.rows : DemoContent.rows(card.id))
                history.append(contentsOf: DemoContent.voiceDurableRows)
                self.demoRows[card.id] = history
                if self.focused?.id == card.id { self.rows = history }
            }
        }
    }
    #endif

    private func activateContext() {
        do {
            try ContextStore.shared().activate(scope)
            selectedContext = UserDefaults.standard.dictionary(forKey: "inbox.contextSelection." + scope) as? [String: [String]] ?? [:]
            excludedContext = UserDefaults.standard.dictionary(forKey: "inbox.contextExclusions." + scope) as? [String: [String]] ?? [:]
            refreshContext()
        } catch { contextError = error.localizedDescription }
    }
    func refreshContext() {
        guard connected || !scope.isEmpty else { return }
        do {
            let store = try ContextStore.shared()
            let snapshot = try store.snapshot(scope: scope)
            automaticContext = Dictionary(uniqueKeysWithValues: Set(snapshot.routes.values).map { ($0, ContextPrompt.candidates(in: snapshot, agentID: $0)) })
            contextItems = snapshot.items; contextEnabled = snapshot.enabled; contextRoutes = snapshot.routes; contextError = nil
        } catch { contextError = error.localizedDescription }
    }
    func enableContext(_ enabled: Bool) {
        do { try ContextStore.shared().setEnabled(enabled, scope: scope); refreshContext() }
        catch { contextError = error.localizedDescription }
    }
    func routeContext(source: String, agentID: String?) {
        do { try ContextStore.shared().route(source: source, agentID: agentID, scope: scope); refreshContext() }
        catch { contextError = error.localizedDescription }
    }
    func captureContext(_ input: CaptureInput) throws {
        try ContextStore.shared().capture([input], scope: scope)
        refreshContext()
    }
    func removeContext(_ ids: Set<String>) {
        do {
            try ContextStore.shared().remove(ids, scope: scope)
            for agent in Array(selectedContext.keys) { selectedContext[agent]?.removeAll { ids.contains($0) } }
            for agent in Array(excludedContext.keys) { excludedContext[agent]?.removeAll { ids.contains($0) } }
            persist(); refreshContext()
        } catch { contextError = error.localizedDescription }
    }
    func selectContext(_ id: String, agentID: String, selected: Bool) {
        var ids = selectedContext[agentID] ?? []
        ids.removeAll { $0 == id }
        if selected { ids.append(id) }
        var excluded = excludedContext[agentID] ?? []
        excluded.removeAll { $0 == id }
        if !selected { excluded.append(id) }
        excludedContext[agentID] = excluded
        selectedContext[agentID] = ids; persist()
    }
    func contextForAgent(_ id: String) -> [CapturedContext] {
        let selected = contextItems.filter { (selectedContext[id] ?? []).contains($0.id) }
        // Pending submissions reserve their captures, including when delivery is
        // uncertain. Only a receipt or durable event marks them used on disk.
        let reserved = Set(pending.filter { $0.agentID == id }.flatMap { $0.contextIDs ?? [] })
        let excluded = Set(excludedContext[id] ?? [])
        let automatic = (automaticContext[id] ?? []).filter { item in !reserved.contains(item.id) && !excluded.contains(item.id) && !selected.contains(where: { $0.id == item.id }) }
        return selected + automatic
    }
    private func recordContextDelivery(_ message: PendingMessage) throws {
        guard let ids = message.contextIDs, !ids.isEmpty else { return }
        try ContextStore.shared().markUsed(ids, agentID: message.agentID, turnID: message.id, scope: scope)
        refreshContext()
    }
}


private enum KeychainAccount {
    private static let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: "xyz.paradigm.centaur", kSecAttrAccount as String: "managed"]
    static func read() throws -> AccountCredential? {
        var q = query; q[kSecReturnData as String] = true; q[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(q as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess else { throw KeychainError(status: status, action: "read") }
        guard let data = result as? Data,
              let credential = try? JSONDecoder().decode(AccountCredential.self, from: data) else {
            throw KeychainError(status: errSecDecode, action: "read")
        }
        return credential
    }
    static func save(_ credential: AccountCredential) throws {
        let data = try JSONEncoder().encode(credential)
        let status = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecSuccess { return }
        guard status == errSecItemNotFound else { throw KeychainError(status: status) }
        var q = query; q[kSecValueData as String] = data; q[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let result = SecItemAdd(q as CFDictionary, nil)
        guard result == errSecSuccess else { throw KeychainError(status: result) }
    }
    static func remove() throws {
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw KeychainError(status: status, action: "removed") }
    }
    private struct KeychainError: LocalizedError {
        let status: OSStatus
        var action = "saved"
        var errorDescription: String? {
            if status == errSecInteractionNotAllowed || status == errSecNotAvailable {
                return "Unlock this device to access your saved sign-in, then retry."
            }
            return "The saved sign-in could not be \(action) securely (\(status)). Try again."
        }
    }
}
