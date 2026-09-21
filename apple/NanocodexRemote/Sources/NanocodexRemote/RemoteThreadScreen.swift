import SwiftUI

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
                Image(systemName: "display").foregroundStyle(.secondary)
                Text(selection?.name ?? "Screen").font(.caption.weight(.medium)).lineLimit(1)
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
            }.padding(.horizontal, 12)
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
                                    Button {
                                        selection = RemoteScreenSelection(hand: hand)
                                        connectionTask?.cancel()
                                        connectionTask = Task {
                                            guard visible, scenePhase == .active, !Task.isCancelled else { return }
                                            await viewer.connect(service: service, hand: hand)
                                        }
                                    } label: {
                                        HStack {
                                            VStack(alignment: .leading, spacing: 2) {
                                                Text(hand.machineName).font(.subheadline)
                                                Text(hand.name).font(.caption).foregroundStyle(.secondary)
                                            }
                                            Spacer()
                                            Image(systemName: "chevron.right").font(.caption).foregroundStyle(.secondary)
                                        }.padding(.horizontal, 16).padding(.vertical, 10).contentShape(Rectangle())
                                    }.accessibilityIdentifier("thread-screen:" + hand.machineID + ":" + hand.id)
                                }
                            }
                        }
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
            }.frame(maxWidth: .infinity, maxHeight: .infinity)
            if selection != nil {
                HStack {
                    Text(viewer.hand == nil ? (loaded ? "Offline" : "Finding desktop…") : viewer.status)
                    Spacer()
                    Text("View only")
                }.font(.caption2).foregroundStyle(.secondary).padding(.horizontal, 12).padding(.vertical, 5)
            }
        }
        .buttonStyle(.plain)
        .background(Color.secondary.opacity(0.1), in: RoundedRectangle(cornerRadius: 14))
        .clipShape(RoundedRectangle(cornerRadius: 14))
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
