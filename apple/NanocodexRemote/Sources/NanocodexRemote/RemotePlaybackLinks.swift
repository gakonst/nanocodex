import SwiftUI
#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// View-only HLS playback links for one screen. The bearer URL exists only in this view's state
/// after an explicit create; it is never persisted, logged or re-requested.
struct RemotePlaybackLinks: View {
    let service: RemoteService
    let hand: RemoteHand

    private struct Pending: Equatable { let operationID: UUID; let expiresInSeconds: Int; let preset: RemotePlaybackPreset }
    private struct Created: Equatable { let id: String; let url: URL?; let expires: Date }

    @State private var expiresInSeconds = 3600
    @State private var preset = RemotePlaybackPreset.p720
    /// An unconfirmed create keeps its operation ID; only an explicit Retry resends it.
    @State private var unconfirmed: Pending?
    @State private var creating = false
    @State private var created: Created?
    @State private var copied = false
    @State private var links: [RemotePlaybackLink] = []
    @State private var loaded = false
    @State private var stopping: String?
    @State private var error: String?

    private var active: [RemotePlaybackLink] {
        links.filter { $0.isActive && $0.machineID == hand.machineID && $0.surfaceID == hand.id }
    }

    var body: some View {
        DisclosureGroup("Playback links" + (active.isEmpty ? "" : " · \(active.count) active")) {
            VStack(alignment: .leading, spacing: 8) {
                Picker("Expires after", selection: $expiresInSeconds) {
                    ForEach(RemoteService.playbackDurations, id: \.seconds) { Text($0.label).tag($0.seconds) }
                }.disabled(creating || unconfirmed != nil)
                Picker("Quality", selection: $preset) {
                    Text("720p").tag(RemotePlaybackPreset.p720)
                    Text("1080p").tag(RemotePlaybackPreset.p1080)
                }.disabled(creating || unconfirmed != nil)
                HStack {
                    Button(creating ? "Creating…" : unconfirmed == nil ? "Create playback link" : "Retry request") { Task { await create() } }
                        .disabled(creating)
                    if unconfirmed != nil && !creating {
                        Button("Discard request") { unconfirmed = nil; error = nil }
                    }
                }
                Text("Anyone with the link can watch this screen (view only, no control or sound) until it expires or you stop it.")
                    .font(.caption).foregroundStyle(.secondary)
                if let error {
                    Text(error + (unconfirmed == nil ? "" : " Check active links first; Retry reuses the same request and cannot create a duplicate."))
                        .font(.caption).foregroundStyle(.red)
                }
                if let created {
                    if let url = created.url {
                        Text(url.absoluteString).font(.caption.monospaced()).textSelection(.enabled).lineLimit(3)
                        HStack {
                            Button(copied ? "Copied" : "Copy link") { copy(url) }
                            ShareLink(item: url) { Text("Share") }
                        }
                        Text("Anyone with this link can watch until \(created.expires.formatted(date: .abbreviated, time: .shortened)). It won't be shown again.")
                            .font(.caption).foregroundStyle(.secondary)
                    } else {
                        Text("This link was already created, but its URL can't be shown again. Stop it and create a new one.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                Text("Active links").font(.subheadline.weight(.semibold))
                if !loaded {
                    ProgressView().controlSize(.small)
                } else if active.isEmpty {
                    Text("No active playback links for this screen.").font(.caption).foregroundStyle(.secondary)
                } else {
                    ForEach(active) { link in
                        HStack {
                            Text("\(link.preset.rawValue) · \(link.state == .live ? "Live" : "Starting") · expires \(link.expiresDate.formatted(date: .omitted, time: .shortened))")
                                .font(.caption)
                            Spacer()
                            Button(stopping == link.id ? "Stopping…" : "Stop", role: .destructive) { Task { await stop(link.id) } }
                                .disabled(stopping == link.id)
                        }
                    }
                }
            }.padding(.top, 6)
        }
        .task(id: hand.identity) {
            // Periodic list refresh while visible; listing never returns URLs.
            while !Task.isCancelled {
                await refresh()
                try? await Task.sleep(for: .seconds(10))
            }
        }
        .onChange(of: hand.identity) { _, _ in created = nil; unconfirmed = nil; error = nil; copied = false }
    }

    private func refresh() async {
        do { links = try await service.playbackLinks(); loaded = true }
        catch { if !loaded { self.error = error.localizedDescription } }
    }

    private func create() async {
        guard !creating else { return }
        let request = unconfirmed ?? Pending(operationID: UUID(), expiresInSeconds: expiresInSeconds, preset: preset)
        creating = true; error = nil; copied = false; created = nil
        defer { creating = false }
        do {
            let receipt = try await service.createPlaybackLink(hand: hand, operationID: request.operationID,
                expiresInSeconds: request.expiresInSeconds, preset: request.preset)
            unconfirmed = nil
            created = Created(id: receipt.link.id, url: receipt.url, expires: receipt.link.expiresDate)
        } catch {
            unconfirmed = (error as? RemotePlaybackError)?.uncertain == true ? request : nil
            self.error = error.localizedDescription
        }
        await refresh()
    }

    private func stop(_ id: String) async {
        stopping = id; error = nil
        defer { stopping = nil }
        do { try await service.revokePlaybackLink(id: id); if created?.id == id { created = nil } }
        catch { self.error = error.localizedDescription }
        await refresh()
    }

    private func copy(_ url: URL) {
#if os(iOS)
        UIPasteboard.general.url = url
#else
        NSPasteboard.general.clearContents(); NSPasteboard.general.setString(url.absoluteString, forType: .string)
#endif
        copied = true
    }
}
