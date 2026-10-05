#if canImport(AuthenticationServices)
import AuthenticationServices
#if os(iOS)
import UIKit
#elseif os(macOS)
import AppKit
#endif

/// The OS browser carries only Google's authorization request. The fixed return
/// URL carries a one-use proof-bound completion code; the account credential
/// is exchanged privately.
@MainActor
public final class GoogleSignInBrowser: NSObject, ASWebAuthenticationPresentationContextProviding {
    private var session: ASWebAuthenticationSession?
    private var continuation: CheckedContinuation<String?, Error>?
    private var generation = UUID()

    public override init() { super.init() }

    public func authenticate(_ authorization: GoogleAuthorization) async throws -> String? {
        guard continuation == nil else { throw SMSAuthError(code: "in_progress", message: "A sign-in is already in progress.") }
        let generation = UUID()
        self.generation = generation
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<String?, Error>) in
                guard !Task.isCancelled else { continuation.resume(throwing: CancellationError()); return }
                self.continuation = continuation
                let session = ASWebAuthenticationSession(url: authorization.url, callbackURLScheme: "nanocodex") { [weak self] callback, error in
                    Task { @MainActor in
                        guard let self, self.generation == generation else { return }
                        if let error = error as? ASWebAuthenticationSessionError, error.code == .canceledLogin {
                            self.finish(.failure(CancellationError()))
                        } else if error != nil {
                            self.finish(.failure(SMSAuthError(code: "browser_failed", message: "Could not complete Google sign-in. Please try again.")))
                        } else if let result = Self.callbackResult(callback, attemptID: authorization.attemptID) {
                            self.finish(.success(result.completionCode))
                        } else {
                            self.finish(.failure(SMSAuthError.invalidResponse))
                        }
                    }
                }
                session.presentationContextProvider = self
                session.prefersEphemeralWebBrowserSession = true
                self.session = session
                if !session.start() {
                    self.finish(.failure(SMSAuthError(code: "browser_unavailable", message: "Could not open Google sign-in. Please try again.")))
                }
            }
        } onCancel: {
            Task { @MainActor in
                if self.generation == generation { self.cancel() }
            }
        }
    }

    private struct CallbackResult { let completionCode: String? }

    private static func callbackResult(_ url: URL?, attemptID: String) -> CallbackResult? {
        guard let url, let parts = URLComponents(url: url, resolvingAgainstBaseURL: false),
              parts.scheme == "nanocodex", parts.host == "auth", parts.path == "/google",
              parts.user == nil, parts.password == nil, parts.port == nil, parts.fragment == nil,
              let items = parts.queryItems,
              items.filter({ $0.name == "attempt_id" && $0.value == attemptID }).count == 1,
              items.filter({ $0.name == "status" && ["ready", "failed"].contains($0.value ?? "") }).count == 1 else { return nil }
        if items.first(where: { $0.name == "status" })?.value == "failed" {
            return items.count == 2 ? CallbackResult(completionCode: nil) : nil
        }
        guard items.count == 3, let code = items.first(where: { $0.name == "completion_code" })?.value,
              code.range(of: "^[A-Za-z0-9_-]{43}$", options: .regularExpression) != nil else { return nil }
        // This one-use code proves possession of the OS callback. It is never
        // sufficient without the verifier retained privately by GoogleAuth.
        return CallbackResult(completionCode: code)
    }

    public func cancel() {
        session?.cancel()
        finish(.failure(CancellationError()))
    }

    private func finish(_ result: Result<String?, Error>) {
        let pending = continuation
        continuation = nil; session = nil
        pending?.resume(with: result)
    }

    public func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        #if os(iOS)
        return UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .filter { $0.activationState == .foregroundActive }
            .flatMap(\.windows).first(where: \.isKeyWindow) ?? UIWindow()
        #else
        return NSApplication.shared.keyWindow ?? NSApplication.shared.mainWindow ?? NSWindow()
        #endif
    }
}
#endif
