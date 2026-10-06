import SwiftUI
#if os(iOS)
import UIKit
#elseif os(macOS)
import AppKit
#endif

/// A passive viewer owned by one conversation. Changing threads destroys this
/// instance and closes its transport; expanding preserves the same viewer.
public struct RemoteThreadScreen: View {
    private let service: RemoteService
    @Binding private var selection: RemoteScreenSelection?
    @Binding private var expanded: Bool
    private let onClose: () -> Void
    private let onControls: (RemoteScreenSelection?) -> Void
    @StateObject private var viewer = RemoteViewer()
    @Environment(\.scenePhase) private var scenePhase
    @State private var hands: [RemoteHand] = []
    @State private var loaded = false
    @State private var visible = true
    @State private var connectionTask: Task<Void, Never>?
    @State private var error: String?

    public init(service: RemoteService, selection: Binding<RemoteScreenSelection?>,
                expanded: Binding<Bool>, onClose: @escaping () -> Void,
                onControls: @escaping (RemoteScreenSelection?) -> Void) {
        self.service = service; _selection = selection; _expanded = expanded
        self.onClose = onClose; self.onControls = onControls
    }

    public var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "display")
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Screens").font(.subheadline.weight(.semibold))
                        .accessibilityAddTraits(.isHeader)
                    Text(selectedName ?? "Choose a device")
                        .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                Spacer(minLength: 0)
                Menu {
                    Button("Change desktop") { viewer.close(); selection = nil }
                        .disabled(selection == nil)
                    Button("Screen controls") { viewer.suspend(); onControls(selection) }
                    Button("Refresh") { Task { await refresh(reconnect: true) } }
                } label: { Image(systemName: "ellipsis").frame(width: 44, height: 44) }
                    .accessibilityLabel("Screen options").accessibilityIdentifier("thread-screen-options")
                Button { expanded.toggle() } label: {
                    Image(systemName: expanded ? "arrow.down.right.and.arrow.up.left" : "arrow.up.left.and.arrow.down.right")
                        .frame(width: 44, height: 44)
                }.accessibilityLabel(expanded ? "Collapse screen" : "Expand screen")
                    .accessibilityIdentifier("thread-screen-expand")
                Button(action: onClose) { Image(systemName: "xmark").frame(width: 44, height: 44) }
                    .accessibilityLabel("Hide screen").accessibilityIdentifier("thread-screen-close")
            }
            .padding(.leading, 14).padding(.trailing, 4)
            .fixedSize(horizontal: false, vertical: true)
            Divider()
            Group {
                if selection == nil {
                    if !loaded { ProgressView("Finding desktops…") }
                    else if hands.isEmpty {
                        VStack(spacing: 6) {
                            Text(error == nil ? "No desktops available" : "Couldn’t load desktops").font(.subheadline)
                            Button("Refresh") { Task { await refresh(reconnect: true) } }
                        }
                    } else {
                        ScrollView {
                            LazyVStack(spacing: 0) {
                                ForEach(hands, id: \.identity) { hand in
                                    screenRow(hand)
                                }
                            }
                        }
                        .frame(minHeight: 0, maxHeight: .infinity)
                        .clipped()
                        .contentShape(Rectangle())
                        .accessibilityIdentifier("thread-screen-devices")
                    }
                } else if viewer.hand != nil {
                    RemoteCanvas(viewer: viewer).accessibilityIdentifier("thread-screen-canvas")
                        .overlay {
                            if viewer.connecting {
                                ProgressView(viewer.status).padding(12).background(.regularMaterial, in: RoundedRectangle(cornerRadius: 10))
                            } else if !viewer.connected {
                                Button("Reconnect") { Task {
                                    guard visible, scenePhase == .active else { return }
                                    await viewer.reconnect()
                                } }
                                    .padding(12).background(.regularMaterial, in: RoundedRectangle(cornerRadius: 10))
                            }
                        }
                } else {
                    VStack(spacing: 6) {
                        if !loaded { ProgressView("Finding desktop…") }
                        else {
                            Text(error == nil ? "This desktop is offline" : "Couldn’t load this desktop").font(.subheadline)
                            Text("Your selection is saved for this thread.").font(.caption).foregroundStyle(.secondary)
                            Button("Refresh") { Task { await refresh(reconnect: true) } }
                        }
                    }
                }
            }
            .frame(minWidth: 0, maxWidth: .infinity, minHeight: 0, maxHeight: .infinity)
            .clipped()
            if selection != nil {
                Divider()
                HStack {
                    Text(viewer.hand == nil ? (loaded ? "Offline" : "Finding desktop…") : viewer.status)
                    Spacer()
                    if viewer.hand != nil { RemotePerformanceView(viewer: viewer) }
                    Text("View only")
                }
                .font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                .padding(.horizontal, 12).padding(.vertical, 5)
                .fixedSize(horizontal: false, vertical: true)
            }
        }
        .buttonStyle(.plain)
        .background(panelBackground)
        .clipShape(RoundedRectangle(cornerRadius: 14))
        .overlay {
            RoundedRectangle(cornerRadius: 14)
                .strokeBorder(.primary.opacity(0.12), lineWidth: 0.5)
                .allowsHitTesting(false)
        }
        .contentShape(RoundedRectangle(cornerRadius: 14))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("thread-screen-panel")
        .task(id: scenePhase) {
            guard scenePhase == .active else { return }
            await viewer.resume()
            while !Task.isCancelled {
                await refresh()
                do { try await Task.sleep(for: .seconds(5)) } catch { return }
            }
        }
        .onChange(of: scenePhase) { _, phase in if phase != .active { viewer.suspend() } }
        .onAppear { visible = true }
        .onDisappear { visible = false; connectionTask?.cancel(); viewer.close() }
    }

    private var panelBackground: Color {
#if os(iOS)
        Color(uiColor: .secondarySystemBackground)
#else
        Color(nsColor: .windowBackgroundColor)
#endif
    }

    private var selectedName: String? {
        guard let selection else { return nil }
        if let hand = hands.first(where: selection.matches) {
            return hand.screenDisplayName
        }
        return selection.screenDisplayName
    }

    private func screenRow(_ hand: RemoteHand) -> some View {
        Button {
            selection = RemoteScreenSelection(hand: hand)
            connectionTask?.cancel()
            connectionTask = Task {
                guard visible, scenePhase == .active, !Task.isCancelled else { return }
                await viewer.connect(service: service, hand: hand)
            }
        } label: {
            HStack(spacing: 10) {
                Image(systemName: hand.kind == .phone ? "iphone" : "display")
                    .font(.body).foregroundStyle(.secondary)
                    .frame(width: 24)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(hand.screenMachineName)
                        .font(.subheadline.weight(.medium)).foregroundStyle(.primary)
                        .lineLimit(1)
                    Text(hand.screenSurfaceName)
                        .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                Image(systemName: "chevron.right")
                    .font(.caption.weight(.semibold)).foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
            }
            .padding(.horizontal, 14).padding(.vertical, 9)
            .frame(minHeight: 52)
            .contentShape(Rectangle())
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(hand.screenDisplayName)
        .accessibilityHint("Watch this screen in the conversation")
        .accessibilityIdentifier("thread-screen:" + hand.machineID + ":" + hand.id)
    }

    private func refresh(reconnect: Bool = false) async {
        do {
            let values = try await service.list()
            guard visible, scenePhase == .active, !Task.isCancelled else { return }
            hands = values; loaded = true; error = nil
            if let selection, viewer.hand == nil, let hand = values.first(where: selection.matches) {
                await viewer.connect(service: service, hand: hand)
            } else if reconnect {
                await viewer.refreshConnection()
            }
        } catch {
            if !Task.isCancelled { loaded = true; self.error = error.localizedDescription }
        }
    }
}
