import ActivityKit
import AppIntents
import InboxCore
import SwiftUI
import WidgetKit

struct MeetingLockedControl: ControlWidget {
    static let kind = "NanocodexMeetingLockedControl"
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: Self.kind) {
            ControlWidgetButton(action: StartMeetingLockedIntent()) {
                Label("Meeting", systemImage: "waveform")
            }
        }
        .displayName("Listen to a meeting")
        .description("Start recording from Control Center or the Lock Screen without opening the app.")
    }
}

struct MeetingLockedActivityWidget: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: MeetingLockedActivityAttributes.self) { context in
            HStack(spacing: 12) {
                Image(systemName: context.state.phase == "sent" ? "checkmark.circle.fill" : "waveform")
                VStack(alignment: .leading, spacing: 3) {
                    Text(label(context)).font(.headline)
                    if context.state.phase == "listening" {
                        Text(Duration.seconds(context.state.seconds).formatted()).font(.caption).monospacedDigit()
                    }
                }
                Spacer()
                actions(context)
            }
            .padding()
            .activityBackgroundTint(.black)
            .activitySystemActionForegroundColor(.white)
            .foregroundStyle(.white)
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) { Image(systemName: "waveform") }
                DynamicIslandExpandedRegion(.center) { Text(label(context)) }
                DynamicIslandExpandedRegion(.bottom) { actions(context) }
            } compactLeading: { Image(systemName: "waveform") }
              compactTrailing: { Text(context.state.phase == "ready" ? "Ready" : "Meeting") }
              minimal: { Image(systemName: "waveform") }
        }
    }

    @ViewBuilder private func actions(_ context: ActivityViewContext<MeetingLockedActivityAttributes>) -> some View {
        if !context.isStale {
            if context.state.phase == "listening" {
                Button(intent: FinishMeetingLockedIntent(captureID: context.attributes.captureID)) {
                    ZStack {
                        Circle().fill(.white)
                        Circle().stroke(.red, lineWidth: 2)
                        RoundedRectangle(cornerRadius: 3).fill(.red).frame(width: 19, height: 19)
                    }.frame(width: 52, height: 52)
                }.buttonStyle(.plain)
                    .accessibilityLabel("Stop Recording")
                    .accessibilityHint("Stops the meeting recording and starts an agent with its transcript")
            } else if context.state.phase == "ready" {
                Button("Retry starting agent", intent: SendMeetingLockedIntent(captureID: context.attributes.captureID))
                    .buttonStyle(.bordered)
            }
        }
    }
    private func label(_ context: ActivityViewContext<MeetingLockedActivityAttributes>) -> String {
        if context.isStale { return "Meeting status unavailable" }
        switch context.state.phase {
        case "preparing": return "Preparing microphone…"
        case "listening": return "Meeting recording"
        case "transcribing": return "Finishing transcript…"
        case "ready": return context.state.warning ? "Not sent · transcript saved" : "Transcript ready to send"
        case "sending": return "Sending transcript…"
        case "sent": return "Meeting sent"
        case "failed": return "Meeting stopped"
        default: return "Meeting ended"
        }
    }
}

// Interactive Lock Screen accessory for devices that don't pin a Control Widget.
// Its timeline contains no transcript or account identity.
private struct MeetingStartEntry: TimelineEntry { let date: Date }
private struct MeetingStartProvider: TimelineProvider {
    func placeholder(in context: Context) -> MeetingStartEntry { .init(date: .now) }
    func getSnapshot(in context: Context, completion: @escaping (MeetingStartEntry) -> Void) {
        completion(.init(date: .now))
    }
    func getTimeline(in context: Context, completion: @escaping (Timeline<MeetingStartEntry>) -> Void) {
        completion(Timeline(entries: [.init(date: .now)], policy: .never))
    }
}

struct MeetingStartWidget: Widget {
    let kind = "NanocodexMeetingStart"
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: kind, provider: MeetingStartProvider()) { _ in
            MeetingStartWidgetView()
                .containerBackground(for: .widget) { Color.clear }
        }
        .configurationDisplayName("Listen to a meeting")
        .description("Start meeting recording from the Lock Screen without opening the app. Grant permission in the app first.")
        .supportedFamilies([.accessoryCircular, .accessoryRectangular])
    }
}

private struct MeetingStartWidgetView: View {
    @Environment(\.widgetFamily) private var family
    var body: some View {
        Button(intent: StartMeetingLockedIntent()) {
            if family == .accessoryRectangular {
                HStack(spacing: 8) {
                    Image(systemName: "waveform").font(.title2)
                    VStack(alignment: .leading) {
                        Text("Meeting listening").font(.headline)
                        Text("Tap to start").font(.caption)
                    }
                }
            } else {
                ZStack {
                    AccessoryWidgetBackground()
                    Image(systemName: "waveform").font(.title2)
                }
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Listen to a meeting")
        .accessibilityHint("Starts recording without opening the app")
    }
}
