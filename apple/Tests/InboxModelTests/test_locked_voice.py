#!/usr/bin/env python3
"""Compile and exercise the actual awaited cloud submission method with gated I/O."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
model = (root / 'NanocodexInbox/InboxModel.swift').read_text()
start = model.index('    func submitLockedVoice(')
end = model.index('\n    func start() async', start)
method = model[start:end]
recovery = model[model.index('    func retainLockedVoiceRecovery('):model.index('    private func restoreLockedVoiceRecovery()')]
ownership = model[model.index('    private func lockedVoiceOwnedTarget('):model.index('    func openLockedVoiceRecovery()')].replace('private func', 'func')
source = r'''
import Foundation
struct JSON {
    var fields: [String: String] = [:]
    var string: String = ""
    subscript(_ key: String) -> JSON { JSON(string: fields[key] ?? "") }
}
enum APIError: Error { case invalidCredential, invalidResponse }
enum QuickVoiceInput {
    static func finalText(_ text: String) -> String? { text.isEmpty ? nil : text }
}
struct Command { let id: String; let agent: String }
struct PendingMessage {
    enum Phase { case submitting, queued, failed, cancelling }
    var agentID: String, input: String, predecessor: String, id: String
    var phase: Phase = .submitting
    var remoteAdmission: Bool? = nil
    var error: String? = nil
    var submission: Command { Command(id: id, agent: agentID) }
    mutating func acknowledge(_ receipt: JSON) throws { phase = .queued }
}
@MainActor final class Preferences {
    var flushes = 0
    func flush() async { flushes += 1 }
}
@MainActor final class Client {
    var commands: [Command] = []
    var gate: CheckedContinuation<JSON, Error>?
    func command(_ command: Command) async throws -> JSON {
        commands.append(command)
        return try await withCheckedThrowingContinuation { gate = $0 }
    }
    func succeed(id: String) { gate?.resume(returning: JSON(fields: ["turn_id": id, "state": "accepted"])); gate = nil }
    func fail() { gate?.resume(throwing: APIError.invalidResponse); gate = nil }
}
@MainActor final class Model {
    struct Card { let id: String }
    var connected = true, isDemo = false, scope = "fixture-account-" + UUID().uuidString, generation = UUID()
    var client: Client? = Client()
    var pending: [PendingMessage] = [], cards: [Card] = []
    var pendingCreations = Set<String>(), busy = Set<String>()
    var drafts: [String: String] = [:], bound: [String: String] = [:]
    var snapshots: [[PendingMessage]] = []
    var recovered = false
    func restoreLockedVoiceRecovery() { recovered = true }
    let preferences = Preferences()
    var creating: CheckedContinuation<Void, Never>?
    var holdCreation = false
    func resolvedAgentID(_ id: String) -> String { bound[id] ?? id }
    func newConversationCard(_ id: String) -> Card { Card(id: id) }
    func persist() { snapshots.append(pending) }
    func readyAgent(_ id: String) async throws -> String {
        if holdCreation { await withCheckedContinuation { creating = $0 } }
        bound[id] = "remote-agent"
        for index in pending.indices where pending[index].agentID == id { pending[index].agentID = "remote-agent" }
        return "remote-agent"
    }
'''
source += method + recovery + ownership
source += r'''
}
@main struct Tests {
    @MainActor static func waitFor(_ predicate: () -> Bool) async {
        for _ in 0..<10000 { if predicate() { return }; await Task.yield() }
        preconditionFailure("async gate was never reached")
    }
    @MainActor static func main() async throws {
        let model = Model(), id = UUID().uuidString
        var returned = false
        let task = Task {
            try await model.submitLockedVoice("Στείλε εργασία", captureID: id, accountScope: model.scope, generation: model.generation)
            returned = true
        }
        await waitFor { model.client!.gate != nil }
        precondition(!returned && model.pending.count == 1)
        precondition(model.snapshots.first?.first?.id == id && model.preferences.flushes >= 2)
        precondition(model.client!.commands.first?.id == id && model.client!.commands.first?.agent == "remote-agent")
        model.client!.succeed(id: id)
        try await task.value
        precondition(returned && model.pending[0].phase == .queued)
        try await model.submitLockedVoice("Στείλε εργασία", captureID: id, accountScope: model.scope, generation: model.generation)
        precondition(model.client!.commands.count == 1)

        model.pending.removeAll() // Foreground reconciliation already observed the admitted turn.
        do { try await model.submitLockedVoice("Στείλε εργασία", captureID: id, accountScope: model.scope, generation: model.generation); preconditionFailure("reconciled capture resubmitted") } catch {}
        precondition(model.client!.commands.count == 1 && model.lockedVoiceOwnedTarget(id, accountScope: model.scope) != nil)
        model.retainLockedVoiceRecovery("Στείλε εργασία", captureID: id, accountScope: model.scope)
        precondition(!model.recovered && UserDefaults.standard.dictionary(forKey: "inbox.lockedVoiceRecovery." + model.scope) == nil)

        let failed = Model(), failedID = UUID().uuidString
        let failing = Task { try await failed.submitLockedVoice("retained", captureID: failedID, accountScope: failed.scope, generation: failed.generation) }
        await waitFor { failed.client!.gate != nil }
        failed.client!.fail()
        do { try await failing.value; preconditionFailure("failure was swallowed") } catch {}
        precondition(failed.pending[0].phase == .failed && failed.pending[0].id == failedID && failed.pending[0].input == "retained")
        do { try await failed.submitLockedVoice("retained", captureID: failedID, accountScope: failed.scope, generation: failed.generation); preconditionFailure("ambiguous retry") } catch {}
        precondition(failed.client!.commands.count == 1)

        let cancelled = Model(); cancelled.holdCreation = true
        let cancelling = Task { try await cancelled.submitLockedVoice("cancel", captureID: UUID().uuidString, accountScope: cancelled.scope, generation: cancelled.generation) }
        await waitFor { cancelled.creating != nil }
        cancelling.cancel(); cancelled.creating?.resume()
        do { try await cancelling.value; preconditionFailure("cancelled submission succeeded") } catch {}
        precondition(cancelled.client!.commands.isEmpty && cancelled.pending[0].phase == .failed)

        let changed = Model(); changed.holdCreation = true
        let changing = Task { try await changed.submitLockedVoice("old account", captureID: UUID().uuidString, accountScope: changed.scope, generation: changed.generation) }
        await waitFor { changed.creating != nil }
        changed.generation = UUID(); changed.creating?.resume()
        do { try await changing.value; preconditionFailure("account fence failed") } catch {}
        precondition(changed.client!.commands.isEmpty)

        let wrong = Model(), wrongID = UUID().uuidString
        let mismatch = Task { try await wrong.submitLockedVoice("request", captureID: wrongID, accountScope: wrong.scope, generation: wrong.generation) }
        await waitFor { wrong.client!.gate != nil }
        wrong.client!.succeed(id: UUID().uuidString)
        do { try await mismatch.value; preconditionFailure("mismatched receipt accepted") } catch {}
        precondition(wrong.pending[0].phase == .failed)
        for item in [model, failed, cancelled, changed, wrong] {
            for prefix in ["inbox.lockedVoiceRecovery.", "inbox.lockedVoiceOwnership.", "inbox.lockedVoiceLastTarget."] {
                UserDefaults.standard.removeObject(forKey: prefix + item.scope)
            }
        }
        print("PASS: recovery after reconciliation never duplicates; awaited cloud admission, durable stable ID, retained failure, cancellation, account fence, receipt identity")
    }
}
'''
with tempfile.TemporaryDirectory(prefix='locked-voice-test-') as directory:
    path = Path(directory)
    (path / 'Tests.swift').write_text(source)
    subprocess.run(['swiftc', '-parse-as-library', str(path / 'Tests.swift'), '-o', str(path / 'tests')], check=True)
    subprocess.run([str(path / 'tests')], check=True)
