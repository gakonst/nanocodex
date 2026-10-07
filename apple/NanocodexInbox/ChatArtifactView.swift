import SwiftUI
import NanocodexApps
import NanocodexUI

/// Renders an assistant response, replacing completed ```swift-artifact fences
/// with live native views from the swift-v1 interpreter. Prose stays Markdown.
struct ChatResponseBody: View {
    let text: String
    let running: Bool

    var body: some View {
        let segments = ChatArtifactSegment.split(text)
        if segments.count == 1, case .markdown(let only) = segments[0] {
            ChatMarkdown(text: only, compact: true)
        } else {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(Array(segments.enumerated()), id: \.offset) { _, segment in
                    switch segment {
                    case .markdown(let prose):
                        ChatMarkdown(text: prose, compact: true)
                    case .artifact(let source, let complete):
                        if complete { ChatArtifactCard(source: source) }
                        else { ChatArtifactPlaceholder(lines: source.split(separator: "\n").count, interrupted: !running) }
                    }
                }
            }
        }
    }
}

/// Sessions survive cell reuse and repeated SwiftUI updates for the same
/// source, so a recycled transcript row does not re-evaluate or reset state.
@MainActor
private final class ChatArtifactSessions {
    static let shared = ChatArtifactSessions()
    private var sessions: [String: NativeAppSession] = [:]
    private var order: [String] = []

    func session(for source: String) throws -> NativeAppSession {
        if let existing = sessions[source] { return existing }
        let host = NativeAppHost(
            loadState: { [:] },
            saveState: { _ in },
            runAgent: { _ in throw AppDiagnostic("Inline artifacts cannot run agent requests. Save it as an app to use Agent.run.") },
            agentRequestsHaveExternalEffects: false)
        let session = try NativeAppSession(source: source, host: host, limits: AppLimits(agentCalls: 0))
        sessions[source] = session
        order.append(source)
        if order.count > 48 { sessions.removeValue(forKey: order.removeFirst())?.invalidate() }
        return session
    }
}

struct ChatArtifactCard: View {
    let source: String
    @State private var session: NativeAppSession?
    @State private var failure: String?
    @State private var showsSource = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: "sparkles.rectangle.stack").font(.caption)
                Text("Artifact").font(.caption.weight(.semibold))
                Spacer()
                Button(showsSource ? "Hide source" : "Source") { showsSource.toggle() }
                    .font(.caption).buttonStyle(.borderless)
                ChatCopyButton(text: source, label: "Copy artifact source")
            }
            .foregroundStyle(.secondary)
            .padding(.leading, 14).padding(.trailing, 6).padding(.vertical, 4)
            Divider().opacity(0.35)
            Group {
                if let failure {
                    Label(failure, systemImage: "exclamationmark.triangle")
                        .font(.footnote).foregroundStyle(.secondary).textSelection(.enabled)
                        .padding(14)
                } else if let session {
                    ChatArtifactSurface(session: session)
                } else {
                    ProgressView().frame(maxWidth: .infinity, minHeight: 80)
                }
            }
            if showsSource {
                Divider().opacity(0.35)
                ScrollView(.horizontal) {
                    Text(source).font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled).fixedSize(horizontal: true, vertical: true).padding(14)
                }
            }
        }
        .background(ChatPalette.userBubble.opacity(0.55), in: RoundedRectangle(cornerRadius: 16))
        .overlay(RoundedRectangle(cornerRadius: 16).strokeBorder(Color.primary.opacity(0.07)))
        .clipShape(RoundedRectangle(cornerRadius: 16))
        .accessibilityIdentifier("chat-artifact")
        .task(id: source) { await load() }
    }

    private func load() async {
        do {
            let next = try ChatArtifactSessions.shared.session(for: source)
            session = next
            failure = nil
            try await next.start()
        } catch {
            if session?.nodes.isEmpty ?? true {
                failure = (error as? AppDiagnostic)?.errorDescription ?? error.localizedDescription
            }
        }
    }
}

/// The interpreter's root fills its container; inside a self-sizing transcript
/// row it must report its intrinsic height instead of expanding without bound.
private struct ChatArtifactSurface: View {
    @ObservedObject var session: NativeAppSession
    var body: some View {
        NativeAppView(session: session, background: .clear)
            .fixedSize(horizontal: false, vertical: true)
            .padding(14)
            .scrollDisabled(true)
    }
}

struct ChatArtifactPlaceholder: View {
    let lines: Int
    let interrupted: Bool
    var body: some View {
        HStack(spacing: 10) {
            if interrupted { Image(systemName: "exclamationmark.triangle") } else { ProgressView() }
            Text(interrupted ? "Artifact was cut off before it finished." : "Building artifact… \(lines) lines")
                .font(.footnote).monospacedDigit()
        }
        .foregroundStyle(.secondary)
        .frame(maxWidth: .infinity, minHeight: 64, alignment: .leading)
        .padding(.horizontal, 14)
        .background(ChatPalette.userBubble.opacity(0.4), in: RoundedRectangle(cornerRadius: 16))
        .accessibilityIdentifier("chat-artifact-pending")
    }
}
