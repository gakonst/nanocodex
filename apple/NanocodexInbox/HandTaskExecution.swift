import Foundation
import InboxCore
#if os(iOS)
import BackgroundTasks
import UIKit
#endif

/// Runtime belongs to a particular user request, never an idle Hand socket.
@MainActor
final class HandTaskExecution {
    nonisolated static let identifierPrefix = "xyz.paradigm.centaur.hand.turn."
    private final class Run {
        let identifier = HandTaskExecution.identifierPrefix + UUID().uuidString
        let progress: Progress
        let cancel: () -> Void
        var activity: HandTaskProgress
        var lastUpdate = Date.distantPast
        var work: Task<String, Error>?
        var hasRuntime: Bool
        var update: (() -> Void)?
        var finish: ((Bool) -> Void)?
        init(id: String, progress: Progress, hasRuntime: Bool, cancel: @escaping () -> Void) {
            self.progress = progress; self.hasRuntime = hasRuntime; self.cancel = cancel
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
               runtimeProvided: Bool = false, cancel: @escaping () -> Void,
               operation: @escaping (Progress) async throws -> String) -> Task<String, Error> {
        if let existing = runs[id]?.work { return existing }
        let run = Run(id: id, progress: progress, hasRuntime: runtimeProvided, cancel: cancel)
        runs[id] = run
        progress.localizedDescription = "Nanocodex task"
        progress.localizedAdditionalDescription = "Submitting request"
        #if os(iOS)
        if !runtimeProvided, #available(iOS 26.0, *), UIApplication.shared.applicationState == .active {
            requestRuntime(id: id, title: title, run: run)
        }
        #endif
        let work = Task {
            var success = false
            defer {
                run.finish?(success)
                run.finish = nil; run.update = nil
                #if os(iOS)
                if #available(iOS 26.0, *) {
                    BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: run.identifier)
                }
                #endif
                if runs[id] === run { runs[id] = nil; changed() }
            }
            try Task.checkCancellation()
            let result = try await operation(progress)
            try Task.checkCancellation()
            progress.completedUnitCount = progress.totalUnitCount
            run.update?()
            success = true
            return result
        }
        run.work = work
        changed()
        return work
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

    func cancel(id: String, stopTurn: Bool = true) {
        guard let run = runs.removeValue(forKey: id) else { return }
        // Persist/send the exact turn's Stop while the background grant still exists.
        if stopTurn { run.cancel() }
        run.work?.cancel()
        run.finish?(false); run.finish = nil; run.update = nil
        #if os(iOS)
        if #available(iOS 26.0, *) {
            BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: run.identifier)
        }
        #endif
        changed()
    }

    func cancelAll(stopTurns: Bool) {
        for id in Array(runs.keys) { cancel(id: id, stopTurn: stopTurns) }
    }

    func suspendWithoutRuntime() {
        for id in runs.keys.filter({ runs[$0]?.hasRuntime == false }) { cancel(id: id, stopTurn: false) }
    }

    #if os(iOS)
    @available(iOS 26.0, *)
    private func requestRuntime(id: String, title: String, run: Run) {
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
                    task.updateTitle("Nanocodex task", subtitle: run.progress.localizedAdditionalDescription ?? "Working")
                }
                run.finish = { task.setTaskCompleted(success: $0) }
                task.expirationHandler = { [weak self] in
                    Task { @MainActor in self?.cancel(id: id) }
                }
                run.update?(); self.changed()
            }
        }
        guard registered else {
            failed("Background execution is unavailable. Keep Nanocodex open for this device's tools.")
            return
        }
        let request = BGContinuedProcessingTaskRequest(identifier: identifier, title: "Nanocodex task", subtitle: String(title.prefix(80)))
        request.strategy = .fail
        do { try BGTaskScheduler.shared.submit(request) }
        catch { failed("iOS couldn't grant background time. Keep Nanocodex open for this device's tools.") }
    }
    #endif
}
