import Foundation
import InboxCore

/// Account-authenticated requests; the provider key is saved only by the server.
final class ElevenLabs: @unchecked Sendable {
    private let http: HTTPTransport
    private let key: String
    private let base = "/api/voice/elevenlabs"
    init(configuration: VoiceConfiguration, urlConfiguration: URLSessionConfiguration? = nil) throws {
        http = try HTTPTransport(origin: configuration.baseURL, configuration: urlConfiguration)
        key = configuration.apiKey
    }
    deinit { http.close() }
    func request(_ suffix: String = "", method: String = "GET", body: JSON? = nil) async throws -> JSON {
        try await http.json(http.request(base + suffix, method: method, body: try body.map { try JSONEncoder().encode($0) }, key: key))
    }
    func speech(text: String, voiceID: String) async throws -> Data {
        let body: JSON = .object(["voice_id": .string(voiceID), "text": .string(text), "output_format": .string("mp3_44100_128")])
        return try await http.data(http.request(base + "/speech", method: "POST", body: JSONEncoder().encode(body), key: key)).0
    }
    func clone(name: String, files: [URL], consent: Bool) async throws -> JSON {
        guard VoiceCloneGuidance.canCreate(name: name, count: files.count, consent: consent) else { throw ManagedError(code: "invalid_clone", message: "Provide a name, one to five audio samples, and consent.") }
        let boundary = "Nanocodex-" + UUID().uuidString
        var body = Data()
        func append(_ text: String) { body.append(Data(text.utf8)) }
        append("--\(boundary)\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\n\(name)\r\n")
        append("--\(boundary)\r\nContent-Disposition: form-data; name=\"consent\"\r\n\r\ntrue\r\n")
        let types = ["mp3": "audio/mpeg", "wav": "audio/wav", "m4a": "audio/mp4", "aac": "audio/aac", "ogg": "audio/ogg", "webm": "audio/webm", "flac": "audio/flac"]
        for (index, url) in files.enumerated() {
            let accessed = url.startAccessingSecurityScopedResource()
            defer { if accessed { url.stopAccessingSecurityScopedResource() } }
            guard let type = types[url.pathExtension.lowercased()],
                  let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize, size > 0, size <= 10 * 1024 * 1024 else {
                throw ManagedError(code: "invalid_audio", message: "Use MP3, WAV, M4A, AAC, OGG, WebM or FLAC samples up to 10 MB each.")
            }
            let data = try Data(contentsOf: url)
            append("--\(boundary)\r\nContent-Disposition: form-data; name=\"files\"; filename=\"sample-\(index).\(url.pathExtension.lowercased())\"\r\nContent-Type: \(type)\r\n\r\n")
            body.append(data); append("\r\n")
            guard body.count < 20 * 1024 * 1024 - 1024 else { throw ManagedError(code: "upload_size", message: "Keep all samples together below 20 MB.") }
        }
        append("--\(boundary)--\r\n")
        var request = try http.request(base + "/voices", method: "POST", body: body, key: key)
        request.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
        request.timeoutInterval = 120
        let result: JSON = try await http.json(request, timeout: .seconds(120))
        guard !result["voice_id"].string.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw ManagedError(code: "clone_result_unknown", message: "The server did not return a voice ID. Refresh your voices before trying again; the clone may have been created.")
        }
        return result
    }
}
