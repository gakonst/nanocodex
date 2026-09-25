import AVFoundation
import InboxCore
import SwiftUI

struct MeetingView: View {
    @ObservedObject var model: InboxModel
    @StateObject private var recorder = MeetingRecorder()
    @StateObject private var summary = MeetingSummaryPreview()
    @AppStorage("quickVoice.locale") private var locale = "en-US"
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var account: UUID?
    @State private var accountScope: String?
    @State private var targetID: String?
    @State private var submitted = false
    @State private var stopRequested = false
    @State private var sendError: String?
    @State private var visible = true

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Picker("Speech language", selection: $locale) {
                        Text("English").tag("en-US")
                        Text("Ελληνικά").tag("el-GR")
                    }
                    .pickerStyle(.segmented)
                    .disabled(recorder.working)
                    Text(recorder.status)
                        .font(.subheadline)
                        .accessibilityIdentifier("meeting-status")
                    if recorder.recording {
                        Label("Recording · \(Duration.seconds(recorder.seconds).formatted())", systemImage: "mic.fill")
                            .foregroundStyle(.red)
                            .accessibilityIdentifier("meeting-recording-indicator")
                    }
                    if let sendError { Text(sendError).font(.caption).foregroundStyle(.red) }
                    HStack {
                        Text("Live transcript").font(.headline)
                        Spacer()
                        if recorder.recording { Text("May revise").font(.caption).foregroundStyle(.secondary) }
                    }
                    if scenePhase == .active {
                        if recorder.reviewing {
                            TextEditor(text: Binding(get: { recorder.transcript }, set: { recorder.edit($0) }))
                                .frame(minHeight: 180, maxHeight: 240)
                                .accessibilityIdentifier("meeting-transcript")
                                .privacySensitive()
                        } else {
                            ScrollView {
                                Text(recorder.transcript.isEmpty ? "Words appear here as they are recognized…" : recorder.transcript)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .accessibilityIdentifier("meeting-transcript")
                                    .privacySensitive()
                            }
                            .frame(minHeight: 180, maxHeight: 240)
                        }
                    } else {
                        Text("Meeting content hidden while inactive")
                            .frame(maxWidth: .infinity, minHeight: 180, alignment: .topLeading)
                    }
                    summaryPanel

                    if recorder.recording {
                        Button("Stop Recording") { stopRequested = true; recorder.finish() }
                            .buttonStyle(.borderedProminent)
                            .accessibilityIdentifier("meeting-finish")
                    } else if !recorder.working {
                        if recorder.reviewing, QuickVoiceInput.finalText(recorder.transcript) != nil {
                            Button(sendError == nil ? "Start agent with transcript" : "Retry starting agent") { submit() }
                                .disabled(!model.connected || model.isDemo || account != model.quickVoiceGeneration || scenePhase != .active)
                                .accessibilityIdentifier("meeting-send")
                        } else {
                            Button("Start listening") { Task { await start() } }
                                .buttonStyle(.borderedProminent)
                                .accessibilityIdentifier("meeting-start")
                        }
                    } else if !recorder.recording {
                        ProgressView("Finishing segments…")
                    }
                    Text("Recording continues through pauses. Stop Recording transcribes and starts one agent thread. An interrupted transcript stays available for review; nothing partial sends automatically.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                .padding()
            }
            .navigationTitle("Meeting listening")
        }
        .interactiveDismissDisabled(recorder.working)
        .onChange(of: recorder.summarySnapshot) { _, snapshot in
            guard scenePhase == .active, model.connected,
                  let account, account == model.quickVoiceGeneration, let snapshot else { return }
            summary.receive(snapshot, account: account)
        }
        .onChange(of: recorder.finalizedSegments) { _, _ in enqueuePreview() }
        .onChange(of: recorder.settledSegmentIndices) { _, _ in enqueuePreview() }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active, model.connected, let account,
               account == model.quickVoiceGeneration, (recorder.working || recorder.reviewing) {
                summary.begin(account: account, captureID: recorder.captureID,
                              snapshot: recorder.summarySnapshot, model: model, accountScope: accountScope)
                enqueuePreview()
            } else if phase != .active { summary.pause() }
        }
        .onChange(of: recorder.reviewing) { _, ready in
            if ready && stopRequested {
                stopRequested = false
                if !recorder.completedWithWarning { submit() }
            }
        }
        .onChange(of: model.connected) { _, connected in
            if !connected { summary.pause(); stopRequested = false; recorder.interrupt("Account disconnected. Review your partial transcript; sign in before sending.") }
            else if let account, account == model.quickVoiceGeneration, scenePhase == .active {
                summary.begin(account: account, captureID: recorder.captureID,
                              snapshot: recorder.summarySnapshot, model: model, accountScope: accountScope)
                enqueuePreview()
            }
        }
        .onChange(of: model.quickVoiceGeneration) { _, generation in
            if let account, generation != account {
                summary.clear()
                accountScope = nil
                stopRequested = false
                recorder.interrupt("Account changed. This transcript cannot be sent from a different account.")
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: AVAudioSession.interruptionNotification)) { notification in
            guard let type = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
                  type == AVAudioSession.InterruptionType.began.rawValue else { return }
            stopRequested = false; recorder.interrupt()
        }
        .onReceive(NotificationCenter.default.publisher(for: AVAudioSession.routeChangeNotification)) { notification in
            if let reason = notification.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
               reason == AVAudioSession.RouteChangeReason.oldDeviceUnavailable.rawValue {
                stopRequested = false; recorder.interrupt()
            }
        }
        .onDisappear {
            visible = false
            summary.close()
            summary.clear()
            if !submitted, let text = QuickVoiceInput.finalText(recorder.transcript),
               let scope = try? model.lockedVoiceAccountScope() {
                model.retainLockedVoiceRecovery(text, captureID: UUID().uuidString, accountScope: scope)
            }
            recorder.discard()
        }
    }

    private func start() async {
        guard visible, scenePhase == .active, !submitted, !recorder.working else { return }
        guard model.connected, !model.isDemo, !model.restoringAccount else { return }
        summary.clear()
        guard QuickVoiceRecorder.audioOwner == nil else {
            // Recorder reports the competing capture as well; do not stop its mic.
            await recorder.start(locale: locale)
            return
        }
        LockedVoiceCoordinator.shared.yieldToForegroundRecording()
        model.voice.stop()
        account = model.quickVoiceGeneration
        accountScope = try? model.lockedVoiceAccountScope()
        targetID = nil
        sendError = nil
        stopRequested = false
        await recorder.start(locale: locale == "el-GR" ? "el-GR" : "en-US")
        if visible, scenePhase == .active, model.connected,
           account == model.quickVoiceGeneration, recorder.working, let account {
            summary.begin(account: account, captureID: recorder.captureID,
                          snapshot: recorder.summarySnapshot, model: model, accountScope: accountScope)
            enqueuePreview()
        }
    }

    private func enqueuePreview() {
        guard scenePhase == .active, model.connected, let account,
              account == model.quickVoiceGeneration else { return }
        summary.enqueue(finalized: recorder.finalizedSegments, settled: recorder.settledSegmentIndices)
    }

    @ViewBuilder private var summaryPanel: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Rolling summary").font(.headline)
            if scenePhase == .active {
                Text(summary.text.isEmpty ? "A live recap will appear as speech segments are confirmed." : summary.text)
                    .font(.subheadline)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .privacySensitive()
                Text(summary.caption).font(.caption).foregroundStyle(.secondary)
            } else {
                Text("Meeting content hidden while inactive").font(.caption)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
        .accessibilityIdentifier("meeting-rolling-summary")
    }

    private func submit() {
        guard !submitted, visible, scenePhase == .active, recorder.reviewing, !recorder.working,
              let text = QuickVoiceInput.finalText(recorder.transcript), let account else { return }
        guard model.connected, !model.isDemo, account == model.quickVoiceGeneration else { return }
        guard model.sendQuickVoice(text, generation: account, targetID: &targetID) else {
            sendError = model.error ?? "Could not start the conversation. Your transcript is still here; try sending again."
            return
        }
        submitted = true
        summary.close()
        dismiss()
    }
}

/// The draft recap is separate from the one final agent turn. Only finalized
/// recognition segments are sent, in capture order and under stable revisions.
@MainActor
final class MeetingSummaryPreview: ObservableObject {
    @Published private(set) var text = ""
    @Published private(set) var caption = "Recent confirmed speech · no generated summary"

    private var account: UUID?
    private var accountScope: String?
    private var captureID: UUID?
    private weak var model: InboxModel?
    private var latestRevision = 0
    private var latestSummaryRevision = 0
    private var confirmedText = ""
    private var generated = false
    private var active = false
    private var epoch = UUID()
    private var finalized: [Int: String] = [:]
    private var settled = Set<Int>()
    private var nextIndex = 0
    private var pieceOffset = 0
    private var serverRevision = 0
    private var task: Task<Void, Never>?

    func begin(account: UUID, captureID: UUID, snapshot: MeetingSummarySnapshot?,
               model: InboxModel, accountScope: String?) {
        if self.account != account || self.captureID != captureID {
            clear()
            self.account = account
            self.captureID = captureID
        }
        self.model = model
        self.accountScope = accountScope
        active = true
        if let snapshot { receive(snapshot, account: account) }
        drain()
    }

    func receive(_ snapshot: MeetingSummarySnapshot, account: UUID) {
        guard self.account == account, captureID == snapshot.captureID,
              snapshot.revision >= latestRevision else { return }
        latestRevision = snapshot.revision
        guard snapshot.confirmedText != confirmedText else { return }
        confirmedText = snapshot.confirmedText
        let points = snapshot.confirmedText.split(separator: "\n")
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }.suffix(4)
            .map { "• " + String($0.prefix(180)) }
        if generated {
            caption = "Generated summary · catching up with recent speech"
        } else {
            text = points.joined(separator: "\n")
            caption = snapshot.warning ? "Partial speech · review before sending" :
                "Recent confirmed speech · not a generated summary"
        }
    }

    func enqueue(finalized: [MeetingFinalizedSegment], settled: [Int]) {
        self.finalized = Dictionary(uniqueKeysWithValues: finalized.map { ($0.index, $0.text) })
        self.settled = Set(settled)
        drain()
    }

    private func drain() {
        guard active, task == nil, let captureID, let account, let accountScope,
              let model, model.quickVoiceGeneration == account else { return }
        while settled.contains(nextIndex), finalized[nextIndex]?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty != false { nextIndex += 1 }
        guard let delta = finalized[nextIndex], !delta.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        let chunks = Self.boundedChunks(delta)
        guard pieceOffset < chunks.count else { return }
        let index = nextIndex
        let token = epoch
        task = Task { [weak self] in
            guard let self else { return }
            for chunk in chunks.dropFirst(self.pieceOffset) {
                guard !Task.isCancelled, self.epoch == token, self.active, self.account == account,
                      self.captureID == captureID, model.quickVoiceGeneration == account else { return }
                let revision = self.serverRevision + 1
                do {
                    let result = try await model.updateMeetingPreview(captureID: captureID,
                        revision: revision, delta: chunk, accountScope: accountScope)
                    guard !Task.isCancelled, self.epoch == token, self.active, self.account == account,
                          self.captureID == captureID else { return }
                    self.serverRevision = revision
                    self.pieceOffset += 1
                    self.apply(summary: result.summary, summaryRevision: result.summaryRevision,
                               account: account, captureID: captureID)
                } catch {
                    if self.active, self.account == account, self.captureID == captureID {
                        self.caption = "Live recap unavailable · transcript still recording"
                    }
                    break // A later segment retries the same revision and text.
                }
            }
            if !Task.isCancelled, self.epoch == token,
               self.account == account, self.captureID == captureID {
                if self.pieceOffset == chunks.count {
                    self.nextIndex = index + 1
                    self.pieceOffset = 0
                }
                self.task = nil
                if self.nextIndex > index { self.drain() }
            }
        }
    }

    private static func boundedChunks(_ text: String) -> [String] {
        var chunks: [String] = [], chunk = "", size = 0
        for character in text {
            let bytes = String(character).utf8.count
            if size + bytes > 4096, !chunk.isEmpty {
                chunks.append(chunk); chunk = ""; size = 0
            }
            chunk.append(character); size += bytes
        }
        if !chunk.isEmpty { chunks.append(chunk) }
        return chunks
    }

    private func apply(summary: String, summaryRevision: Int, account: UUID, captureID: UUID) {
        guard self.account == account, self.captureID == captureID,
              summaryRevision > latestSummaryRevision,
              !summary.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        latestSummaryRevision = summaryRevision
        text = String(summary.prefix(2_000))
        caption = "Generated preview · check against the transcript"
        generated = true
    }

    func pause() {
        active = false
        epoch = UUID()
        task?.cancel(); task = nil
    }

    func close() {
        guard let captureID, let accountScope, let model else { return }
        pause()
        Task { await model.closeMeetingPreview(captureID: captureID, accountScope: accountScope) }
    }

    func clear() {
        pause()
        account = nil; accountScope = nil; captureID = nil; model = nil
        latestRevision = 0; latestSummaryRevision = 0
        confirmedText = ""; generated = false
        finalized = [:]; settled = []; nextIndex = 0; pieceOffset = 0; serverRevision = 0
        text = ""
        caption = "Recent confirmed speech · no generated summary"
    }
}
