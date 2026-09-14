import Foundation
import XCTest
@testable import InboxCore

final class HistoryCacheTests: XCTestCase {
    func testCachesAreBoundedAndSeparatedByCredentialAndOrigin() async throws {
        let first = try AccountCredential(origin: "https://cache.invalid", apiKey: fixtureKey)
        let otherKey = try AccountCredential(origin: first.origin, apiKey: "ncx_live_abcdefgh1234_" + String(repeating: "y", count: 43))
        let otherOrigin = try AccountCredential(origin: "https://other.invalid", apiKey: fixtureKey)
        let cache = ManagedResponseCache.cache(for: first)
        XCTAssertTrue(cache === ManagedResponseCache.cache(for: first))
        XCTAssertFalse(cache === ManagedResponseCache.cache(for: otherKey))
        XCTAssertFalse(cache === ManagedResponseCache.cache(for: otherOrigin))
        XCTAssertEqual(cache.memoryCapacity, 8 * 1024 * 1024)
        XCTAssertEqual(cache.diskCapacity, 128 * 1024 * 1024)
        let client = ManagedClient(credential: first)
        let request = try client.request(path: "/v1/agents/example/events/history?limit=128")
        let response = HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil,
            headerFields: ["Cache-Control": "private, no-cache", "ETag": "\"one\""])!
        cache.storeCachedResponse(CachedURLResponse(response: response, data: Data("private history".utf8)), for: request)
        client.close()
        XCTAssertNotNil(cache.cachedResponse(for: request), "Ending an observer keeps recreatable history")
        let reconnected = ManagedClient(credential: first)
        reconnected.clearCachedResponses()
        let deadline = ContinuousClock.now.advanced(by: .seconds(3))
        while cache.cachedResponse(for: request) != nil, ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(50))
        }
        XCTAssertNil(cache.cachedResponse(for: request), "Explicit sign-out removes cached history")
        reconnected.close()
    }

    func testNativeURLCacheRevalidatesRealDurableHistory() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["NANOCODEX_HISTORY_CACHE_LIVE"] == "1" else {
            throw XCTSkip("Set NANOCODEX_HISTORY_CACHE_LIVE=1 and NC_API_KEY for native URLCache/real DO validation.")
        }
        let credential = try AccountCredential(origin: environment["NANOCODEX_MANAGED_URL"] ?? "https://nanocodex.gakonst.workers.dev",
                                               apiKey: XCTUnwrap(environment["NC_API_KEY"]))
        let client = ManagedClient(credential: credential)
        defer { client.close() }
        let supplied = environment["NANOCODEX_HISTORY_CACHE_AGENT"]
        let id: String
        if let supplied { id = supplied }
        else { id = try await client.create(requestID: "history-cache-" + UUID().uuidString) }
        if supplied == nil {
            addTeardownBlock {
                let cleanup = ManagedClient(credential: credential)
                defer { cleanup.close() }
                _ = try await cleanup.json(path: ManagedClient.agentPath(id), method: "DELETE")
            }
        }
        let request = try client.request(path: ManagedClient.agentPath(id) + "/events/history?limit=128")
        let cache = ManagedResponseCache.cache(for: credential)
        cache.removeCachedResponse(for: request)
        let first = try await client.history(id)
        // Foundation completes disk-cache admission asynchronously.
        let deadline = ContinuousClock.now.advanced(by: .seconds(3))
        while cache.cachedResponse(for: request) == nil, ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(50))
        }
        let cached = try XCTUnwrap(cache.cachedResponse(for: request))
        let original = try XCTUnwrap(cached.response as? HTTPURLResponse)
        XCTAssertEqual(original.value(forHTTPHeaderField: "Cache-Control"), "private, no-cache")
        XCTAssertNotNil(original.value(forHTTPHeaderField: "ETag"))
        let metrics = HistoryCacheMetrics()
        let configuration = URLSessionConfiguration.default
        configuration.urlCache = cache
        configuration.httpCookieStorage = nil
        let session = URLSession(configuration: configuration, delegate: metrics, delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        let (data, response) = try await session.data(for: request)
        XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200, "Foundation combines a 304 with cached body")
        XCTAssertEqual(data, cached.data)
        let values = metrics.snapshot()
        XCTAssertTrue(values.contains { $0.responseStatus == 304 && $0.hasValidator }, "Native HTTP caching must revalidate authorization without downloading history")
        print("HISTORY_NATIVE_CACHE agent=\(id) events=\(first.events.count) cached_bytes=\(cached.data.count) network_body_bytes=\(values.reduce(0) { $0 + $1.bodyBytes }) validated304=\(values.contains { $0.responseStatus == 304 })")
    }
}

private final class HistoryCacheMetrics: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    struct Value { let responseStatus: Int; let hasValidator: Bool; let bodyBytes: Int64 }
    private let lock = NSLock()
    private var values: [Value] = []
    func urlSession(_ session: URLSession, task: URLSessionTask, didFinishCollecting metrics: URLSessionTaskMetrics) {
        let collected = metrics.transactionMetrics.map {
            Value(responseStatus: ($0.response as? HTTPURLResponse)?.statusCode ?? 0,
                  hasValidator: $0.request.value(forHTTPHeaderField: "If-None-Match") != nil,
                  bodyBytes: $0.countOfResponseBodyBytesReceived)
        }
        lock.lock(); values = collected; lock.unlock()
    }
    func snapshot() -> [Value] { lock.lock(); defer { lock.unlock() }; return values }
}
