import Foundation
import UniformTypeIdentifiers

/// Item-provider URLs only live for the duration of their completion callback.
/// Retain an original file before returning so decoding and storage can run later.
public enum AttachmentProviderImport {
    public static func copyImage(from provider: NSItemProvider) async throws -> URL {
        try Task.checkCancellation()
        guard let identifier = provider.registeredTypeIdentifiers.first(where: {
            UTType($0)?.conforms(to: .image) == true
        }) else { throw AttachmentError.unsupportedImage }
        let transfer = ProviderImageTransfer()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                guard transfer.begin(continuation) else { return }
                let progress = provider.loadFileRepresentation(forTypeIdentifier: identifier) { source, error in
                    do {
                        if let error { throw error }
                        guard let source else { throw AttachmentError.unavailable }
                        let suffix = source.pathExtension.isEmpty ? UTType(identifier)?.preferredFilenameExtension ?? "image" : source.pathExtension
                        let copy = FileManager.default.temporaryDirectory
                            .appendingPathComponent("pasted-image-" + UUID().uuidString).appendingPathExtension(suffix)
                        try FileManager.default.copyItem(at: source, to: copy)
                        transfer.finish(.success(copy))
                    } catch { transfer.finish(.failure(error)) }
                }
                transfer.setProgress(progress)
            }
        } onCancel: { transfer.cancel() }
    }
}

/// Cancellation must release the awaiting import even if a provider never
/// invokes its callback. A callback racing cancellation still owns its copy.
private final class ProviderImageTransfer: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<URL, Error>?
    private var progress: Progress?
    private var finished = false
    private var cancelled = false

    func begin(_ continuation: CheckedContinuation<URL, Error>) -> Bool {
        lock.lock()
        if finished {
            lock.unlock()
            continuation.resume(throwing: CancellationError())
            return false
        }
        self.continuation = continuation
        lock.unlock()
        return true
    }

    func setProgress(_ progress: Progress) {
        lock.lock()
        let cancel = cancelled
        if !finished { self.progress = progress }
        lock.unlock()
        if cancel { progress.cancel() }
    }

    func finish(_ result: Result<URL, Error>) {
        lock.lock()
        let continuation = self.continuation
        self.continuation = nil
        progress = nil
        finished = true
        lock.unlock()
        if let continuation { continuation.resume(with: result) }
        else if case .success(let copy) = result { try? FileManager.default.removeItem(at: copy) }
    }

    func cancel() {
        lock.lock()
        guard !finished else { lock.unlock(); return }
        cancelled = true
        finished = true
        let continuation = self.continuation, progress = self.progress
        self.continuation = nil
        self.progress = nil
        lock.unlock()
        continuation?.resume(throwing: CancellationError())
        progress?.cancel()
    }
}
