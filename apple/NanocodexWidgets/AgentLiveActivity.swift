import ActivityKit
import AppIntents
import InboxCore
import SwiftUI
import WidgetKit

@main
struct NanocodexWidgets: WidgetBundle {
    var body: some Widget {
        AgentLiveActivity()
        VoiceTaskWidget()
        LockedVoiceActivity()
        LockedVoiceControl()
        MeetingStartWidget()
        MeetingLockedControl()
        MeetingLockedActivityWidget()
    }
}

struct AgentLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: AgentActivityAttributes.self) { context in
            AgentActivityView(state: context.state, account: context.attributes.account, stale: context.isStale)
                .activityBackgroundTint(.black)
                .activitySystemActionForegroundColor(.white)
                .widgetURL(AgentActivityLink.url(account: context.attributes.account, agentID: context.state.entries.first?.id))
        } dynamicIsland: { context in
            let state = context.state
            let stale = context.isStale || state.paused
            return DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Label("Nanocodex", systemImage: "square.stack.3d.up.fill").font(.caption.bold())
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Text(stale && state.needsAttention == 0 && state.ready == 0 ? "\(state.running) last running" : state.headline)
                        .font(.caption).foregroundStyle(stale ? .secondary : .primary)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    VStack(alignment: .leading, spacing: 6) {
                        AgentActivityRows(state: state, account: context.attributes.account, stale: stale)
                        if stale { Text("Updates paused · open to refresh").font(.caption2).foregroundStyle(.secondary) }
                    }
                }
            } compactLeading: {
                Image(systemName: state.needsAttention > 0 ? "exclamationmark.circle.fill" : stale ? "pause.circle" : state.ready > 0 ? "checkmark.circle.fill" : "square.stack.3d.up.fill")
                    .foregroundStyle(state.needsAttention > 0 ? .orange : stale ? .gray : .green)
            } compactTrailing: {
                Text(state.needsAttention > 0 ? "\(state.needsAttention)" : state.ready > 0 ? "\(state.ready)" : stale ? "—" : "\(state.running)").monospacedDigit()
                    .accessibilityLabel(state.headline + (stale ? ", updates paused" : ""))
            } minimal: {
                Image(systemName: state.needsAttention > 0 ? "exclamationmark.circle.fill" : stale ? "pause.circle" : "square.stack.3d.up.fill")
                    .accessibilityLabel(state.headline + (stale ? ", updates paused" : ""))
            }
            .widgetURL(AgentActivityLink.url(account: context.attributes.account, agentID: state.entries.first?.id))
        }
    }
}

struct AgentActivityView: View {
    let state: AgentActivitySnapshot
    let account: String
    let stale: Bool
    @Environment(\.dynamicTypeSize) private var typeSize
    private var paused: Bool { stale || state.paused }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label("Nanocodex", systemImage: "square.stack.3d.up.fill")
                    .font(.subheadline.bold())
                Spacer(minLength: 8)
                Text(paused && state.needsAttention == 0 && state.ready == 0 ? "\(state.running) last running" : state.headline)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(state.needsAttention > 0 ? Color.orange : paused && state.ready == 0 ? .gray : .green)
            }
            .lineLimit(1).minimumScaleFactor(0.8)
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 12) {
                    Text("\(state.running) \(paused ? "last running" : "running")")
                    Text("\(state.ready) to review")
                    if (state.deliveryFailures ?? 0) > 0 { Text("\(state.deliveryFailures ?? 0) unconfirmed").foregroundStyle(.orange) }
                    else if (state.queued ?? 0) > 0 { Text("\(state.queued ?? 0) queued") }
                }
                Text("\(state.ready) ready · \(state.failed) failed · \(state.running) \(paused ? "last running" : "running")")
            }
            .font(.caption).foregroundStyle(.secondary).monospacedDigit()
            AgentActivityRows(state: state, account: account, limit: typeSize > .large ? 1 : 2, stale: paused)
            if paused || typeSize <= .large { HStack(spacing: 3) {
                if paused {
                    Text("Updates paused ·")
                    Text(state.observedAt, style: .time)
                    Spacer(minLength: 4)
                    Text("Open to refresh").foregroundStyle(.white.opacity(0.8))
                } else {
                    Text(state.total > state.entries.count ? "+\(state.total - state.entries.count) more · " : "")
                    Text("Tap to open conversation")
                    Spacer(minLength: 0)
                }
            }
            .font(.caption2).foregroundStyle(.gray).lineLimit(1) }
        }
        .padding(12).foregroundStyle(.white)
        .dynamicTypeSize(...DynamicTypeSize.xxxLarge)
    }
}

private struct AgentActivityRows: View {
    let state: AgentActivitySnapshot
    let account: String
    var limit = 2
    var stale = false
    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            ForEach(Array(state.entries.prefix(limit).enumerated()), id: \.element.id) { index, entry in
                Link(destination: AgentActivityLink.url(account: account, agentID: entry.id)) {
                    HStack(spacing: 8) {
                        Image(systemName: ["failed", "delivery"].contains(entry.status) ? "exclamationmark.circle.fill"
                              : entry.status == "ready" ? "checkmark.circle.fill" : "circle.dotted")
                            .foregroundStyle(["failed", "delivery"].contains(entry.status) ? Color.orange : entry.status == "ready" ? .green : .gray)
                            .font(.caption)
                        VStack(alignment: .leading, spacing: 1) {
                            HStack(spacing: 6) {
                                Text(entry.title).font(.caption.weight(.semibold)).lineLimit(1).privacySensitive()
                                Spacer(minLength: 0)
                                if (entry.queued ?? 0) > 0 {
                                    Text("+\(entry.queued ?? 0) queued").font(.caption2).foregroundStyle(.secondary).fixedSize()
                                }
                            }
                            Text(entry.detail).font(.caption2).foregroundStyle(.white.opacity(0.75))
                                .lineLimit(index == 0 ? 2 : 1).privacySensitive()
                        }
                        Spacer(minLength: 0)
                        Image(systemName: "chevron.right").font(.caption2).foregroundStyle(.gray)
                    }
                    .contentShape(Rectangle())
                    .accessibilityHint((stale && entry.status == "running" ? "Last observed. " : "") + (entry.action ?? "Open conversation"))
                }
                .foregroundStyle(.white)
            }
        }
    }
}

// Launches microphone capture in the foreground app after the system unlocks it.
// No account information or dictated text is stored in the widget timeline.
private struct VoiceTaskEntry: TimelineEntry {
    let date: Date
}

private struct VoiceTaskProvider: TimelineProvider {
    func placeholder(in context: Context) -> VoiceTaskEntry { VoiceTaskEntry(date: .now) }
    func getSnapshot(in context: Context, completion: @escaping (VoiceTaskEntry) -> Void) {
        completion(VoiceTaskEntry(date: .now))
    }
    func getTimeline(in context: Context, completion: @escaping (Timeline<VoiceTaskEntry>) -> Void) {
        completion(Timeline(entries: [VoiceTaskEntry(date: .now)], policy: .never))
    }
}

struct VoiceTaskWidget: Widget {
    let kind = "NanocodexVoiceTask"
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: kind, provider: VoiceTaskProvider()) { _ in
            VoiceTaskWidgetView()
                .containerBackground(for: .widget) { Color.clear }
        }
        .configurationDisplayName("Speak to Nanocodex")
        .description("A rectangular widget offers Speak and Meeting buttons. A circular widget starts Speak. Grant microphone and speech permission in the app first.")
        .supportedFamilies([.accessoryCircular, .accessoryRectangular])
    }
}

private struct VoiceTaskWidgetView: View {
    @Environment(\.widgetFamily) private var family
    var body: some View {
        if family == .accessoryRectangular {
            // The already-pinned Speak widget gains meeting capture without
            // requiring the user to discover and add a second Lock Screen widget.
            // Keep two distinct hit targets; a tap on Speak must still record now.
            VStack(alignment: .leading, spacing: 1) {
                Button(intent: StartLockedVoiceIntent()) {
                    Label("Speak", systemImage: "mic.fill")
                        .frame(maxWidth: .infinity, minHeight: 27, alignment: .leading)
                }
                .accessibilityLabel("Speak to Nanocodex")
                .accessibilityHint("Record one voice task")
                Button(intent: StartMeetingLockedIntent()) {
                    Label("Meeting", systemImage: "waveform")
                        .frame(maxWidth: .infinity, minHeight: 27, alignment: .leading)
                }
                .accessibilityLabel("Listen to a meeting")
                .accessibilityHint("Record and recap until you tap Stop Recording")
            }
            .font(.caption.weight(.semibold))
            .lineLimit(1)
            .buttonStyle(.plain)
        } else {
            Button(intent: StartLockedVoiceIntent()) {
                ZStack {
                    AccessoryWidgetBackground()
                    Image(systemName: "mic.fill").font(.title2)
                }
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Speak to Nanocodex")
            .accessibilityHint("Starts recording without opening the app")
        }
    }
}

struct LockedVoiceControl: ControlWidget {
    static let kind = "NanocodexLockedVoiceControl"
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: Self.kind) {
            ControlWidgetButton(action: StartLockedVoiceIntent()) {
                Label("Voice task", systemImage: "mic.fill")
            }
        }
        .displayName("Record a voice task")
        .description("Record a new Nanocodex task in English or Greek.")
    }
}

/// Voice Memos-inspired recorder controls, adapted to ActivityKit's compact
/// Lock Screen card. The red waveform is actual quantized microphone level.
struct LockedVoiceActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: LockedVoiceActivityAttributes.self) { context in
            VStack(alignment: .leading, spacing: 9) {
                HStack {
                    Image(systemName: "waveform").foregroundStyle(.red)
                    Text("Nanocodex").font(.subheadline.weight(.semibold))
                    Spacer()
                    if context.state.phase == "listening" {
                        Text("RECORDING").font(.caption2.weight(.bold))
                            .tracking(1).foregroundStyle(.red)
                    }
                }
                if context.state.phase == "listening" {
                    HStack(alignment: .center, spacing: 14) {
                        VStack(alignment: .leading, spacing: 7) {
                            if let startedAt = context.state.startedAt {
                                Text(startedAt, style: .timer)
                                    .font(.system(size: 29, weight: .medium, design: .rounded))
                                    .monospacedDigit()
                                    .accessibilityLabel("Recording duration")
                            }
                            RecordingWaveform(levels: context.state.waveform ?? [])
                        }
                        stopButton(captureID: context.attributes.captureID)
                    }
                    Text("Stop recording to start your agent")
                        .font(.caption2).foregroundStyle(.secondary)
                } else {
                    HStack(spacing: 9) {
                        Image(systemName: symbol(displayPhase(context)))
                            .foregroundStyle(statusTint(displayPhase(context)))
                        Text(headline(displayPhase(context), failure: context.state.failure))
                            .font(.headline)
                        Spacer()
                        if ["recordingFailed", "transcriptionFailed"].contains(context.state.phase) {
                            Button("Record again", intent: StartLockedVoiceIntent())
                                .buttonStyle(.plain).foregroundStyle(.red)
                        }
                    }.frame(minHeight: 48)
                }
            }
            .padding(.horizontal, 17).padding(.vertical, 13)
            .activityBackgroundTint(.black)
            .activitySystemActionForegroundColor(.white)
            .foregroundStyle(.white)
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Image(systemName: symbol(displayPhase(context)))
                        .foregroundStyle(statusTint(displayPhase(context)))
                }
                DynamicIslandExpandedRegion(.center) {
                    if context.state.phase == "listening", let startedAt = context.state.startedAt {
                        Text(startedAt, style: .timer).font(.title2).monospacedDigit()
                    } else {
                        Text(headline(displayPhase(context), failure: context.state.failure))
                    }
                }
                DynamicIslandExpandedRegion(.bottom) {
                    if context.state.phase == "listening" {
                        HStack(spacing: 14) {
                            RecordingWaveform(levels: context.state.waveform ?? [])
                            stopButton(captureID: context.attributes.captureID)
                        }.padding(.vertical, 5)
                    }
                }
            } compactLeading: {
                Image(systemName: symbol(displayPhase(context)))
                    .foregroundStyle(statusTint(displayPhase(context)))
            } compactTrailing: {
                if context.state.phase == "listening", let startedAt = context.state.startedAt {
                    Text(startedAt, style: .timer).monospacedDigit().font(.caption2)
                } else {
                    Text(displayPhase(context) == "sent" ? "Sent" : "Voice").font(.caption2)
                }
            } minimal: {
                Image(systemName: "waveform").foregroundStyle(.red)
            }
        }
    }

    private func stopButton(captureID: String) -> some View {
        Button(intent: FinishLockedVoiceIntent(captureID: captureID)) {
            ZStack {
                Circle().fill(.white)
                Circle().stroke(.red, lineWidth: 2)
                RoundedRectangle(cornerRadius: 3).fill(.red).frame(width: 19, height: 19)
            }.frame(width: 52, height: 52)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Stop Recording")
        .accessibilityHint("Ends microphone capture and starts an agent with the transcript")
    }

    private func displayPhase(_ context: ActivityViewContext<LockedVoiceActivityAttributes>) -> String {
        let phase = context.state.phase
        let active = ["preparing", "listening", "transcribing", "sending"].contains(phase)
        return context.isStale && active ? "expired" : phase
    }

    private func headline(_ phase: String, failure: String?) -> String {
        switch phase {
        case "preparing": "Getting ready…"
        case "listening": "Recording"
        case "transcribing": "Transcribing…"
        case "sending": "Starting agent…"
        case "sent": "Agent started"
        case "cancelled": "Recording stopped"
        case "recordingFailed": failure ?? "Recording stopped"
        case "transcriptionFailed": "Transcription unfinished"
        case "deliveryFailed": "Delivery unconfirmed"
        case "expired": "Recording interrupted"
        default: "Voice task ended"
        }
    }

    private func symbol(_ phase: String) -> String {
        switch phase {
        case "sent": "checkmark.circle.fill"
        case "preparing", "listening": "waveform"
        case "transcribing": "text.bubble"
        case "sending": "arrow.up.circle.fill"
        default: "exclamationmark.circle"
        }
    }

    private func statusTint(_ phase: String) -> Color {
        switch phase {
        case "sent": .green
        case "preparing", "listening": .red
        case "transcribing", "sending": .white
        default: .red
        }
    }
}

private struct RecordingWaveform: View {
    let levels: [UInt8]
    var body: some View {
        HStack(alignment: .center, spacing: 2) {
            ForEach(0..<28, id: \.self) { index in
                let offset = levels.count - 28 + index
                let level = offset >= 0 && offset < levels.count ? Int(levels[offset]) : 0
                Capsule()
                    .fill(.red.opacity(level == 0 ? 0.35 : 0.95))
                    .frame(maxWidth: .infinity)
                    .frame(height: CGFloat(4 + level * 2))
            }
        }
        .frame(height: 35)
        .accessibilityLabel("Live recording waveform")
    }
}
