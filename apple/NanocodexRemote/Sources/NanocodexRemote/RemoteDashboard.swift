import SwiftUI
#if os(macOS)
import AppKit

enum RemoteHostIdentity {
    static func load(defaults: UserDefaults = .standard) -> String {
        let key = "nanocodex.remote.machine-id"
        if let existing = defaults.string(forKey: key), !existing.isEmpty { return existing }
        let identity = UUID().uuidString
        defaults.set(identity, forKey: key)
        return identity
    }
}
#endif

public struct RemoteDashboard: View {
    private let service: RemoteService
    @StateObject private var viewer = RemoteViewer()
    @Environment(\.scenePhase) private var scenePhase
    @State private var hands: [RemoteHand] = []
    @State private var discoveryLoaded = false
    @State private var error: String?
    @State private var discoveryError: String?
    @State private var text = ""
#if os(macOS)
    @StateObject private var host: RemoteMacHost
    @StateObject private var phoneHost: RemoteMacHost
    private let ownsHosts: Bool
    @State private var displays: [RemoteSurface] = []
    @State private var displayID = ""
    @State private var starting = false
    @State private var phones: [PairedPhone] = []
    @AppStorage("nanocodex.remote.phone-id") private var phoneID = ""
    @AppStorage("nanocodex.remote.phone-runner") private var phoneRunner = ""
    // AppStorage's default value is not persisted until written. A generated
    // default changed the machine identity on every new dashboard/relaunch.
    private let machineID = RemoteHostIdentity.load()
#endif
    public init(service: RemoteService) {
        self.service = service
#if os(macOS)
        _host = StateObject(wrappedValue: RemoteMacHost()); _phoneHost = StateObject(wrappedValue: RemoteMacHost()); ownsHosts = true
#endif
    }
#if os(macOS)
    public init(service: RemoteService, host: RemoteMacHost, phoneHost: RemoteMacHost) {
        self.service = service; _host = StateObject(wrappedValue: host); _phoneHost = StateObject(wrappedValue: phoneHost); ownsHosts = false
    }
#endif
    public var body: some View {
        VStack(spacing: 12) {
            if let hand = viewer.hand {
                HStack {
                    Button { viewer.close() } label: { Label("Screens", systemImage: "chevron.left") }
                    Text(hand.machineName + " · " + hand.name).lineLimit(1)
                    Spacer()
                    if viewer.controlling { Button("Release control") { viewer.releaseControl() }.buttonStyle(.borderedProminent) }
                    else { Button("Take control") { viewer.takeControl() }.disabled(!viewer.connected || !hand.controllable) }
                }
                RemoteCanvas(viewer: viewer).accessibilityIdentifier("remote-canvas")
                    .overlay {
                        if viewer.connecting { ProgressView(viewer.status).padding().background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12)) }
                    }
                HStack {
                    Text(viewer.status).font(.caption).foregroundStyle(.secondary)
                        .accessibilityValue(viewer.diagnosticPresentation)
                        .accessibilityIdentifier("remote-status")
                    if !viewer.connected && !viewer.connecting {
                        Button("Reconnect") { Task { await viewer.reconnect() } }
                    }
                    Spacer()
#if os(macOS)
                    Text("⌘⇧Esc releases control").font(.caption).foregroundStyle(.secondary)
#else
                    Text("Tap to click · drag to move · two fingers to scroll").font(.caption2).foregroundStyle(.secondary)
#endif
                }
                if viewer.controlling {
                    VStack(spacing: 8) {
                        HStack {
                            TextField("Type on remote screen", text: $text).textFieldStyle(.roundedBorder).onSubmit(sendText)
                                .autocorrectionDisabled()
#if os(iOS)
                                .textInputAutocapitalization(.never)
#endif
                            Button("Send", action: sendText).disabled(text.isEmpty || text.utf8.count > 4096)
                        }
                        HStack {
                            Button("Return") { key(40) }
                            Button("Tab") { key(43) }
                            Button("Esc") { key(41) }
                            Button("⌫") { key(42) }
                            if viewer.hand?.kind == .phone { Button("Home") { key(74) } }
                            Spacer()
                        }
                    }
                }
            } else {
                HStack { Text("Screens").font(.title2.bold()); Spacer(); Button("Refresh") { Task { await refresh() } } }
                if !discoveryLoaded {
                    ProgressView("Loading remote screens…")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if hands.isEmpty {
                    ContentUnavailableView("No screens available", systemImage: "display", description: Text("VM desktops appear here when ready. To view a Mac, start screen sharing on that Mac."))
                } else {
                    List(hands, id: \.identity) { hand in
                        Button { Task { await viewer.connect(service: service, hand: hand) } } label: {
                            HStack {
                                Image(systemName: hand.kind == .phone ? "iphone" : "display")
                                VStack(alignment: .leading) { Text(hand.machineName); Text(hand.name).font(.caption).foregroundStyle(.secondary) }
                                Spacer(); Text(hand.controllable ? "View and control" : "View only").font(.caption)
                            }.padding(.vertical, 4).contentShape(Rectangle())
                        }.buttonStyle(.plain)
                            .accessibilityIdentifier("remote-screen:\(hand.machineID):\(hand.id)")
                    }
                }
                if viewer.status != "Disconnected" { Text(viewer.status).font(.callout).foregroundStyle(.secondary) }
            }
            if let message = error ?? discoveryError { Text(message).font(.callout).foregroundStyle(.red).textSelection(.enabled) }
#if os(macOS)
            Divider()
            HStack {
                if host.sharing || host.reconnecting {
                    Label(host.reconnecting ? "Reconnecting screen sharing…" : "Sharing this Mac · \(host.viewerCount) viewing", systemImage: "record.circle").foregroundStyle(.red)
                    Spacer()
                    if host.reconnecting {
                        ProgressView().controlSize(.small)
                    } else if host.surface?.controllable == true {
                        Button("Revoke control") { host.revokeControl() }
                    } else {
                        Button("Enable control") {
                            guard let surfaceID = host.surface?.id else { return }
                            guard MacScreen.requestInputPermission() else {
                                error = "Enable Nanocodex in System Settings → Privacy & Security → Accessibility, then choose Enable control again."
                                return
                            }
                            Task {
                                error = nil
                                await host.start(service: service, machineID: machineID, name: Host.current().localizedName ?? "Mac", surfaceID: surfaceID)
                                await refresh()
                            }
                        }
                    }
                    Button("Stop sharing") { Task { await host.stopSharing(); await refresh() } }
                } else {
                    if displays.isEmpty {
                        Button("Choose a screen to share…") { Task { await chooseScreen() } }
                    } else {
                        Picker("Screen", selection: $displayID) { ForEach(displays) { Text($0.name).tag($0.id) } }.frame(maxWidth: 260)
                        Button("Share this screen") {
                            starting = true
                            Task {
                                _ = MacScreen.requestInputPermission()
                                await host.start(service: service, machineID: machineID, name: Host.current().localizedName ?? "Mac", surfaceID: displayID)
                                starting = false; await refresh()
                            }
                        }.disabled(starting || displayID.isEmpty)
                    }
                    Spacer(); Text(host.status).font(.caption).foregroundStyle(.secondary)
                }
            }
            HStack {
                if phoneHost.sharing {
                    Label("Sharing paired iPhone · \(phoneHost.viewerCount) viewing", systemImage: "iphone").foregroundStyle(.red)
                    Spacer()
                    Button("Revoke control") { phoneHost.revokeControl() }
                    Button("Stop sharing iPhone") { Task { await phoneHost.stop(); await refresh() } }
                } else {
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            Button("Find paired iPhones") { Task { await findPhones() } }.disabled(starting)
                            if !phones.isEmpty {
                                Picker("iPhone", selection: $phoneID) { ForEach(phones) { Text($0.name).tag($0.id) } }.frame(maxWidth: 260)
                            }
                            Button(phoneRunner.isEmpty ? "Choose signed runner…" : "Change runner…") { choosePhoneRunner() }.disabled(starting)
                            Button("Share iPhone") {
                                starting = true
                                Task {
                                    let configuration = PhoneBridgeConfiguration(deviceID: phoneID, runner: URL(fileURLWithPath: phoneRunner))
                                    await phoneHost.startPhone(service: service, machineID: machineID + "-phone-" + phoneID,
                                        name: phones.first(where: { $0.id == phoneID })?.name ?? "Paired iPhone", bridge: configuration)
                                    starting = false; await refresh()
                                }
                            }.disabled(starting || phoneRunner.isEmpty || !phones.contains(where: { $0.id == phoneID }))
                        }
                        Text("Requires a trusted, paired iPhone with Developer Mode and a WebDriverAgent runner signed in Xcode.").font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer(); Text(phoneHost.status).font(.caption).foregroundStyle(.secondary)
                }
            }
#endif
        }
        .padding()
        .task(id: scenePhase) {
            guard scenePhase == .active else { return }
            while !Task.isCancelled {
                await refresh()
                do { try await Task.sleep(for: .seconds(5)) } catch { return }
            }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { Task { await viewer.resume() } }
            if phase != .active { viewer.releaseControl() }
#if os(iOS)
            if phase == .background { viewer.suspend() }
#endif
        }
        .onChange(of: viewer.controlling) { _, controlling in if !controlling { text = "" } }
        .onDisappear {
            viewer.close()
#if os(macOS)
            if ownsHosts { Task { await host.stop(); await phoneHost.stop() } }
#endif
        }
    }
    private func refresh() async {
        do { let values = try await service.list(); guard !Task.isCancelled else { return }; hands = values; discoveryError = nil; discoveryLoaded = true }
        catch { if !Task.isCancelled { discoveryError = error.localizedDescription; discoveryLoaded = true } }
    }
    private func key(_ code: UInt16) { for down in [true, false] { viewer.input(kind: .key, down: down, key: code) } }
    private func sendText() { guard !text.isEmpty, text.utf8.count <= 4096 else { return }; viewer.input(kind: .text, text: text); text = "" }
#if os(macOS)
    private func chooseScreen() async {
        guard MacScreen.requestScreenPermission() else { error = RemoteError.screenPermission.localizedDescription; return }
        do { displays = try await MacScreen.surfaces(); displayID = displays.first?.id ?? ""; error = nil }
        catch { self.error = error.localizedDescription }
    }
    private func findPhones() async {
        starting = true; defer { starting = false }
        do {
            phones = try await PhoneBridge.devices()
            if !phones.contains(where: { $0.id == phoneID }) { phoneID = phones.first?.id ?? "" }
            error = phones.isEmpty ? "No trusted paired iPhone found. Connect the iPhone and trust this Mac." : nil
        } catch { self.error = error.localizedDescription }
    }
    private func choosePhoneRunner() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = false; panel.allowsMultipleSelection = false
        panel.title = "Choose your signed WebDriverAgent runner"
        panel.message = "Select the .xctestrun produced by building the WebDriverAgentRunner scheme. Only choose a build you trust."
        guard panel.runModal() == .OK, let url = panel.url, url.pathExtension == "xctestrun" else { return }
        phoneRunner = url.path
    }
#endif
}

#if os(macOS)
/// Keeps sharing visible after the screen picker closes.
public struct RemoteSharingIndicator: View {
    @ObservedObject private var host: RemoteMacHost
    @ObservedObject private var phoneHost: RemoteMacHost
    public init(host: RemoteMacHost, phoneHost: RemoteMacHost) { self.host = host; self.phoneHost = phoneHost }
    public var body: some View {
        if host.sharing || host.reconnecting || phoneHost.sharing {
            HStack(spacing: 8) {
                Label(host.reconnecting ? "Screen sharing reconnecting…" : "Screen sharing active", systemImage: "record.circle").foregroundStyle(.red)
                Button("Stop sharing") { Task { await host.stopSharing(); await phoneHost.stop() } }
                    .accessibilityLabel("Stop sharing")
                    .accessibilityIdentifier("remote-stop-sharing")
                    .help("Stop sharing this Mac and any paired iPhone")
            }
        }
    }
}
#endif
