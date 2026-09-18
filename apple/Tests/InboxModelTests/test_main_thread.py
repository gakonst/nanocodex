#!/usr/bin/env python3
"""Execute production navigation methods against manually resumed Swift transports."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
model = (root / 'NanocodexInbox/InboxModel.swift').read_text()

def method(signature):
    start = model.index(signature)
    brace = model.index('{', start)
    depth = 1
    end = brace + 1
    while depth:
        depth += (model[end] == '{') - (model[end] == '}')
        end += 1
    return model[start:end] + '\n'

reset = method('    private func reset()')
assert 'resetProjectNavigation()' in reset, 'Account reset must clear navigation state'
assert 'generation = UUID()' in reset, 'Account reset must invalidate pending navigation requests'

source = r'''
import Foundation
struct AgentCard { let id: String; var title = "" }
struct InboxProject: Equatable { let id: String; let name: String; let primaryAgentID: String }
struct Failure: LocalizedError { var errorDescription: String? { "old account failure" } }
@MainActor final class Preferences { func flush() async {} }
@MainActor final class Client {
    var mains: [(Bool, CheckedContinuation<String?, Error>)] = []
    var projects: [CheckedContinuation<[InboxProject], Error>] = []
    var writes: [CheckedContinuation<InboxProject, Error>] = []
    var ensureCount = 0
    func mainThread(ensure: Bool) async throws -> String? {
        if ensure { ensureCount += 1 }
        return try await withCheckedThrowingContinuation { mains.append((ensure, $0)) }
    }
    func canonicalProjects() async throws -> [InboxProject] {
        try await withCheckedThrowingContinuation { projects.append($0) }
    }
    func registerProject(id: String, name: String, coordinatorAgentID: String?) async throws -> InboxProject {
        try await withCheckedThrowingContinuation { writes.append($0) }
    }
    func main(_ ensure: Bool, _ result: Result<String?, Error>) {
        let index = mains.firstIndex { $0.0 == ensure }!
        mains.remove(at: index).1.resume(with: result)
    }
}
@MainActor final class Model {
    var connected = true, isDemo = false, openingMain = false, savingProject = false
    var mainThreadID: String?, pendingProjectID: String?, pendingProjectName: String?, error: String?
    var generation = UUID(), navigationRevision = UUID()
    var cards: [AgentCard] = [], canonicalProjects: [InboxProject] = []
    var savedProjects: [InboxProject] = []
    var unlistedAgents = Set<String>()
    var focused: AgentCard?
    var client: Client? = Client()
    let preferences = Preferences()
    var persisted = 0
    var jobs: [_Concurrency.Task<Void, Never>] = []
    // Capture production fire-and-forget tasks so tests can await completion, including defer.
    func Task(@_implicitSelfCapture _ operation: @escaping @MainActor () async -> Void) {
        jobs.append(_Concurrency.Task { await operation() })
    }
    func newConversationCard(_ id: String) -> AgentCard { AgentCard(id: id) }
    func select(_ id: String) { focused = AgentCard(id: id) }
    func persistPendingProject() { persisted += 1 }
    func refresh() async { await refreshNavigation() }
    func save() {
        pendingProjectID = "pending"; pendingProjectName = "Project"
        saveCanonicalProject(id: "pending", name: "Project", coordinator: nil, selectOnSuccess: true)
    }
    func resetAccount() {
        generation = UUID()
        resetProjectNavigation()
        cards = []; unlistedAgents = []; focused = nil; error = nil
        client = Client()
    }
'''
for signature in ['    private func admitNavigationAgent(', '    func openMainThread()',
                  '    private func refreshNavigation()', '    private func saveCanonicalProject(',
                  '    private func resetProjectNavigation()', '    func projectRenameIsLocal(']:
    source += method(signature)
source += r'''
}
@main struct Tests {
    @MainActor static func until(_ ready: () -> Bool) async {
        for _ in 0..<10000 {
            if ready() { return }
            await _Concurrency.Task.yield()
        }
        fatalError("Transport did not reach expected suspension")
    }
    @MainActor static func main() async {
        let project = InboxProject(id: "p", name: "Project", primaryAgentID: "root")
        let aliases = Model()
        aliases.canonicalProjects = [project]
        precondition(!aliases.projectRenameIsLocal(project.id), "Canonical-only rename is shared")
        aliases.savedProjects = [project]
        precondition(aliases.projectRenameIsLocal(project.id), "Registering an existing saved project preserves local rename semantics")
        aliases.savedProjects = [InboxProject(id: "local-alias", name: "My name", primaryAgentID: "root")]
        precondition(aliases.projectRenameIsLocal("local-alias"), "Local alias of a canonical root stays local")
        precondition(!aliases.projectRenameIsLocal(project.id))
        precondition(aliases.projectRenameIsLocal("unregistered"))
        let m = Model(), c: Client
        c = m.client!
        m.openMainThread(); m.openMainThread()
        await until { c.mains.count == 1 }
        precondition(c.ensureCount == 1 && m.jobs.count == 1)
        c.main(true, .success("main")); await m.jobs[0].value
        m.openMainThread()
        precondition(c.ensureCount == 1 && m.cards.map(\.id) == ["main"] && m.focused?.id == "main")
        let duplicateRefresh = _Concurrency.Task { await m.refresh() }
        await until { c.mains.count == 1 && c.projects.count == 1 }
        c.main(false, .success("main")); c.projects.removeFirst().resume(returning: [project])
        await duplicateRefresh.value
        precondition(m.cards.map(\.id) == ["main", "root"] && c.writes.isEmpty, "Navigation reads must not register projects")

        // Every old completion (including its defer) runs while a new account request is suspended.
        for fail in [false, true] {
            let old = Model(), transport = old.client!
            old.openMainThread(); old.save()
            await until { transport.mains.count == 1 && transport.writes.count == 1 }
            old.mainThreadID = "cached-old-main"; old.canonicalProjects = [project]
            old.resetAccount()
            precondition(old.mainThreadID == nil && old.canonicalProjects.isEmpty && !old.openingMain && !old.savingProject)
            precondition(old.pendingProjectID == nil && old.pendingProjectName == nil)
            let fresh = old.client!
            old.openMainThread(); old.save()
            await until { fresh.mains.count == 1 && fresh.writes.count == 1 }
            transport.main(true, fail ? .failure(Failure()) : .success("old-main"))
            transport.writes.removeFirst().resume(with: fail ? .failure(Failure()) : .success(project))
            await old.jobs[0].value; await old.jobs[1].value
            precondition(old.openingMain && old.savingProject && old.error == nil)
            precondition(old.cards.isEmpty && old.mainThreadID == nil && old.canonicalProjects.isEmpty && old.focused == nil)
            precondition(old.pendingProjectID == "pending" && old.persisted == 0)
            fresh.main(true, .success("new-main")); await old.jobs[2].value
            fresh.writes.removeFirst().resume(returning: project); await old.jobs[3].value
            precondition(!old.openingMain && !old.savingProject && old.mainThreadID == "new-main")
        }
        // Refresh captured before ensure/create must not replace their newer navigation state.
        for create in [false, true] {
            let race = Model()
            let client = race.client!
            let refresh = _Concurrency.Task { await race.refresh() }
            await until { client.mains.count == 1 && client.projects.count == 1 }
            if create {
                race.save(); await until { client.writes.count == 1 }
                client.writes.removeFirst().resume(returning: project)
            } else {
                race.openMainThread(); await until { client.mains.count == 2 }
                client.main(true, .success("ensured"))
            }
            await race.jobs[0].value
            client.main(false, .success("stale-main"))
            client.projects.removeFirst().resume(returning: [InboxProject(id: "stale", name: "Stale", primaryAgentID: "stale-root")])
            await refresh.value
            precondition(!race.cards.contains { $0.id.hasPrefix("stale") })
            if create { precondition(race.canonicalProjects == [project]) }
            else { precondition(race.mainThreadID == "ensured" && race.canonicalProjects.isEmpty) }
        }
        let late = Model(), client = Client()
        late.client = client; late.select("initial"); late.save()
        await until { client.writes.count == 1 }
        late.select("user-choice")
        client.writes.removeFirst().resume(returning: project); await late.jobs[0].value
        precondition(late.focused?.id == "user-choice" && late.canonicalProjects == [project])
        precondition(late.pendingProjectID == nil && late.persisted == 1)
        let mainLate = Model()
        mainLate.openMainThread(); await until { mainLate.client!.mains.count == 1 }
        mainLate.select("user-choice")
        mainLate.client!.main(true, .success("main")); await mainLate.jobs[0].value
        precondition(mainLate.focused?.id == "user-choice")
        let resetRefresh = Model(), oldClient = resetRefresh.client!
        let refresh = _Concurrency.Task { await resetRefresh.refresh() }
        await until { oldClient.mains.count == 1 && oldClient.projects.count == 1 }
        resetRefresh.resetAccount()
        oldClient.main(false, .success("old-main")); oldClient.projects.removeFirst().resume(returning: [project])
        await refresh.value
        precondition(resetRefresh.cards.isEmpty && resetRefresh.mainThreadID == nil && resetRefresh.canonicalProjects.isEmpty)
        print("PASS: ensure coalescing, duplicate admission, reset clearing, stale success/error/defer isolation, ensure/create refresh races, selection preservation, reset refresh isolation, local versus canonical rename policy")
    }
}
'''
with tempfile.TemporaryDirectory(prefix='inbox-main-thread-tests-') as directory:
    path = Path(directory)
    (path / 'MainThreadTests.swift').write_text(source)
    subprocess.run(['swiftc', '-parse-as-library', str(path / 'MainThreadTests.swift'), '-o', str(path / 'tests')], check=True)
    subprocess.run([str(path / 'tests')], check=True, timeout=30)
