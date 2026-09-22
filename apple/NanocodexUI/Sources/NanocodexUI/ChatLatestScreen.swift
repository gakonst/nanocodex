import SwiftUI

/// One conversation-level entry for the most recent attributed computer snapshot.
/// Keep this view's identity stable so an open sheet follows incoming snapshots.
public struct ChatLatestScreen: View {
    public let output: ChatGeneratedOutput
    private let onWatchLive: (() -> Void)?
    @State private var expanded = false
    @State private var thumbnail: CGImage?
    @State private var failed = false

    public init(output: ChatGeneratedOutput, onWatchLive: (() -> Void)? = nil) {
        self.output = output; self.onWatchLive = onWatchLive
    }

    public var body: some View {
        Button { if let onWatchLive { onWatchLive() } else { expanded = true } } label: {
            HStack(spacing: 12) {
                screenImage
                    .frame(width: 64, height: 44)
                    .clipped()
                    .background(.black.opacity(0.08), in: RoundedRectangle(cornerRadius: 6))
                VStack(alignment: .leading, spacing: 2) {
                    Text(onWatchLive == nil ? "Latest screen" : "Watch live").font(.subheadline.weight(.semibold))
                    Text(onWatchLive == nil ? "Computer snapshot" : "Last captured frame · tap to connect").font(.caption).foregroundStyle(.secondary)
                }
                Spacer(minLength: 8)
                Image(systemName: "arrow.up.left.and.arrow.down.right").foregroundStyle(.secondary)
            }
            .padding(10)
            .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 12))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(onWatchLive == nil ? "Open latest computer screen" : "Watch live computer screen")
        .contextMenu { Button("View last snapshot") { expanded = true } }
        .accessibilityIdentifier("latest-computer-screen")
        .sheet(isPresented: $expanded) {
            NavigationStack {
                GeometryReader { geometry in
                    ScrollView([.horizontal, .vertical]) {
                        screenImage
                            .frame(width: geometry.size.width, height: geometry.size.height)
                    }
                }
                .background(.black.opacity(0.04))
                .navigationTitle("Latest screen")
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Done") { expanded = false }
                    }
                    ToolbarItem(placement: .automatic) {
                        ChatMediaPreview(title: "Computer snapshot", load: { [try await GeneratedAsset.previewURL(output)] }) {
                            Label("Open full resolution", systemImage: "arrow.up.left.and.arrow.down.right")
                        }
                    }
                }
                .safeAreaInset(edge: .bottom) {
                    Text("Latest captured snapshot · updates as the computer is used")
                        .font(.caption).foregroundStyle(.secondary)
                        .padding(10).frame(maxWidth: .infinity).background(.regularMaterial)
                }
            }
            .accessibilityIdentifier("latest-computer-screen-viewer")
        }
        .task(id: output.id) {
            thumbnail = nil; failed = false
            do {
                let decoded = try await GeneratedAsset.thumbnail(output)
                guard !Task.isCancelled else { return }
                thumbnail = decoded; failed = decoded == nil
            } catch { if !Task.isCancelled { failed = true } }
        }
    }

    @ViewBuilder private var screenImage: some View {
        if let thumbnail {
            Image(decorative: thumbnail, scale: 1).resizable().aspectRatio(contentMode: .fit)
                .accessibilityLabel("Latest captured computer screen")
        } else if failed {
            Label("Screen unavailable", systemImage: "display.trianglebadge.exclamationmark")
                .font(.caption).foregroundStyle(.secondary)
        } else {
            ProgressView().accessibilityLabel("Loading latest screen")
        }
    }
}
