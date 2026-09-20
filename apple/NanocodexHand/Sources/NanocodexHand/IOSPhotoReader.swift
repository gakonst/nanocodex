#if os(iOS)
import Foundation
import Photos
import UIKit

/// One bounded PhotoKit request. No permission prompt or iCloud transfer.
@MainActor
final class IOSPhotoReader {
    private let manager = PHImageManager.default()
    private var requestID: PHImageRequestID?
    private var continuation: CheckedContinuation<UIImage, Error>?
    private var timer: Task<Void, Never>?

    static func read(id: String) async throws -> Data {
        let asset = try accessibleImage(id)
        let reader = IOSPhotoReader()
        let image = try await reader.image(asset)
        try Task.checkCancellation()
        // Permission or limited-library membership can change while PhotoKit runs.
        _ = try accessibleImage(id)
        let worker = Task.detached(priority: .userInitiated) { try encode(image) }
        let data = try await withTaskCancellationHandler {
            try await worker.value
        } onCancel: { worker.cancel() }
        try Task.checkCancellation()
        _ = try accessibleImage(id)
        return data
    }

    private static func accessibleImage(_ id: String) throws -> PHAsset {
        let status = PHPhotoLibrary.authorizationStatus(for: .readWrite)
        guard status == .authorized || status == .limited else {
            throw HandFailure.contextAccess("Photos read permission is not granted. Enable it in Settings before reading photos.")
        }
        guard let asset = PHAsset.fetchAssets(withLocalIdentifiers: [id], options: nil).firstObject else {
            throw HandFailure.contextAccess("Photo is unavailable or outside the currently accessible library.")
        }
        guard asset.mediaType == .image else { throw HandFailure.contextAccess("read_photo supports images only.") }
        return asset
    }

    private func image(_ asset: PHAsset) async throws -> UIImage {
        try Task.checkCancellation()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                self.continuation = continuation
                let options = PHImageRequestOptions()
                options.isNetworkAccessAllowed = false
                options.deliveryMode = .highQualityFormat
                options.resizeMode = .exact
                options.version = .current
                requestID = manager.requestImage(for: asset,
                    targetSize: CGSize(width: HandPhotoRendition.maxPixels, height: HandPhotoRendition.maxPixels),
                    contentMode: .aspectFit, options: options) { image, info in
                    let cancelled = (info?[PHImageCancelledKey] as? Bool) == true
                    let degraded = (info?[PHImageResultIsDegradedKey] as? Bool) == true
                    let failed = info?[PHImageErrorKey] != nil
                    Task { @MainActor [weak self] in
                        guard let self else { return }
                        if cancelled { self.finish(.failure(CancellationError())) }
                        else if failed { self.finish(.failure(HandFailure.contextAccess("The photo rendition could not be read."))) }
                        else if degraded { return }
                        else if let image { self.finish(.success(image)) }
                        else { self.finish(.failure(HandFailure.contextAccess("This photo is not available locally. Open it in Photos to download it, then retry."))) }
                    }
                }
                timer = Task { [weak self] in
                    do { try await Task.sleep(for: .seconds(10)) } catch { return }
                    self?.finish(.failure(HandFailure.contextAccess("Reading the photo timed out.")))
                }
            }
        } onCancel: {
            Task { @MainActor [weak self] in self?.finish(.failure(CancellationError())) }
        }
    }

    private func finish(_ result: Result<UIImage, Error>) {
        guard let continuation else { return }
        self.continuation = nil
        timer?.cancel(); timer = nil
        if let requestID { manager.cancelImageRequest(requestID) }
        requestID = nil
        continuation.resume(with: result)
    }

    nonisolated static func encode(_ image: UIImage) throws -> Data {
        guard image.size.width.isFinite, image.size.height.isFinite,
              image.size.width > 0, image.size.height > 0 else { throw HandFailure.invalidFile }
        var dimension = CGFloat(HandPhotoRendition.maxPixels)
        while dimension >= 256 {
            try Task.checkCancellation()
            let ratio = min(1, dimension / max(image.size.width, image.size.height))
            let size = CGSize(width: max(1, floor(image.size.width * ratio)), height: max(1, floor(image.size.height * ratio)))
            let format = UIGraphicsImageRendererFormat()
            format.scale = 1; format.opaque = true
            let rendered = UIGraphicsImageRenderer(size: size, format: format).image { context in
                UIColor.white.setFill(); context.fill(CGRect(origin: .zero, size: size))
                image.draw(in: CGRect(origin: .zero, size: size))
            }
            if let data = rendered.jpegData(compressionQuality: 0.85), data.count <= HandPhotoRendition.maxBytes {
                _ = try HandPhotoRendition.dimensions(data)
                return data
            }
            dimension = floor(dimension * 0.75)
        }
        throw HandFailure.contextAccess("The photo could not be rendered within the image size limit.")
    }
}
#endif
