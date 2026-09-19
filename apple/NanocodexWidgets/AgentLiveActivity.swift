import ActivityKit
import InboxCore
import SwiftUI
import WidgetKit

@main
struct NanocodexWidgets: WidgetBundle {
    var body: some Widget {
        AgentLiveActivity()
        VoiceTaskWidget()
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
                .widgetURL(URL(string: "nanocodex://voice/new")!)
                .containerBackground(for: .widget) { Color.clear }
        }
        .configurationDisplayName("Speak to Nanocodex")
        .description("Speak in Greek or English to start a new agent thread.")
        .supportedFamilies([.accessoryCircular, .accessoryRectangular, .accessoryInline])
    }
}

private struct VoiceTaskWidgetView: View {
    @Environment(\.widgetFamily) private var family
    var body: some View {
        Group {
            switch family {
            case .accessoryInline:
                Label("Speak to Nanocodex", systemImage: "mic.fill")
            case .accessoryRectangular:
                HStack(spacing: 8) {
                    Image(systemName: "mic.fill").font(.title2)
                    VStack(alignment: .leading) {
                        Text("Speak to Nanocodex").font(.headline)
                        Text("New agent thread").font(.caption)
                    }
                }
            default:
                ZStack {
                    AccessoryWidgetBackground()
                    Image(systemName: "mic.fill").font(.title2)
                }
            }
        }
        .accessibilityLabel("Speak to Nanocodex")
        .accessibilityHint("Opens voice capture to start a new agent thread in Greek or English")
    }
}
