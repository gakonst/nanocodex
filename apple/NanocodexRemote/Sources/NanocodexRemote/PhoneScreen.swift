#if os(macOS)
import Foundation
import CoreGraphics
import CoreVideo
import ImageIO
import WebRTC

/// Paired developer iPhone backend. The runner is reached only over the local
/// USB/device tunnel. Screenshot decoding never blocks the control actor.
final class PhoneScreen: NSObject, RemoteCapture, URLSessionDataDelegate, @unchecked Sendable {
    private let source: RTCVideoSource
    private let capturer: RTCVideoCapturer
    private let endpoint: URL
    private let expectedSize: CGSize?
    private var session: URLSession?
    private var request: URLSessionDataTask?
    private var bytes = Data()
    private var imageLength: Int?
    private var decodedPartLength: Int?
    private let lock = NSLock()
    private var stopped = false
    private let snapshotBuffer = RemoteSnapshotBuffer()
    var onFailure: @Sendable (Error) -> Void = { _ in }

    init(source: RTCVideoSource, port: Int, expectedSize: CGSize? = nil) throws {
        guard (1024...65535).contains(port) else { throw RemoteError.invalidMessage }
        self.source = source; capturer = RTCVideoCapturer(delegate: source)
        self.expectedSize = expectedSize
        endpoint = URL(string: "http://127.0.0.1:\(port)/")!
        super.init()
    }
    @MainActor func start() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 10; configuration.timeoutIntervalForResource = 86_400
        let queue = OperationQueue(); queue.maxConcurrentOperationCount = 1; queue.qualityOfService = .userInteractive
        let session = URLSession(configuration: configuration, delegate: self, delegateQueue: queue)
        self.session = session; let request = session.dataTask(with: endpoint); self.request = request; request.resume()
    }
    @MainActor func stop() async { lock.withLock { stopped = true }; snapshotBuffer.clear(); request?.cancel(); session?.invalidateAndCancel(); session = nil }
    func snapshot() throws -> RemoteSnapshot { try snapshotBuffer.snapshot() }
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
                    completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        guard let response = response as? HTTPURLResponse, response.statusCode == 200 else {
            completionHandler(.cancel); onFailure(RemoteError.unavailable); return
        }
        if response.mimeType == "image/jpeg", (1...2_097_152).contains(response.expectedContentLength) {
            // Foundation unwraps multipart responses on Apple platforms and
            // calls this delegate again for each JPEG part.
            decodedPartLength = Int(response.expectedContentLength); bytes.removeAll(keepingCapacity: true)
        } else if response.mimeType != "multipart/x-mixed-replace" {
            completionHandler(.cancel); onFailure(RemoteError.invalidMessage); return
        }
        completionHandler(.allow)
    }
    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        guard !lock.withLock({ stopped }) else { return }
        do {
            guard bytes.count + data.count <= 4_194_304 else { throw RemoteError.invalidMessage }
            bytes.append(data)
            if let length = decodedPartLength {
                guard bytes.count <= length else { throw RemoteError.invalidMessage }
                if bytes.count == length { try render(bytes); bytes.removeAll(keepingCapacity: true) }
                return
            }
            var latest: Data?
            while true {
                if imageLength == nil {
                    guard let end = bytes.range(of: Data("\r\n\r\n".utf8)) else {
                        if bytes.count > 8192 { throw RemoteError.invalidMessage }; break
                    }
                    let header = String(decoding: bytes[..<end.lowerBound], as: UTF8.self)
                    guard let line = header.components(separatedBy: "\r\n").first(where: { $0.lowercased().hasPrefix("content-length:") }),
                          let count = Int(line.dropFirst("content-length:".count).trimmingCharacters(in: .whitespaces)),
                          (1...2_097_152).contains(count) else { throw RemoteError.invalidMessage }
                    imageLength = count; bytes.removeSubrange(..<end.upperBound)
                }
                guard let count = imageLength, bytes.count >= count else { break }
                latest = Data(bytes.prefix(count)); bytes.removeFirst(count); imageLength = nil
            }
            if let latest { try render(latest) }
        } catch { dataTask.cancel(); onFailure(error) }
    }
    private func render(_ jpeg: Data) throws {
        guard let imageSource = CGImageSourceCreateWithData(jpeg as CFData, nil),
              let image = CGImageSourceCreateImageAtIndex(imageSource, 0, nil), image.width <= 4096, image.height <= 4096 else { throw RemoteError.invalidMessage }
        if let expectedSize {
            let expectedRatio = expectedSize.width / expectedSize.height
            let actualRatio = Double(image.width) / Double(image.height)
            guard abs(actualRatio / expectedRatio - 1) < 0.02 else { throw RemoteError.geometryChanged }
        }
        var pixelBuffer: CVPixelBuffer?
        guard CVPixelBufferCreate(kCFAllocatorDefault, image.width, image.height, kCVPixelFormatType_32BGRA,
            [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &pixelBuffer) == kCVReturnSuccess,
              let pixelBuffer else { throw RemoteError.unavailable }
        CVPixelBufferLockBaseAddress(pixelBuffer, [])
        defer { CVPixelBufferUnlockBaseAddress(pixelBuffer, []) }
        guard let context = CGContext(data: CVPixelBufferGetBaseAddress(pixelBuffer), width: image.width, height: image.height,
            bitsPerComponent: 8, bytesPerRow: CVPixelBufferGetBytesPerRow(pixelBuffer), space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue) else { throw RemoteError.unavailable }
        context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
        let timestamp = Int64(ProcessInfo.processInfo.systemUptime * 1_000_000_000)
        snapshotBuffer.update(pixelBuffer)
        source.capturer(capturer, didCapture: RTCVideoFrame(buffer: RTCCVPixelBuffer(pixelBuffer: pixelBuffer), rotation: ._0, timeStampNs: timestamp))
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        if !lock.withLock({ stopped }) { onFailure(error ?? RemoteError.closed) }
    }
}

@MainActor final class PhoneInput: RemoteInputInjector {
    let size: CGSize
    var controlAllowed: Bool { failure == nil }
    var onFailure: (Error) -> Void = { _ in }
    private let endpoint: URL
    private let sessionID: String
    private let session: URLSession
    private var start: CGPoint?
    private var queue: [(path: String, body: [String: Any], created: TimeInterval)] = []
    private var pump: Task<Void, Never>?
    private var epoch = UUID()
    private var failure: Error?

    private init(endpoint: URL, sessionID: String, session: URLSession, size: CGSize) {
        self.endpoint = endpoint; self.sessionID = sessionID; self.session = session; self.size = size
    }
    deinit { session.invalidateAndCancel() }
    static func connect(port: Int) async throws -> PhoneInput {
        guard (1024...65535).contains(port) else { throw RemoteError.invalidMessage }
        let endpoint = URL(string: "http://127.0.0.1:\(port)")!
        let config = URLSessionConfiguration.ephemeral; config.timeoutIntervalForRequest = 3
        config.httpShouldSetCookies = false; config.urlCache = nil
        let session = URLSession(configuration: config, delegate: RemoteNoRedirects(), delegateQueue: nil)
        let response = try await call(session, endpoint: endpoint, path: "/session", body: ["capabilities": ["alwaysMatch": ["waitForIdleTimeout": 0, "shouldWaitForQuiescence": false]]])
        guard let id = response["sessionId"] as? String, UUID(uuidString: id) != nil else { throw RemoteError.unavailable }
        let dimensions = try await call(session, endpoint: endpoint, path: "/session/\(id)/window/size", body: nil)
        guard let value = dimensions["value"] as? [String: Any], let width = value["width"] as? Double, let height = value["height"] as? Double,
              width > 0, height > 0, width <= 4096, height <= 4096 else { throw RemoteError.invalidMessage }
        _ = try await call(session, endpoint: endpoint, path: "/session/\(id)/appium/settings", body: ["settings": ["mjpegServerFramerate": 30, "mjpegScalingFactor": 50, "mjpegServerScreenshotQuality": 60, "waitForIdleTimeout": 0]])
        return PhoneInput(endpoint: endpoint, sessionID: id, session: session, size: CGSize(width: width, height: height))
    }
    func apply(_ event: RemoteInput) throws {
        try event.validate(); if let failure { throw failure }
        let point = CGPoint(x: (event.x ?? 0) * (size.width - 1), y: (event.y ?? 0) * (size.height - 1))
        switch event.kind {
        case .button:
            if event.button == 1 {
                if event.down == false { try enqueue("/wda/touchAndHold", ["x": point.x, "y": point.y, "duration": 0.6]) }
                return
            }
            guard event.button == 0 else { return }
            if event.down == true { start = point }
            else if let origin = start {
                start = nil
                if hypot(point.x - origin.x, point.y - origin.y) < 6 { try enqueue("/wda/tap", ["x": point.x, "y": point.y]) }
                else { try drag(from: origin, to: point) }
            }
        case .scroll:
            let target = CGPoint(x: min(size.width - 12, max(12, point.x + (event.deltaX ?? 0))), y: min(size.height - 12, max(12, point.y + (event.deltaY ?? 0))))
            try drag(from: point, to: target)
        case .text: try enqueue("/wda/keys", ["value": [event.text!], "frequency": 120])
        case .key:
            guard event.down == true else { return }
            switch event.key {
            case 40: try enqueue("/wda/keys", ["value": ["\n"]])
            case 42: try enqueue("/wda/keys", ["value": ["\u{8}"]])
            case 74: try enqueue("/wda/homescreen", [:])
            default: break
            }
        case .releaseAll: releaseAll()
        case .move: break // XCTest submits complete gestures when the pointer lifts.
        }
    }
    private func drag(from: CGPoint, to: CGPoint) throws {
        try enqueue("/wda/dragfromtoforduration", ["fromX": from.x, "fromY": from.y, "toX": to.x, "toY": to.y, "duration": 0.05])
    }
    private func enqueue(_ path: String, _ body: [String: Any]) throws {
        // A phone submits text through XCTest. Combine adjacent pending text
        // commits so normal typing does not create one HTTP round trip per key.
        if path == "/wda/keys", let last = queue.last, last.path == path,
           let old = (last.body["value"] as? [String])?.first, let next = (body["value"] as? [String])?.first,
           (old.utf8.count + next.utf8.count) <= 4096 {
            queue[queue.count - 1].body = ["value": [old + next], "frequency": 120]
            return
        }
        guard queue.count < 8 else { releaseAll(); throw RemoteError.unavailable }
        queue.append((path, body, ProcessInfo.processInfo.systemUptime))
        guard pump == nil else { return }
        let attempt = epoch
        pump = Task { [weak self] in
            guard let self else { return }
            defer { if epoch == attempt { pump = nil } }
            do {
                while !Task.isCancelled, epoch == attempt, !queue.isEmpty {
                    let next = queue.removeFirst()
                    guard ProcessInfo.processInfo.systemUptime - next.created < 0.75 else { throw RemoteError.unavailable }
                    // A queued tap must never use coordinates from the previous
                    // orientation. Require the caller to share the new surface.
                    let dimensions = try await Self.call(session, endpoint: endpoint, path: "/session/\(sessionID)/window/size", body: nil)
                    guard let value = dimensions["value"] as? [String: Any],
                          value["width"] as? Double == size.width,
                          value["height"] as? Double == size.height else { throw RemoteError.geometryChanged }
                    guard !Task.isCancelled, epoch == attempt else { return }
                    guard ProcessInfo.processInfo.systemUptime - next.created < 0.75 else { throw RemoteError.unavailable }
                    let path = next.path == "/wda/homescreen" ? next.path : "/session/\(sessionID)" + next.path
                    _ = try await Self.call(session, endpoint: endpoint, path: path, body: next.body)
                }
            } catch { if epoch == attempt { failure = error; releaseAll(); onFailure(error) } }
        }
    }
    func settled() async throws {
        let attempt = epoch
        await pump?.value
        if let failure { throw failure }
        guard attempt == epoch, !Task.isCancelled else { throw RemoteError.busy }
    }
    func releaseAll() { epoch = UUID(); start = nil; queue.removeAll(); pump?.cancel(); pump = nil }
    private static func call(_ session: URLSession, endpoint: URL, path: String, body: [String: Any]?) async throws -> [String: Any] {
        var request = URLRequest(url: URL(string: path, relativeTo: endpoint)!, timeoutInterval: 3)
        if let body { request.httpMethod = "POST"; request.httpBody = try JSONSerialization.data(withJSONObject: body); request.setValue("application/json", forHTTPHeaderField: "Content-Type") }
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse, response.statusCode == 200, data.count <= 131_072,
              let value = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              (value["value"] as? [String: Any])?["error"] == nil else { throw RemoteError.unavailable }
        return value
    }
}
#endif
