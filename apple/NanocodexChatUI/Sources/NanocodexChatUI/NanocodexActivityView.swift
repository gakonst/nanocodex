import SwiftUI
import InboxCore
import NanocodexUI

/// Row frames in the host’s `conversation-viewport` coordinate space.
public struct NanocodexConversationRowFrames: PreferenceKey {
    public static var defaultValue: [String: CGRect] { [:] }
    public static func reduce(value: inout [String: CGRect], nextValue: () -> [String: CGRect]) {
        value.merge(nextValue()) { _, new in new }
    }
}

public struct NanocodexActivityView: View {
    let item: ConversationItem
    var onExpansion: (Bool) -> Void = { _ in }
    var onInteraction: () -> Void = {}
    public init(item: ConversationItem, onExpansion: @escaping (Bool) -> Void = { _ in },
                onInteraction: @escaping () -> Void = {}) {
        self.item = item; self.onExpansion = onExpansion; self.onInteraction = onInteraction
    }
    @State private var expanded = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var visibleStep: String?
    private var failures: Int { item.activity.filter { $0.tool?.status == "Failed" }.count }
    private var headline: String {
        guard item.isRunning else { return "Activity" }
        let current = item.activity.last(where: \.running) ?? item.activity.last
        return current?.tool?.title ?? (current?.role == "Thinking" ? "Thinking" : "Working")
    }
    private var summary: String {
        let calls = item.activity.filter { $0.tool != nil }.count
        let thoughts = item.activity.filter { $0.role == "Thinking" }.count
        var parts: [String] = []
        if thoughts > 0 { parts.append("Reasoning") }
        if calls > 0 { parts.append("\(calls) tool call\(calls == 1 ? "" : "s")") }
        if parts.isEmpty { parts.append(item.activity.isEmpty ? "Getting started" : "\(item.activity.count) steps") }
        return parts.joined(separator: " · ")
    }
    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                let opening = !expanded
                if opening && visibleStep == nil { visibleStep = item.activity.first?.id }
                // Register the reading anchor before expansion publishes geometry.
                onExpansion(opening)
                withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.18)) { expanded = opening }
            } label: {
                HStack(spacing: 10) {
                    Group {
                        if item.isRunning { ProgressView().controlSize(.small) }
                        else { Image(systemName: failures > 0 ? "exclamationmark.circle" : "checkmark.circle") }
                    }.frame(width: 20).foregroundStyle(failures > 0 ? Color.orange : NanocodexConversationPalette.muted)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(headline).font(.subheadline.weight(.medium)).foregroundStyle(NanocodexConversationPalette.text)
                        Text(summary).font(.caption).foregroundStyle(NanocodexConversationPalette.muted)
                    }.frame(maxWidth: .infinity, alignment: .leading)
                    if failures > 0 {
                        Text("\(failures) failed").font(.caption.weight(.medium)).foregroundStyle(.orange)
                    }
                    if !item.activity.isEmpty {
                        Image(systemName: expanded ? "chevron.up" : "chevron.down")
                            .font(.caption.weight(.semibold)).foregroundStyle(NanocodexConversationPalette.muted)
                    }
                }.frame(minHeight: 44).contentShape(Rectangle())
            }.buttonStyle(.plain).disabled(item.activity.isEmpty)
                .accessibilityIdentifier("activity-disclosure")
                .accessibilityLabel("Activity")
                .accessibilityValue("\(expanded ? "Expanded" : "Collapsed"), \(headline), \(summary), \(failures) failed")
                .accessibilityHint(item.activity.isEmpty ? "Waiting for activity" : "Show or hide thinking and tool calls")
            if expanded && !item.activity.isEmpty {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(item.activity) { row in
                            NanocodexActivityStep(row: row, live: item.isRunning && row.running)
                                .id(row.id)
                                .background(GeometryReader { geometry in
                                    Color.clear.preference(key: NanocodexConversationRowFrames.self,
                                        value: [row.id: geometry.frame(in: .named("conversation-viewport"))])
                                })
                        }
                    }.scrollTargetLayout().padding(.top, 8)
                }.scrollPosition(id: $visibleStep, anchor: .top)
                    .frame(maxHeight: 300).fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("activity-timeline")
                    .background(GeometryReader { geometry in
                        Color.clear.preference(key: NanocodexConversationRowFrames.self,
                            value: [item.id + ":timeline": geometry.frame(in: .named("conversation-viewport"))])
                    })
                    .onScrollPhaseChange { _, phase in
                        if phase == .tracking || phase == .interacting { onInteraction() }
                    }
            }
        }.padding(.horizontal, 12).padding(.vertical, 6)
            .background(NanocodexConversationPalette.surface, in: RoundedRectangle(cornerRadius: 16))
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .contain).accessibilityIdentifier("activity-group")
    }
}

public struct NanocodexActivityStep: View {
    let row: TranscriptRow
    let live: Bool
    public init(row: TranscriptRow, live: Bool) { self.row = row; self.live = live }
    @State private var expanded = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    private var failed: Bool { row.tool?.status == "Failed" }
    private var title: String { row.tool?.title ?? (row.role == "Thinking" ? "Thinking" : "Progress update") }
    private var subject: String {
        if let subject = row.tool?.subject { return subject }
        // A bounded plain preview avoids parsing a growing Markdown document on every token.
        let firstLine = row.text.prefix(180).split(whereSeparator: \.isNewline).first ?? ""
        return firstLine.trimmingCharacters(in: CharacterSet(charactersIn: "#*` _"))
    }
    private var status: String {
        if failed { return "Failed" }
        if live { return "Running" }
        if row.tool?.status == "Stopped" { return "Stopped" }
        // A past turn can retain an unfinished tool. Don't claim it completed.
        if row.running || row.tool?.status == "Running" { return "Interrupted" }
        return "Completed"
    }
    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button { withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.18)) { expanded.toggle() } } label: {
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: failed ? "exclamationmark.circle.fill" : row.role == "Tool" ? "terminal" : "text.alignleft")
                        .font(.subheadline).foregroundStyle(failed ? Color.orange : NanocodexConversationPalette.muted)
                        .frame(width: 20).padding(.top, 3)
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(alignment: .firstTextBaseline) {
                            Text(title).font(.subheadline.weight(.medium)).foregroundStyle(NanocodexConversationPalette.text)
                            Spacer(minLength: 4)
                            if live { ProgressView().controlSize(.mini) }
                            Text(status).font(.caption2).foregroundStyle(failed ? Color.orange : NanocodexConversationPalette.muted)
                        }
                        if !subject.isEmpty { Text(subject).font(.caption).foregroundStyle(NanocodexConversationPalette.muted).lineLimit(2).multilineTextAlignment(.leading) }
                    }
                    Image(systemName: expanded ? "chevron.up" : "chevron.down")
                        .font(.caption2.weight(.semibold)).foregroundStyle(NanocodexConversationPalette.muted).padding(.top, 4)
                }.padding(.vertical, 10).frame(minHeight: 44).contentShape(Rectangle())
            }.buttonStyle(.plain).accessibilityIdentifier("activity-step-" + row.id)
                .accessibilityValue(expanded ? "Expanded" : "Collapsed")
            if expanded {
                ScrollView {
                    Group {
                        if row.tool != nil { NanocodexToolActivityView(row: row) }
                        else if row.role == "Thinking" { ChatMarkdown(text: row.text) }
                        else { Text(row.text).font(.body).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading) }
                    }.padding(12)
                }.frame(maxHeight: 240).fixedSize(horizontal: false, vertical: true)
                    .background(NanocodexConversationPalette.background, in: RoundedRectangle(cornerRadius: 12))
                    .accessibilityIdentifier("activity-detail-" + row.id)
                    .padding(.bottom, 10)
            }
            Divider().opacity(0.4)
        }.accessibilityElement(children: .contain)
    }
}

public struct NanocodexToolActivityView: View {
    let row: TranscriptRow
    public init(row: TranscriptRow) { self.row = row }
    private var tool: ToolPresentation {
        if let tool = row.tool { return tool }
        var fallback = ToolPresentation(name: row.text, arguments: .null)
        if !row.running { fallback.finish(.string(row.detail)) }
        return fallback
    }
    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if !tool.input.isEmpty { fields(tool.input, heading: "Input") }
            if !tool.output.isEmpty { fields(tool.output, heading: "Result") }
        }.foregroundStyle(NanocodexConversationPalette.muted).accessibilityIdentifier("tool-activity")
    }
    private func fields(_ values: [ToolField], heading: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(heading).font(.system(size: 12, weight: .semibold)).foregroundStyle(NanocodexConversationPalette.muted)
            ForEach(Array(values.enumerated()), id: \.offset) { _, field in
                VStack(alignment: .leading, spacing: 3) {
                    if field.label != heading { Text(field.label).font(.system(size: 12)).foregroundStyle(NanocodexConversationPalette.muted) }
                    Text(field.value).font(.system(size: 14, design: field.code ? .monospaced : .default))
                        .foregroundStyle(NanocodexConversationPalette.text).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                }
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
}

