import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

private let fixturesLock = NSLock()
private var fixtures: [String: HTTPFixture] = [:]
let fixtureKey = "ncx_live_abcdefgh1234_" + String(repeating: "x", count: 43)

struct FixtureRequest {
    let method: String
    let path: String
    let query: String?
    let headers: [String: String]
    let body: Data
    var json: [String: Any] { (try? JSONSerialization.jsonObject(with: body)) as? [String: Any] ?? [:] }
}
struct FixtureReply {
    var status = 200
    var headers = ["Content-Type": "application/json"]
    var body = "{}"
    var delay: Double = 0
}

/// Each test gets an isolated HTTPS origin and URLSession protocol.
final class HTTPFixture: @unchecked Sendable {
    let origin = "https://" + UUID().uuidString.lowercased() + ".invalid"
    let configuration = URLSessionConfiguration.ephemeral
    let queue = DispatchQueue(label: "inbox.voice.fixture")
    let handler: (FixtureRequest) -> FixtureReply
    init(_ handler: @escaping (FixtureRequest) -> FixtureReply) throws {
        self.handler = handler
        configuration.protocolClasses = [VoiceFixtureProtocol.self]
        fixturesLock.lock(); defer { fixturesLock.unlock() }
        fixtures[URL(string: origin)!.host!] = self
    }
    func close() {
        fixturesLock.lock(); defer { fixturesLock.unlock() }
        fixtures.removeValue(forKey: URL(string: origin)!.host!)
    }
}

private final class VoiceFixtureProtocol: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        fixturesLock.lock()
        let fixture = fixtures[request.url!.host!]
        fixturesLock.unlock()
        guard let fixture else { client?.urlProtocol(self, didFailWithError: URLError(.cannotConnectToHost)); return }
        fixture.queue.async {
            var body = self.request.httpBody ?? Data()
            if let stream = self.request.httpBodyStream {
                stream.open(); defer { stream.close() }
                var buffer = [UInt8](repeating: 0, count: 4096)
                while stream.hasBytesAvailable {
                    let count = stream.read(&buffer, maxLength: buffer.count)
                    if count <= 0 { break }
                    body.append(contentsOf: buffer.prefix(count))
                }
            }
            let request = FixtureRequest(method: self.request.httpMethod ?? "GET", path: self.request.url!.path, query: self.request.url!.query,
                headers: Dictionary(uniqueKeysWithValues: (self.request.allHTTPHeaderFields ?? [:]).map { ($0.key.lowercased(), $0.value) }), body: body)
            let reply = fixture.handler(request)
            fixture.queue.asyncAfter(deadline: .now() + reply.delay) {
                let response = HTTPURLResponse(url: self.request.url!, statusCode: reply.status, httpVersion: "HTTP/1.1", headerFields: reply.headers)!
                self.client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                self.client?.urlProtocol(self, didLoad: Data(reply.body.utf8))
                self.client?.urlProtocolDidFinishLoading(self)
            }
        }
    }
    override func stopLoading() {}
}
