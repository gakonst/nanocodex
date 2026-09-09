import ActivityKit
import InboxCore
import UIKit
import UserNotifications
import CryptoKit
import os

/// Native, independently dismissible thread notifications. Updates replace the
/// same request instead of adding an alert for every progress message.
@MainActor
final class AgentNotificationController: NSObject, UNUserNotificationCenterDelegate {
    private struct Desired: Equatable {
        var account: String
        var threads: [AgentThreadNotification]
        var unchecked: Set<String>
        var foreground: Bool
    }
    private static let category = "nanocodex.agent-thread"
    private let center = UNUserNotificationCenter.current()
    private let open: (URL) -> Void
    private var desired: Desired?
    private var task: Task<Void, Never>?
    private var revision = 0
    private var account = ""
    private var ledger = AgentNotificationLedger()
    private var removedLegacyActivity = false
    private let log = Logger(subsystem: "xyz.paradigm.centaur", category: "AgentNotifications")

    init(open: @escaping (URL) -> Void) {
        self.open = open
        super.init()
        center.delegate = self
        center.setNotificationCategories([UNNotificationCategory(identifier: Self.category,
            actions: [], intentIdentifiers: [], options: [.customDismissAction])])
    }

    func update(account: String, threads: [AgentThreadNotification], unchecked: Set<String> = [], foreground: Bool) {
        let next = Desired(account: account, threads: threads, unchecked: unchecked, foreground: foreground)
        guard desired != next else { return }
        desired = next; revision += 1
        guard task == nil else { return }
        task = Task { [weak self] in
            guard let self else { return }
            let background = UIApplication.shared.beginBackgroundTask(withName: "Update agent notifications") { [weak self] in
                Task { @MainActor in self?.task?.cancel() }
            }
            defer {
                if background != .invalid { UIApplication.shared.endBackgroundTask(background) }
                self.task = nil
            }
            while !Task.isCancelled, let next = self.desired {
                let version = self.revision
                await self.apply(next, version: version)
                if version == self.revision { break }
            }
        }
    }

    private func apply(_ next: Desired, version: Int) async {
        if !removedLegacyActivity {
            for activity in Activity<AgentActivityAttributes>.activities { await activity.end(nil, dismissalPolicy: .immediate) }
            removedLegacyActivity = true
        }
        let delivered = await center.deliveredNotifications()
        let pending = await center.pendingNotificationRequests()
        guard version == revision, !Task.isCancelled else { return }
        let own = delivered.map(\.request).filter { $0.content.categoryIdentifier == Self.category }
        let old = (own + pending.filter { $0.content.categoryIdentifier == Self.category }).filter {
            next.account.isEmpty || $0.content.userInfo["account"] as? String != next.account
        }.map(\.identifier)
        center.removeDeliveredNotifications(withIdentifiers: old)
        center.removePendingNotificationRequests(withIdentifiers: old)
        if account != next.account { account = next.account; ledger = Self.load(account) }
        guard !next.account.isEmpty else { return }
        let removed = ledger.reconcile(next.threads, retaining: next.unchecked)
            .map { Self.identifier(account: next.account, agentID: $0) }
        center.removeDeliveredNotifications(withIdentifiers: removed)
        center.removePendingNotificationRequests(withIdentifiers: removed)
        // Honor Clear All and system removals even without a dismissal callback.
        let present = Set((own + pending).map(\.identifier))
        for (id, receipt) in ledger.published where !present.contains(Self.identifier(account: account, agentID: id)) {
            ledger.dismiss(id: id, revision: receipt.revision)
        }
        persist()
        var settings = await center.notificationSettings()
        if settings.authorizationStatus == .notDetermined, next.foreground, next.threads.contains(where: \.isRunning) {
            do { _ = try await center.requestAuthorization(options: [.alert]) }
            catch { log.error("Notification authorization failed: \(error.localizedDescription, privacy: .public)") }
            settings = await center.notificationSettings()
        }
        guard version == revision, !Task.isCancelled,
              [.authorized, .provisional, .ephemeral].contains(settings.authorizationStatus) else { return }
        for thread in next.threads {
            guard version == revision, !Task.isCancelled else { return }
            guard ledger.shouldPublish(thread, foreground: next.foreground) else { continue }
            let content = UNMutableNotificationContent()
            content.title = thread.title; content.subtitle = thread.subtitle; content.body = thread.body
            content.categoryIdentifier = Self.category
            content.threadIdentifier = Self.identifier(account: account, agentID: thread.id)
            content.userInfo = ["account": account, "agent": thread.id, "revision": thread.revision]
            content.sound = nil
            content.interruptionLevel = thread.isRunning ? .passive : .active
            let request = UNNotificationRequest(identifier: content.threadIdentifier, content: content, trigger: nil)
            do {
                try await center.add(request)
                ledger.didPublish(thread); persist()
            } catch { log.error("Could not update agent notification: \(error.localizedDescription, privacy: .public)") }
        }
    }

    private static func identifier(account: String, agentID: String) -> String {
        let data = (try? JSONEncoder().encode([account, agentID])) ?? Data()
        return category + "." + SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }
    private static func load(_ account: String) -> AgentNotificationLedger {
        guard !account.isEmpty, let data = UserDefaults.standard.data(forKey: "inbox.notifications." + account),
              let ledger = try? JSONDecoder().decode(AgentNotificationLedger.self, from: data) else { return .init() }
        return ledger
    }
    private func persist() {
        guard !account.isEmpty, let data = try? JSONEncoder().encode(ledger) else { return }
        UserDefaults.standard.set(data, forKey: "inbox.notifications." + account)
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                                            withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.list])
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                                            withCompletionHandler completionHandler: @escaping () -> Void) {
        let request = response.notification.request
        let info = request.content.userInfo
        let account = info["account"] as? String, agent = info["agent"] as? String, revision = info["revision"] as? String
        let identifier = request.identifier, category = request.content.categoryIdentifier, action = response.actionIdentifier
        Task { @MainActor [weak self] in
            defer { completionHandler() }
            guard let self, category == Self.category, let account, let agent, let revision,
                  identifier == Self.identifier(account: account, agentID: agent) else { return }
            if self.account == account {
                self.ledger.dismiss(id: agent, revision: revision); self.persist()
            } else {
                var ledger = Self.load(account); ledger.dismiss(id: agent, revision: revision)
                if let data = try? JSONEncoder().encode(ledger) { UserDefaults.standard.set(data, forKey: "inbox.notifications." + account) }
            }
            if action == UNNotificationDefaultActionIdentifier { self.open(AgentActivityLink.url(account: account, agentID: agent)) }
        }
    }
}
