import QuickLook
import SwiftUI

/// The system owns presentation, image zoom, video playback, sharing and dismissal.
/// Original files are loaded on demand and live only for the preview's lifetime.
public struct ChatMediaPreview<Label: View>: View {
    private let load: () async throws -> [URL]
    private let title: String?
    private let label: Label
    @State private var loading = false
    @State private var selection: URL?
    @State private var files: [URL] = []
    @State private var failure: String?

    public init(title: String? = nil, load: @escaping () async throws -> [URL], @ViewBuilder label: () -> Label) {
        self.title = title; self.load = load; self.label = label()
    }
    public var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button { loading = true; failure = nil } label: {
                label.overlay { if loading { ProgressView().padding(8).background(.regularMaterial, in: Circle()) } }
            }.buttonStyle(.plain).disabled(loading)
            if let failure { Text(failure).font(.caption).foregroundStyle(.secondary) }
        }
        .nativeMediaPreview($selection, in: files, title: title)
        .task(id: loading) {
            guard loading else { return }
            do {
                let loaded = try await load()
                guard !Task.isCancelled else { Self.remove(loaded); return }
                guard !loaded.isEmpty else { throw CocoaError(.fileReadUnknown) }
                files = loaded; selection = loaded.first
            } catch { if !Task.isCancelled { failure = "Couldn’t open attachment. Tap to retry." } }
            loading = false
        }
        .onChange(of: selection) { _, value in
            if value == nil { Self.remove(files); files = [] }
        }
        .onDisappear { if selection == nil { Self.remove(files); files = [] } }
    }
    private static func remove(_ urls: [URL]) {
        for url in urls where url.isFileURL { try? FileManager.default.removeItem(at: url) }
    }
}

public extension View {
    /// Keep Quick Look's navigation controls independent of the conversation's
    /// hidden navigation bar. Rendering and gestures remain entirely native.
    @ViewBuilder func nativeMediaPreview(_ selection: Binding<URL?>, in files: [URL], title: String? = nil) -> some View {
        #if os(iOS)
        fullScreenCover(isPresented: Binding(get: { selection.wrappedValue != nil }, set: { if !$0 { selection.wrappedValue = nil } })) {
            NativeQuickLook(files: files, selected: selection.wrappedValue, title: title) { selection.wrappedValue = nil }
                .ignoresSafeArea()
        }
        #else
        quickLookPreview(selection, in: files)
        #endif
    }
}

#if os(iOS)
private struct NativeQuickLook: UIViewControllerRepresentable {
    let files: [URL]
    let selected: URL?
    let title: String?
    let dismiss: () -> Void
    func makeCoordinator() -> Coordinator { Coordinator(files: files, title: title, dismiss: dismiss) }
    func makeUIViewController(context: Context) -> UINavigationController {
        let preview = QLPreviewController()
        preview.dataSource = context.coordinator
        preview.currentPreviewItemIndex = files.firstIndex { $0 == selected } ?? 0
        preview.navigationItem.leftBarButtonItem = UIBarButtonItem(barButtonSystemItem: .done, target: context.coordinator, action: #selector(Coordinator.close))
        return UINavigationController(rootViewController: preview)
    }
    func updateUIViewController(_ controller: UINavigationController, context: Context) {}
    final class Coordinator: NSObject, QLPreviewControllerDataSource {
        let items: [Item]
        let dismiss: () -> Void
        init(files: [URL], title: String?, dismiss: @escaping () -> Void) {
            items = files.map { Item(url: $0, title: title) }; self.dismiss = dismiss
        }
        func numberOfPreviewItems(in controller: QLPreviewController) -> Int { items.count }
        func previewController(_ controller: QLPreviewController, previewItemAt index: Int) -> QLPreviewItem { items[index] }
        @objc func close() { dismiss() }
    }
    final class Item: NSObject, QLPreviewItem {
        let previewItemURL: URL?
        let previewItemTitle: String?
        init(url: URL, title: String?) { previewItemURL = url; previewItemTitle = title }
    }
}
#endif

public enum ChatMediaFile {
    /// Returns a disposable original copy, not the downsampled transcript image.
    public static func inline(_ source: String) async throws -> URL {
        let output = await Task.detached(priority: .utility) { ChatGeneratedOutput.image(source: source) }.value
        try Task.checkCancellation()
        guard let output, output.kind == .image else { throw CocoaError(.fileReadCorruptFile) }
        return try await GeneratedAsset.previewURL(output)
    }
}
