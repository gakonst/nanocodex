import UIKit
import SwiftUI
import NanocodexContext

final class ShareViewController: UIViewController {
    override func viewDidLoad() {
        super.viewDidLoad()
        let providers = (extensionContext?.inputItems as? [NSExtensionItem] ?? []).flatMap { item -> [NSItemProvider] in
            var providers = item.attachments ?? []
            if let text = item.attributedContentText?.string, !text.isEmpty {
                providers.append(NSItemProvider(object: text as NSString))
            }
            return providers
        }
        let root = ShareCaptureView(providers: providers) { [weak self] in
            self?.extensionContext?.completeRequest(returningItems: nil)
        }
        let host = UIHostingController(rootView: root)
        addChild(host); host.view.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(host.view)
        NSLayoutConstraint.activate([
            host.view.leadingAnchor.constraint(equalTo: view.leadingAnchor), host.view.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            host.view.topAnchor.constraint(equalTo: view.topAnchor), host.view.bottomAnchor.constraint(equalTo: view.bottomAnchor)
        ])
        host.didMove(toParent: self)
    }
}

private struct ShareCaptureView: View {
    let providers: [NSItemProvider]
    let finish: () -> Void
    @State private var items: [CaptureInput] = []
    @State private var source = "Shared"
    @State private var session: CaptureSession?
    @State private var loading = true
    @State private var saving = false
    @State private var error: String?
    var body: some View {
        NavigationStack {
            Form {
                Section {
                    if session?.scope.hasPrefix("demo.") == true { Text("Demo context").font(.caption.weight(.semibold)) }
                    Text("Save on this device so your agents can query this text through its Hand while capture is enabled.")
                        .font(.subheadline).foregroundStyle(.secondary)
                    TextField("Source app", text: $source).accessibilityIdentifier("capture-source")
                }
                if loading { ProgressView("Reading shared content…") }
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    Section(item.filename.isEmpty ? "Content" : "Text from \(item.filename)") {
                        Text(item.text.isEmpty ? item.url : item.text).textSelection(.enabled)
                        if !item.text.isEmpty, !item.url.isEmpty {
                            Text(item.url).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
                if let error { Section { Text(error).foregroundStyle(.red).accessibilityIdentifier("capture-error") } }
            }
            .navigationTitle("Save to Nanocodex")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel", action: finish) }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        guard let session, !saving else { return }
                        saving = true
                        do {
                            let inputs = items.map { item in var item = item; item.source = source; return item }
                            try ContextStore.shared().capture(inputs, session: session)
                            finish()
                        } catch { self.error = error.localizedDescription; saving = false }
                    }.disabled(loading || saving || items.isEmpty || session == nil).accessibilityIdentifier("capture-save")
                }
            }
            .task {
                defer { loading = false }
                do {
                    session = try ContextStore.shared().captureSession()
                    let loaded = try await ContextImport.load(providers)
                    items = loaded
                    if let first = loaded.first { source = first.source }
                } catch { self.error = error.localizedDescription }
            }
        }
    }
}
