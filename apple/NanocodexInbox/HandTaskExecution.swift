import Foundation
import InboxCore
#if os(iOS)
import BackgroundTasks
import UIKit
#endif

/// Owns local observation and device runtime, never the remote turn lifecycle.
@MainActor
final class HandTaskExecution {
    nonisolated static let identifierPrefix = "xyz.paradigm.centaur.hand.turn."
    private final class Run {
        let identifier = HandTaskExecution.identifierPrefix + UUID().uuidString
        var title: String
        let progress: Progress
        var activity: HandTaskProgress
        var lastUpdate = Date.distantPast
        var work: Task<String, Error>?
        var hasRuntime: Bool
        var update: (() -> Void)?
        var finish: ((Bool) -> Void)?
        init(id: String, title: String, progress: Progress, hasRuntime: Bool) {
            self.title = title.isEmpty ? "Nanocodex task" : String(title.prefix(80))
            self.progress = progress; self.hasRuntime = hasRuntime
            activity = HandTaskProgress(turnID: id)
        }
    }
    private var runs: [String: Run] = [:]
    private let changed: () -> Void
    private let failed: (String) -> Void
    var hasBackgroundRuntime: Bool { runs.values.contains { $0.hasRuntime } }

    init(changed: @escaping () -> Void, failed: @escaping (String) -> Void) {
        self.changed = changed; self.failed = failed
    }

    @discardableResult
    func start(id: String, title: String, progress: Progress = Progress(totalUnitCount: 1),
               runtimeProvided: Bool = false,
               operation: @escaping (Progress) async throws -> String) -> Task<String, Error> {
        if let existing = runs[id]?.work { return existing }
        let run = Run(id: id, title: title, progress: progress, hasRuntime: runtimeProvided)
        runs[id] = run
        progress.localizedDescription = run.title
        progress.localizedAdditionalDescription = "Submitting request"
        #if os(iOS)
        if !runtimeProvided, #available(iOS 26.0, *), UIApplication.shared.applicationState == .active {
            requestRuntime(id: id, run: run)
        }
        #endif
        let work = Task {
            var success = false
            defer {
                progress.localizedAdditionalDescription = run.activity.detail
                run.update?()
                run.finish?(success)
                run.finish = nil; run.update = nil
                #if os(iOS)
                if #available(iOS 26.0, *) {
                    BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: run.identifier)
                }
                #endif
                if runs[id] === run { runs[id] = nil; changed() }
            }
            do {
                try Task.checkCancellation()
                let result = try await operation(progress)
                try Task.checkCancellation()
                run.activity.finish(.completed)
                progress.completedUnitCount = progress.totalUnitCount
                success = true
                return result
            } catch HandTaskError.cancelled {
                run.activity.finish(.stopped)
                throw HandTaskError.cancelled
            } catch {
                run.activity.finish(error is CancellationError ? .paused : .failed)
                throw error
            }
        }
        run.work = work
        changed()
        return work
    }

    func updateTitle(id: String, title: String) {
        guard let run = runs[id], !title.isEmpty else { return }
        let title = String(title.prefix(80))
        guard run.title != title else { return }
        run.title = title; run.progress.localizedDescription = title
        run.update?()
    }

    func cursor(id: String) -> Cursor { runs[id]?.activity.cursor ?? .zero }
    func beginObservation(id: String, after cursor: Cursor) {
        runs[id]?.activity = HandTaskProgress(turnID: id, after: cursor)
    }

    func receive(_ event: AgentEvent, id: String) {
        guard let run = runs[id], run.activity.receive(event) else { return }
        // The amount of agent work is not known in advance. Count observed
        // activity plus the outstanding final result, never elapsed seconds.
        run.progress.totalUnitCount = run.activity.completedUnits + 1
        run.progress.completedUnitCount = run.activity.completedUnits
        run.progress.localizedAdditionalDescription = run.activity.detail
        if Date().timeIntervalSince(run.lastUpdate) >= 1 {
            run.lastUpdate = Date(); run.update?()
        }
    }

    func endObservation(id: String, outcome: HandTaskOutcome = .paused) {
        guard let run = runs.removeValue(forKey: id) else { return }
        run.activity.finish(outcome)
        run.progress.localizedAdditionalDescription = run.activity.detail
        run.update?()
        run.work?.cancel()
        run.finish?(false); run.finish = nil; run.update = nil
        #if os(iOS)
        if #available(iOS 26.0, *) {
            BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: run.identifier)
        }
        #endif
        changed()
    }

    func endAllObservations() {
        for id in Array(runs.keys) { endObservation(id: id) }
    }

    func suspendWithoutRuntime() {
        for id in runs.keys.filter({ runs[$0]?.hasRuntime == false }) { endObservation(id: id) }
    }

    #if os(iOS)
    @available(iOS 26.0, *)
    private func requestRuntime(id: String, run: Run) {
        let identifier = run.identifier
        // Each handler is tied to one durable turn. A late grant can never
        // resurrect a completed/cancelled turn or follow an account switch.
        let registered = BGTaskScheduler.shared.register(forTaskWithIdentifier: identifier, using: .main) { [weak self, weak run] task in
            MainActor.assumeIsolated {
                guard let self, let run, self.runs[id] === run,
                      let task = task as? BGContinuedProcessingTask else {
                    task.setTaskCompleted(success: false); return
                }
                run.hasRuntime = true
                run.update = {
                    task.progress.totalUnitCount = run.progress.totalUnitCount
                    task.progress.completedUnitCount = run.progress.completedUnitCount
                    task.updateTitle(run.title, subtitle: run.progress.localizedAdditionalDescription ?? "Working")
                }
                run.finish = { task.setTaskCompleted(success: $0) }
                task.expirationHandler = { [weak self, weak run] in
                    Task { @MainActor in
                        guard let self, let run, self.runs[id] === run else { return }
                        // Expiration does not establish a user intent to stop cloud work.
                        self.endObservation(id: id)
                    }
                }
                run.update?(); self.changed()
            }
        }
        guard registered else {
            failed("Background execution is unavailable. Keep Nanocodex open for this device's tools.")
            return
        }
        let request = BGContinuedProcessingTaskRequest(identifier: identifier, title: run.title, subtitle: run.progress.localizedAdditionalDescription ?? "Submitting request")
        request.strategy = .fail
        do { try BGTaskScheduler.shared.submit(request) }
        catch { failed("iOS couldn't grant background time. Keep Nanocodex open for this device's tools.") }
    }
    #endif
}

enum HandTaskError: LocalizedError {
    case signIn, disabled, accountChanged, emptyRequest, cancelled, delivery(String)
    var errorDescription: String? {
        switch self {
        case .signIn: "Open Nanocodex and sign in before running this shortcut."
        case .disabled: "This device's Hand is disabled. Enable it in Nanocodex Settings to run this task."
        case .accountChanged: "This shortcut's agent belongs to a different account. Choose an agent from the connected account."
        case .emptyRequest: "Enter a request for the agent."
        case .cancelled: "The remote turn was stopped."
        case .delivery(let message): message
        }
    }
}
