import AppKit
import SwiftUI

struct HandsView: View {
    @EnvironmentObject private var model: AppModel
    @State private var enablingMac = false
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Hands").font(.system(size: 29, weight: .semibold)).tracking(-0.6)
                        Text("Give your agents a place to work.").font(.system(size: 14)).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Menu {
                        Button("This Mac…") { model.editingHand = nil; model.showingHandSetup = true }
                        Button("Virtual Machine…") { model.editingHand = Hand(id: "vm-\(UUID().uuidString.prefix(8).lowercased())", name: "Private VM", kind: "vm", workspace: model.state.defaults["workspace"].string); model.showingHandSetup = true }
                        Button("Another Computer…") { model.showingRemoteSetup = true }
                    } label: { Label("Add Hand", systemImage: "plus") }.menuStyle(.borderedButton).disabled(!model.state.connected)
                }
                HStack(spacing: 14) {
                    Image(systemName: "brain").font(.system(size: 22)).frame(width: 42, height: 42).background(Color.primary.opacity(0.045), in: RoundedRectangle(cornerRadius: 11))
                    VStack(alignment: .leading, spacing: 5) { Text("Managed agents").font(.system(size: 14, weight: .medium)); Text("Durable threads, models, and connected tools. Always available.").font(.system(size: 12)).foregroundStyle(.secondary) }
                    Spacer()
                    HStack(spacing: 5) { Circle().fill(model.state.connected ? .green : .secondary).frame(width: 5, height: 5); Text(model.state.connected ? "Connected" : "Sign in") }.font(.system(size: 11)).foregroundStyle(.secondary).fixedSize()
                }.padding(18).background(Color.primary.opacity(0.025), in: RoundedRectangle(cornerRadius: 13))
                if !model.state.hands.contains(where: { $0.kind == "local" }) {
                    VStack(alignment: .leading, spacing: 15) {
                        Label("Use this Mac", systemImage: "laptopcomputer").font(.system(size: 18, weight: .medium))
                        Text("Run commands and work with files in your Nanocodex folder. It’s created automatically, and you can stop sharing at any time.").font(.system(size: 13)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                        Text("Commands run with your macOS user’s permissions.").font(.system(size: 12)).foregroundStyle(.secondary)
                        HStack {
                            Button {
                                enablingMac = true
                                Task { await model.useThisMac(); enablingMac = false }
                            } label: { if enablingMac { ProgressView().controlSize(.small) } else { Text("Enable this Mac") } }.buttonStyle(.borderedProminent).tint(.primary).disabled(enablingMac || !model.state.connected).accessibilityIdentifier("enable-this-mac")
                            Button("Choose a different folder…") { model.editingHand = nil; model.showingHandSetup = true }.buttonStyle(.link).disabled(!model.state.connected)
                        }
                    }.padding(22).frame(maxWidth: .infinity, alignment: .leading).overlay(RoundedRectangle(cornerRadius: 13).stroke(Color.primary.opacity(0.12)))
                }
                if !model.state.hands.isEmpty {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Your compute").font(.system(size: 12, weight: .medium)).foregroundStyle(.secondary).padding(.bottom, 3)
                        ForEach(model.state.hands) { hand in HandCard(hand: hand) }
                    }
                }
                VStack(alignment: .leading, spacing: 14) {
                    Text("More places to work").font(.system(size: 16, weight: .medium))
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 205), spacing: 12)], spacing: 12) {
                        option("Cloud Hand", subtitle: "A workspace hosted for you.", icon: "cloud", action: model.createCloudHand)
                        option("Another computer", subtitle: "Connect your server or laptop.", icon: "desktopcomputer", action: { model.showingRemoteSetup = true })
                    }
                    Button { model.discoverHands() } label: { Label("Find Hands already connected to my account", systemImage: "arrow.clockwise") }.buttonStyle(.link).font(.system(size: 12)).disabled(!model.state.connected)
                }.padding(.top, 9)
            }.frame(maxWidth: 780, alignment: .leading).padding(.horizontal, 30).padding(.vertical, 32).frame(maxWidth: .infinity)
        }.accessibilityIdentifier("hands-page")
    }
    private func option(_ title: String, subtitle: String, icon: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(alignment: .leading, spacing: 15) {
                HStack { Image(systemName: icon).font(.system(size: 21)); Spacer(); Image(systemName: "arrow.up.right").font(.system(size: 11)).foregroundStyle(.tertiary) }
                VStack(alignment: .leading, spacing: 5) { Text(title).font(.system(size: 13, weight: .medium)).lineLimit(1); Text(subtitle).font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(2) }
            }.padding(18).frame(maxWidth: .infinity, minHeight: 100, alignment: .leading).background(Color.primary.opacity(0.025), in: RoundedRectangle(cornerRadius: 12)).overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Color.primary.opacity(0.035)))
        }.buttonStyle(.plain).disabled(!model.state.connected)
    }
}

struct HandCard: View {
    @EnvironmentObject private var model: AppModel
    let hand: Hand
    var body: some View {
        VStack(alignment: .leading, spacing: 13) {
            HStack(spacing: 12) {
                Image(systemName: hand.kind == "vm" ? "shippingbox" : "laptopcomputer").font(.system(size: 21)).frame(width: 42, height: 42).background(Color.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 10))
                VStack(alignment: .leading, spacing: 5) {
                    Text(hand.name).font(.system(size: 14, weight: .semibold)).lineLimit(1)
                    Text(hand.workspace).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle).help(hand.workspace)
                }.layoutPriority(1)
                Spacer()
                status
                Menu {
                    Button("Show Activity") { model.selectedHandForLogs = hand }
                    Button("Show Folder in Finder") { NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: hand.workspace) }
                    Button("Edit…") { model.editingHand = hand; model.showingHandSetup = true }.disabled(hand.isRunning)
                    Divider()
                    Button("Remove Hand", role: .destructive) { Task { await model.removeHand(hand.id) } }
                } label: { Image(systemName: "ellipsis") }.menuStyle(.borderlessButton).fixedSize()
            }
            Divider().opacity(0.45)
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 4) {
                    Label(hand.agentId == nil ? "All your threads" : "This thread", systemImage: hand.agentId == nil ? "square.stack" : "bubble.left")
                    if let active = hand.activeCalls, active > 0 { Text("\(active) command\(active == 1 ? "" : "s") running").foregroundStyle(.green) }
                    else if let calls = hand.calls, calls > 0 { Text("\(calls) command\(calls == 1 ? "" : "s") completed").foregroundStyle(.tertiary) }
                }.font(.system(size: 11)).foregroundStyle(.secondary)
                Spacer(minLength: 8)
                if hand.status == "connected" { Button("Use in a tab") { Task { await model.useHand(hand) } }.buttonStyle(.bordered).controlSize(.small).disabled(model.busyHands.contains(hand.id)).accessibilityIdentifier("use-hand-\(hand.id)") }
                Button { Task { if hand.isRunning { await model.stopHand(hand.id) } else { await model.startHand(hand.id) } } } label: {
                    if model.busyHands.contains(hand.id) { ProgressView().controlSize(.mini).frame(width: 33) }
                    else { Text(hand.isRunning ? "Stop" : hand.status == "error" ? "Retry" : "Start").frame(minWidth: 33) }
                }.buttonStyle(.bordered).controlSize(.small).disabled(model.busyHands.contains(hand.id)).accessibilityIdentifier("toggle-hand-\(hand.id)")
            }
            if let error = hand.error { Text(error).font(.system(size: 12)).foregroundStyle(.orange).textSelection(.enabled) }
        }.padding(18).background(Color.primary.opacity(0.012), in: RoundedRectangle(cornerRadius: 13)).overlay(RoundedRectangle(cornerRadius: 13).strokeBorder(Color.primary.opacity(0.085))).accessibilityElement(children: .contain).accessibilityIdentifier("hand-\(hand.id)")
    }
    private var status: some View {
        HStack(spacing: 5) { Circle().fill(hand.status == "connected" ? .green : hand.status == "error" ? .orange : .secondary.opacity(0.5)).frame(width: 5, height: 5); Text(hand.status == "connecting" ? "Connecting…" : (hand.status ?? "stopped").capitalized).font(.system(size: 11)) }.foregroundStyle(.secondary).padding(.horizontal, 8).padding(.vertical, 5).background(Color.primary.opacity(0.035), in: Capsule()).fixedSize()
    }
}

struct HandSetupView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let hand: Hand?
    @State private var name = "This Mac"
    @State private var workspace = ""
    @State private var kind = "local"
    @State private var thisThreadOnly = false
    @State private var advanced = false
    @State private var binary = ""
    @State private var rootfs = ""
    @State private var guestRuntime = ""
    @State private var cpus = 2
    @State private var memory = 2048
    @State private var network = true
    @State private var saving = false
    private var valid: Bool { !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !workspace.isEmpty && (kind == "local" || (!binary.isEmpty && !rootfs.isEmpty && !guestRuntime.isEmpty)) }
    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text(hand == nil ? "Add a Hand" : "Edit Hand").font(.system(size: 23, weight: .semibold))
            Text(kind == "local" ? "Give your agents a workspace on this Mac." : "Run agent commands inside a private Linux VM.").font(.system(size: 13)).foregroundStyle(.secondary)
            Form {
                TextField("Name", text: $name).accessibilityIdentifier("hand-name")
                pathRow("Folder", path: $workspace, directory: true)
                if kind == "local" {
                    Text("Commands run as your macOS user. Stop the Hand whenever you want to disconnect it.").font(.caption).foregroundStyle(.secondary)
                    Toggle("Only this thread", isOn: $thisThreadOnly).disabled(model.activeTab?.threadId == nil)
                } else {
                    if binary.isEmpty || rootfs.isEmpty || guestRuntime.isEmpty {
                        Text("A prepared nanocodex2 VM runtime is required. Choose the runtime and image below, or use a Cloud Hand for automatic setup.").font(.caption).foregroundStyle(.secondary)
                    }
                    pathRow("Runtime", path: $binary, directory: false)
                    pathRow("Linux image", path: $rootfs, directory: false)
                    pathRow("Guest runtime", path: $guestRuntime, directory: false)
                }
                DisclosureGroup("Advanced", isExpanded: $advanced) {
                    Picker("Compute", selection: $kind) { Text("This Mac").tag("local"); Text("Virtual machine").tag("vm") }
                    if kind == "vm" {
                        Stepper("\(cpus) CPU cores", value: $cpus, in: 1...32)
                        Picker("Memory", selection: $memory) { Text("2 GB").tag(2048); Text("4 GB").tag(4096); Text("8 GB").tag(8192); Text("16 GB").tag(16384) }
                        Toggle("Allow network access", isOn: $network)
                    }
                }
            }.formStyle(.grouped)
            if let error = model.error { Text(error).font(.caption).foregroundStyle(.orange).textSelection(.enabled) }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button(saving ? "Saving…" : hand == nil ? "Enable Hand" : "Save") {
                    saving = true
                    let config = Hand(id: hand?.id ?? "mac-\(UUID().uuidString.prefix(8).lowercased())", name: name, kind: kind, workspace: workspace, agentId: thisThreadOnly ? model.activeTab?.threadId : nil, rootfs: kind == "vm" ? rootfs : nil, guestRuntime: kind == "vm" ? guestRuntime : nil, binary: kind == "vm" ? binary : nil, cpus: cpus, memoryMiB: memory, network: network)
                    Task { await model.saveHand(config, start: hand == nil); saving = false }
                }.buttonStyle(.borderedProminent).tint(.primary).disabled(!valid || saving).keyboardShortcut(.defaultAction).accessibilityIdentifier("save-hand")
            }
        }.padding(25).frame(width: 530).onAppear {
            model.error = nil
            name = hand?.name ?? "This Mac"; workspace = hand?.workspace ?? model.state.defaults["workspace"].string
            kind = hand?.kind ?? "local"; thisThreadOnly = hand?.agentId != nil
            binary = hand?.binary ?? model.state.defaults["binary"].string
            rootfs = hand?.rootfs ?? model.state.defaults["rootfs"].string
            guestRuntime = hand?.guestRuntime ?? model.state.defaults["guestRuntime"].string
            cpus = hand?.cpus ?? 2; memory = hand?.memoryMiB ?? 2048; network = hand?.network ?? true
        }
    }
    private func pathRow(_ title: String, path: Binding<String>, directory: Bool) -> some View {
        LabeledContent(title) {
            HStack {
                Text(path.wrappedValue.isEmpty ? "Choose…" : URL(fileURLWithPath: path.wrappedValue).lastPathComponent).foregroundStyle(path.wrappedValue.isEmpty ? .tertiary : .secondary).lineLimit(1).truncationMode(.middle).help(path.wrappedValue)
                Button("Choose…") { let panel = NSOpenPanel(); panel.canChooseFiles = !directory; panel.canChooseDirectories = directory; panel.canCreateDirectories = directory; if panel.runModal() == .OK, let url = panel.url { path.wrappedValue = url.path } }
            }
        }
    }
}

struct HandLogView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let id: String
    var hand: Hand? { model.state.hands.first { $0.id == id } }
    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack { Text("\(hand?.name ?? "Hand") activity").font(.title2); Spacer(); Button("Done") { dismiss() }.keyboardShortcut(.cancelAction) }
            ScrollView { Text((hand?.logs ?? ["No activity yet."]).joined(separator: "\n")).font(.system(size: 12, design: .monospaced)).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading) }
        }.padding(24).frame(width: 640, height: 410)
    }
}

struct RemoteSetupView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var advanced = false
    var command: String { "nanocodex2 hand \\\n  --vm /path/to/linux.ext4 \\\n  --vm-guest-runtime /path/to/nanocodex-vm-guest \\\n  --vm-workspace /workspace \\\n  --machine-id remote-computer \\\n  --machine-name 'Remote computer'" }
    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Connect another computer").font(.system(size: 23, weight: .semibold))
            VStack(alignment: .leading, spacing: 14) {
                Label("Open Nanocodex on the other Mac.", systemImage: "1.circle")
                Label("Connect the same Nanocodex account.", systemImage: "2.circle")
                Label("Open Hands and choose Enable this Mac.", systemImage: "3.circle")
            }.font(.system(size: 14)).padding(.vertical, 6)
            Text("Its compute becomes available to your agents here. Keep Nanocodex running on that computer while you use it.").font(.system(size: 13)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            DisclosureGroup("Server or VM with nanocodex2", isExpanded: $advanced) {
                VStack(alignment: .leading, spacing: 12) {
                    Text("With a prepared Linux image and guest runtime, set NANOCODEX_API_KEY in the server’s environment and run:").font(.system(size: 12)).foregroundStyle(.secondary)
                    Text(command).font(.system(size: 11, design: .monospaced)).textSelection(.enabled).padding(14).frame(maxWidth: .infinity, alignment: .leading).background(Color.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 9))
                    Button("Copy Command") { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(command, forType: .string) }
                }.padding(.top, 12)
            }
            HStack {
                Button("Open Account") { model.openAccount() }
                Spacer()
                Button("Find Connected Hands") { dismiss(); model.discoverHands() }
                Button("Done") { dismiss() }.buttonStyle(.borderedProminent).tint(.primary).keyboardShortcut(.cancelAction)
            }
        }.padding(27).frame(width: 575)
    }
}
