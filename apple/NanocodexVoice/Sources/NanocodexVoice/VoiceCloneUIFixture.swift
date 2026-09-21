#if DEBUG && os(iOS) && targetEnvironment(simulator)
import SwiftUI
import Foundation

/// Hosts the production form; only account/provider HTTP is intercepted locally.
public struct VoiceCloneUIFixture: View {
    @StateObject private var session = VoiceSession()
    @State private var presented = false
    public init() {}
    public var body: some View {
        NavigationStack {
            Form {
                Button("ElevenLabs") {
                    session.settings = VoiceSettings(outputProvider: .elevenlabs)
                    presented = true
                }
                if !presented, session.settings.outputProvider == .elevenlabs, session.settings.elevenLabsVoiceId == "fixture-clone" {
                    Text("Fixture voice saved").accessibilityIdentifier("fixture-voice-saved")
                }
            }.navigationTitle("Voice settings")
        }
        .sheet(isPresented: $presented) {
            VoiceSettingsView(session: session, onStart: {
                VoiceConfiguration(baseURL: URL(string: "https://voice-clone-fixture.invalid")!, apiKey: "synthetic-fixture", agentID: "00000000-0000-0000-0000-000000000003")
            }, urlConfiguration: Self.network)
        }
    }
    private static var network: URLSessionConfiguration {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [VoiceCloneFixtureProtocol.self]
        return configuration
    }
}

private final class VoiceCloneFixtureProtocol: URLProtocol, @unchecked Sendable {
    private static let lock = NSLock()
    private static var created = false
    private var reply: DispatchWorkItem?
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        guard request.url?.host == "voice-clone-fixture.invalid" else {
            client?.urlProtocol(self, didFailWithError: URLError(.unsupportedURL)); return
        }
        let body: String
        var status = 200
        switch (request.httpMethod ?? "GET", request.url!.path) {
        case ("GET", "/api/voice/elevenlabs"):
            body = #"{"configured":true}"#
        case ("GET", "/api/voice/elevenlabs/voices"):
            let created = Self.lock.withLock { Self.created }
            if created && ProcessInfo.processInfo.arguments.contains("--clone-refresh-fails") {
                status = 503; body = #"{"error":"fixture_unavailable"}"#
            } else {
                body = created ? #"{"voices":[{"voice_id":"fixture-clone","name":"Fixture clone","category":"cloned"}],"has_more":false}"# : #"{"voices":[],"has_more":false}"#
            }
        case ("POST", "/api/voice/elevenlabs/voices"):
            Self.lock.withLock { Self.created = true }
            body = #"{"voice_id":"fixture-clone","requires_verification":false}"#
        default:
            client?.urlProtocol(self, didFailWithError: URLError(.unsupportedURL)); return
        }
        let reply = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.client?.urlProtocol(self, didReceive: HTTPURLResponse(url: self.request.url!, statusCode: status, httpVersion: nil, headerFields: ["Content-Type": "application/json"])!, cacheStoragePolicy: .notAllowed)
            self.client?.urlProtocol(self, didLoad: Data(body.utf8))
            self.client?.urlProtocolDidFinishLoading(self)
        }
        self.reply = reply
        let delay: Double = request.httpMethod == "POST" && ProcessInfo.processInfo.arguments.contains("--clone-upload-slow") ? 60 : 0
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: reply)
    }
    override func stopLoading() { reply?.cancel(); reply = nil }
}
#endif
