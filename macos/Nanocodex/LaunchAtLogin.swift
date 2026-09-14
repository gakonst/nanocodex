import Combine
import Foundation
import Security
import ServiceManagement

@MainActor
protocol LoginItemService {
    var status: SMAppService.Status { get }
    func register() throws
    func unregister() throws
    func openLoginItems()
}

@MainActor
private struct MainAppLoginItem: LoginItemService {
    var status: SMAppService.Status { SMAppService.mainApp.status }
    func register() throws { try SMAppService.mainApp.register() }
    func unregister() throws { try SMAppService.mainApp.unregister() }
    func openLoginItems() { SMAppService.openSystemSettingsLoginItems() }
}

/// App-owned preference and OS state; window lifetime never changes registration.
@MainActor
final class LaunchAtLogin: ObservableObject {
    static let preferenceKey = "launchAtLoginEnabled"
    @Published private(set) var status: SMAppService.Status = .notRegistered
    @Published private(set) var error: String?
    private let service: (any LoginItemService)?
    private let preferences: UserDefaults?
    private var started = false

    var isAvailable: Bool { service != nil }
    var isEnabled: Bool { isAvailable && status == .enabled }
    var requiresApproval: Bool { isAvailable && status == .requiresApproval }
    var statusDescription: String {
        guard isAvailable else { return "Use the installed Nanocodex app in Applications to enable launch at login." }
        switch status {
        case .enabled: return "Nanocodex opens when you log in to this Mac."
        case .notRegistered: return "Nanocodex won’t open automatically at login."
        case .requiresApproval: return "Approve Nanocodex in Login Items to enable automatic launch."
        case .notFound: return "macOS couldn’t find this app’s login item."
        @unknown default: return "macOS couldn’t determine this app’s login status."
        }
    }

    init(service: (any LoginItemService)?, preferences: UserDefaults?) {
        self.service = service
        self.preferences = preferences
        refresh()
    }

    static func installed(isolatedSession: Bool, preferences: UserDefaults?) -> LaunchAtLogin {
        let bundle = Bundle.main
        let environment = ProcessInfo.processInfo.environment
        let eligibleLocation = allowsRegistration(bundleURL: bundle.bundleURL,
            bundleIdentifier: bundle.bundleIdentifier, isolatedSession: isolatedSession,
            environment: environment, signed: true)
        return LaunchAtLogin(service: eligibleLocation && hasValidIdentitySignature() ? MainAppLoginItem() : nil,
            preferences: preferences)
    }

    static func allowsRegistration(bundleURL: URL, bundleIdentifier: String?, isolatedSession: Bool,
                                   environment: [String: String], signed: Bool) -> Bool {
        guard !isolatedSession, signed, bundleIdentifier == "xyz.paradigm.nanocodex.macos",
              !["NANOCODEX_DESKTOP_DATA", "XCTestConfigurationFilePath", "XCTestBundlePath", "XCODE_RUNNING_FOR_PREVIEWS"]
                .contains(where: { environment[$0] != nil }) else { return false }
        let location = bundleURL.resolvingSymlinksInPath().standardizedFileURL
        return location.pathExtension == "app" && location.deletingLastPathComponent().path == "/Applications"
    }

    private static func hasValidIdentitySignature() -> Bool {
        var code: SecCode?
        guard SecCodeCopySelf([], &code) == errSecSuccess, let code,
              SecCodeCheckValidity(code, [], nil) == errSecSuccess else { return false }
        var staticCode: SecStaticCode?
        guard SecCodeCopyStaticCode(code, [], &staticCode) == errSecSuccess, let staticCode else { return false }
        var information: CFDictionary?
        guard SecCodeCopySigningInformation(staticCode, SecCSFlags(rawValue: kSecCSSigningInformation), &information) == errSecSuccess,
              let information = information as? [String: Any],
              let team = information[kSecCodeInfoTeamIdentifier as String] as? String else { return false }
        return !team.isEmpty
    }

    func start() {
        guard !started else { return }; started = true
        refresh()
        guard isAvailable, let preferences, preferences.object(forKey: Self.preferenceKey) == nil else { return }
        // A persisted choice also records our one default attempt. Do not undo
        // a later OS opt-out or repeatedly request registration after a failure.
        // A newly installed main app can report notFound until its first
        // registration; the signed installation gate has already been checked.
        if status == .notRegistered || status == .notFound { setEnabled(true) }
        else { preferences.set(status == .enabled, forKey: Self.preferenceKey) }
    }

    func refresh() { if let service { status = service.status } }

    func setEnabled(_ enabled: Bool) {
        guard let service else { return }
        preferences?.set(enabled, forKey: Self.preferenceKey)
        error = nil
        refresh()
        do {
            if enabled {
                if status == .requiresApproval { service.openLoginItems() }
                else if status != .enabled { try service.register() }
            } else if status != .notRegistered {
                try service.unregister()
            }
        } catch { self.error = error.localizedDescription }
        refresh()
    }

    func openLoginItems() { service?.openLoginItems() }
}
