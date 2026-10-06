import SwiftUI
import Photos
import PhotosUI
import UniformTypeIdentifiers
import UIKit

/// The composer keeps ownership of picker presentation and attachment imports.
struct AttachmentLibrarySheet: View {
    let isPreparing: Bool
    let onCamera: () -> Void
    let onPhotos: () -> Void
    let onFiles: () -> Void
    let onVideos: () -> Void
    let onRecentPhotos: ([NSItemProvider]) -> Void
    @StateObject private var library = AttachmentRecentPhotos()
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var manageLimitedAccess = false
    @State private var selectedPhotoIDs: [String] = []
    @State private var submitting = false
    @State private var sheetDetent: PresentationDetent = .height(380)

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                HStack {
                    Text("Library").font(.title2.weight(.semibold))
                    Spacer()
                    Button("See all", action: onPhotos)
                        .font(.body.weight(.medium))
                        .frame(minHeight: 44)
                        .accessibilityIdentifier("choose-photos")
                }
                .padding(.horizontal, 24)

                ScrollView(.horizontal) {
                    LazyHStack(spacing: 10) {
                        Button(action: onCamera) {
                            Image(systemName: "camera.fill")
                                .font(.system(size: 30, weight: .medium))
                                .frame(width: 94, height: 126)
                                .background(Color(uiColor: .tertiarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 18))
                        }
                        .accessibilityLabel("Camera")
                        .accessibilityIdentifier("choose-camera")

                        ForEach(library.assets, id: \.localIdentifier) { asset in
                            Button { togglePhoto(asset.localIdentifier) } label: {
                                AttachmentRecentPhotoThumbnail(asset: asset)
                                    .overlay(alignment: .topTrailing) {
                                        if selectedPhotoIDs.contains(asset.localIdentifier) {
                                            Image(systemName: "checkmark.circle.fill")
                                                .font(.system(size: 24, weight: .semibold))
                                                .symbolRenderingMode(.palette)
                                                .foregroundStyle(.white, Color.blue)
                                                .background(.white, in: Circle())
                                                .padding(6)
                                        }
                                    }
                            }
                            .accessibilityLabel(asset.creationDate.map { "Photo from \($0.formatted(date: .abbreviated, time: .shortened))" } ?? "Recent photo")
                            .accessibilityAddTraits(selectedPhotoIDs.contains(asset.localIdentifier) ? [.isSelected] : [])
                            .accessibilityValue(selectedPhotoIDs.contains(asset.localIdentifier) ? "Selected" : "Not selected")
                            .accessibilityHint("Double tap to toggle selection")
                            .accessibilityIdentifier("recent-photo-" + asset.localIdentifier)
                        }
                        if library.assets.isEmpty {
                            Button(action: library.authorization == .notDetermined ? requestRecentPhotos : onPhotos) {
                                VStack(spacing: 8) {
                                    Image(systemName: "photo.on.rectangle").font(.title2)
                                    Text(library.authorization == .notDetermined ? "Show recent photos" : "Choose photos")
                                        .font(.subheadline.weight(.medium))
                                        .multilineTextAlignment(.center)
                                }
                                .padding(.horizontal, 12)
                                .frame(width: 156, height: 126)
                                .background(Color(uiColor: .tertiarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 18))
                            }
                            .accessibilityIdentifier("recent-photos-access")
                        }
                    }
                    .padding(.horizontal, 24)
                }
                .scrollIndicators(.hidden)
                .padding(.top, -14)
                .accessibilityIdentifier("recent-photos")

                if library.authorization == .limited {
                    HStack {
                        Text("Selected photos only").foregroundStyle(.secondary)
                        Spacer()
                        Button("Manage") { manageLimitedAccess = true }
                            .frame(minHeight: 44)
                            .accessibilityIdentifier("manage-limited-photos")
                    }
                    .font(.footnote)
                    .padding(.horizontal, 24)
                    .padding(.top, -14)
                }

                VStack(spacing: 0) {
                    option("Add files", symbol: "paperclip", identifier: "choose-files", action: onFiles)
                    Divider().padding(.leading, 60)
                    option("Add videos", symbol: "play.circle", identifier: "choose-videos", action: onVideos)
                }
                .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 22))
                .padding(.horizontal, 24)


            }
            .padding(.top, 20)
            .padding(.bottom, 16)
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if !selectedPhotoIDs.isEmpty {
                Button(action: addSelectedPhotos) {
                    Text(selectedPhotoIDs.count == 1 ? "Add 1 Attachment" : "Add \(selectedPhotoIDs.count) Attachments")
                        .font(.body.weight(.semibold))
                        .frame(maxWidth: .infinity, minHeight: 28)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.regular)
                .tint(.blue)
                .buttonBorderShape(.capsule)
                .accessibilityIdentifier("add-selected-attachments")
                .padding(.horizontal, 24).padding(.vertical, 8)
                .background(Color(uiColor: .systemGroupedBackground))
                .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(reduceMotion ? nil : .smooth(duration: 0.28), value: selectedPhotoIDs.isEmpty)
        .buttonStyle(.plain)
        .foregroundStyle(.primary)
        .disabled(isPreparing || submitting)
        .background(Color(uiColor: .systemGroupedBackground))
        .presentationBackground(Color(uiColor: .systemGroupedBackground))
        .presentationDetents(dynamicTypeSize.isAccessibilitySize ? [.large] : [.height(380), .height(440), .large], selection: $sheetDetent)
        .presentationDragIndicator(.visible)
        .presentationCornerRadius(32)
        .background(AttachmentLimitedLibraryPresenter(isPresented: $manageLimitedAccess, onFinish: library.reload).frame(width: 0, height: 0))
        .task { library.reload() }
        .onChange(of: selectedPhotoIDs.isEmpty) { _, empty in
            guard !dynamicTypeSize.isAccessibilitySize, sheetDetent != .large else { return }
            withAnimation(reduceMotion ? nil : .smooth(duration: 0.28)) {
                sheetDetent = .height(empty ? 380 : 440)
            }
        }
        .onChange(of: dynamicTypeSize.isAccessibilitySize) { _, accessible in
            sheetDetent = accessible ? .large : .height(selectedPhotoIDs.isEmpty ? 380 : 440)
        }
        .onChange(of: library.assets.map(\.localIdentifier)) { _, available in
            selectedPhotoIDs.removeAll { !available.contains($0) }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { library.reload() }
        }
        .accessibilityIdentifier("attachment-library-sheet")
    }

    private func togglePhoto(_ identifier: String) {
        if selectedPhotoIDs.contains(identifier) {
            selectedPhotoIDs.removeAll { $0 == identifier }
        } else {
            selectedPhotoIDs.append(identifier)
        }
    }

    private func addSelectedPhotos() {
        guard !submitting, !isPreparing else { return }
        let assets = selectedPhotoIDs.compactMap { id in library.assets.first { $0.localIdentifier == id } }
        guard !assets.isEmpty else { return }
        submitting = true
        onRecentPhotos(assets.map { library.provider(for: $0) })
    }

    private func option(_ title: String, symbol: String, identifier: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 16) {
                Image(systemName: symbol).font(.system(size: 24)).frame(width: 28)
                Text(title).font(.body)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 19)
            .frame(minHeight: 62)
            .contentShape(Rectangle())
        }
        .accessibilityIdentifier(identifier)
    }

    private func requestRecentPhotos() {
        Task {
            _ = await PHPhotoLibrary.requestAuthorization(for: .readWrite)
            library.reload()
        }
    }
}

@MainActor
private final class AttachmentRecentPhotos: NSObject, ObservableObject, PHPhotoLibraryChangeObserver {
    @Published private(set) var assets: [PHAsset] = []
    @Published private(set) var authorization = PHPhotoLibrary.authorizationStatus(for: .readWrite)
    private var observing = false

    deinit {
        if observing { PHPhotoLibrary.shared().unregisterChangeObserver(self) }
    }

    func reload() {
        authorization = PHPhotoLibrary.authorizationStatus(for: .readWrite)
        guard authorization == .authorized || authorization == .limited else {
            assets = []
            return
        }
        if !observing {
            PHPhotoLibrary.shared().register(self)
            observing = true
        }
        let options = PHFetchOptions()
        options.sortDescriptors = [NSSortDescriptor(key: "creationDate", ascending: false)]
        options.fetchLimit = 24
        let result = PHAsset.fetchAssets(with: .image, options: options)
        var recent: [PHAsset] = []
        result.enumerateObjects { asset, _, _ in recent.append(asset) }
        assets = recent
    }

    nonisolated func photoLibraryDidChange(_ changeInstance: PHChange) {
        Task { @MainActor [weak self] in self?.reload() }
    }

    /// Load full-size current image bytes only after selection. The existing
    /// provider import owns cancellation, preparation, storage, and upload.
    func provider(for asset: PHAsset) -> NSItemProvider {
        let provider = NSItemProvider()
        provider.registerDataRepresentation(forTypeIdentifier: UTType.image.identifier, visibility: .all) { completion in
            let progress = Progress(totalUnitCount: 1)
            let manager = PHImageManager.default()
            let options = PHImageRequestOptions()
            options.version = .current
            options.deliveryMode = .highQualityFormat
            options.isNetworkAccessAllowed = true
            let request = manager.requestImageDataAndOrientation(for: asset, options: options) { data, _, _, info in
                if let error = info?[PHImageErrorKey] as? Error {
                    completion(nil, error)
                } else if (info?[PHImageCancelledKey] as? Bool) == true {
                    completion(nil, CancellationError())
                } else if let data {
                    progress.completedUnitCount = 1
                    completion(data, nil)
                } else {
                    completion(nil, NSError(domain: "AttachmentRecentPhotos", code: 1,
                        userInfo: [NSLocalizedDescriptionKey: "This photo could not be loaded. Try choosing it from See all."]))
                }
            }
            progress.cancellationHandler = { manager.cancelImageRequest(request) }
            return progress
        }
        return provider
    }
}

private struct AttachmentRecentPhotoThumbnail: View {
    let asset: PHAsset
    @Environment(\.displayScale) private var displayScale
    @State private var image: UIImage?
    @State private var request: PHImageRequestID?

    var body: some View {
        ZStack {
            Color(uiColor: .tertiarySystemGroupedBackground)
            if let image {
                Image(uiImage: image).resizable().scaledToFill()
            } else {
                Image(systemName: "photo").foregroundStyle(.secondary)
            }
        }
        .frame(width: 94, height: 126)
        .clipShape(RoundedRectangle(cornerRadius: 18))
        .onAppear {
            let options = PHImageRequestOptions()
            options.deliveryMode = .opportunistic
            options.resizeMode = .fast
            // Scrolling the strip does not download full originals from iCloud.
            options.isNetworkAccessAllowed = false
            request = PHImageManager.default().requestImage(for: asset,
                targetSize: CGSize(width: 94 * displayScale, height: 126 * displayScale),
                contentMode: .aspectFill, options: options) { result, _ in
                    Task { @MainActor in image = result }
                }
        }
        .onDisappear {
            if let request { PHImageManager.default().cancelImageRequest(request) }
            request = nil
        }
    }
}

/// Present from the sheet's controller, not an unrelated window/root controller.
private struct AttachmentLimitedLibraryPresenter: UIViewControllerRepresentable {
    @Binding var isPresented: Bool
    let onFinish: () -> Void

    final class Coordinator { var presenting = false }
    func makeCoordinator() -> Coordinator { Coordinator() }
    func makeUIViewController(context: Context) -> UIViewController { UIViewController() }

    func updateUIViewController(_ controller: UIViewController, context: Context) {
        guard isPresented, !context.coordinator.presenting, controller.viewIfLoaded?.window != nil,
              controller.presentedViewController == nil else { return }
        context.coordinator.presenting = true
        DispatchQueue.main.async {
            isPresented = false
            PHPhotoLibrary.shared().presentLimitedLibraryPicker(from: controller) { _ in
                Task { @MainActor in
                    context.coordinator.presenting = false
                    onFinish()
                }
            }
        }
    }
}
