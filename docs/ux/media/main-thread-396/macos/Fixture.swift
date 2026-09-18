// Added only to an archived f517a439 build by prepare.py.
// Uses the implementation's AppModel, ContentView, toolbar, ProjectsView and chat views.
@MainActor enum EvidenceFixture {
    static var frameTimer: Timer?
    static var frameIndex = 0
    static func recordFrame() {
        guard let window = NSApp.windows.first(where: { $0.canBecomeMain }), let view = (window.attachedSheet ?? window).contentView?.superview else { return }
        view.layoutSubtreeIfNeeded()
        guard let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return }
        view.cacheDisplay(in: view.bounds, to: bitmap)
        let path = ProcessInfo.processInfo.environment["NANOCODEX_EVIDENCE_OUTPUT"]!
        try! bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: path + "/frames/" + String(format: "%05d", frameIndex) + ".png"))
        frameIndex += 1
    }
    static func json(_ text: String) -> JSONValue { try! JSONDecoder().decode(JSONValue.self, from: Data(text.utf8)) }
    static func start(_ model: AppModel) {
        model.state.connected = true
        model.state.defaultHandEnabled = false
        model.isStarting = false
        model.theme = "light"
        model.workspaceFilter = .all
        model.state.threads = [AgentThread(id: "fixture-main", title: "Main Thread", updatedAt: 0, turnCount: 0), AgentThread(id: "fixture-project", title: "Orchard website", updatedAt: 0, turnCount: 0)]
        model.runtime.requestOverride = { method, args in
            print("fixture RPC: \(method)")
            switch method {
            case "openMainThread": return json(#"{"id":"fixture-main","title":"Main Thread","updatedAt":0,"turnCount":0}"#)
            case "listProjects": return json(#"{"data":[{"id":"fixture-orchard","name":"Orchard website","coordinator_agent_id":"fixture-project"},{"id":"fixture-docs","name":"Documentation refresh","coordinator_agent_id":"fixture-docs-agent"}]}"#)
            case "openThread":
                let id = args.first?.string ?? "fixture-main"
                return json("{\"id\":\"\(id)\",\"events\":[],\"hasMore\":false,\"connected\":true,\"activeTurns\":[],\"settings\":{}}")
            default: return .null
            }
        }
        Task {
            try? await Task.sleep(for: .seconds(2))
            if let window = NSApp.windows.first(where: { $0.canBecomeMain }) {
                window.setContentSize(NSSize(width: 1280, height: 820))
            }
            frameTimer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { _ in
                Task { @MainActor in recordFrame() }
            }
            await capture("00", caption: "Main Thread entry — isolated native fixture")
            await model.openMainThread()
            try? await Task.sleep(for: .milliseconds(500))
            model.messages["fixture-main"] = [MessageEntry(id: "welcome", turnId: "intro", kind: .assistant, text: "What would you like to work on?\n\nI can help you create a project, register an existing coordinator, or open one of your projects.")]
            await capture("01", caption: "Main Thread opened — synthetic runtime response")
            let tabID = model.activeTabID
            let count = model.tabs.count
            await model.openMainThread()
            precondition(model.activeTabID == tabID && model.tabs.count == count, "Main Thread must reuse its tab")
            print("PASS: repeated openMainThread reused tab; tab count \(count)")
            await capture("02", caption: "Main Thread reopened — same tab asserted")
            await model.refreshProjects()
            model.showingProjects = true
            await capture("03", caption: "Projects picker — synthetic project list")
            model.openProject(model.projects[0])
            try? await Task.sleep(for: .milliseconds(500))
            precondition(model.activeTab?.threadId == "fixture-project")
            print("PASS: openProject selected fixture-project")
            model.messages["fixture-project"] = [MessageEntry(id: "project-user", turnId: "project-turn", kind: .user, text: "Review the Orchard website release plan."), MessageEntry(id: "project-assistant", turnId: "project-turn", kind: .assistant, text: "The project coordinator is ready.\n\nI’ll organize the release review and report the outcome here.")]
            await capture("04", caption: "Selected project coordinator — synthetic transcript")
            await model.openMainThread()
            model.messages["fixture-main"] = [MessageEntry(id: "request", turnId: "delegation", kind: .user, text: "Ask the Orchard website coordinator to review the release plan."), MessageEntry(id: "result", turnId: "delegation", kind: .assistant, text: "## Orchard website review\n\nThe coordinator reported that the release checklist is ready.\n\n- Navigation and content review completed.\n- Accessibility checks are recorded.\n- Deployment remains pending your approval.\n\nThis is a synthetic result fixture for the native transcript view.")]
            await capture("05", caption: "Returned result presentation — seeded text; no delegation executed")
            print("DONE: six native view snapshots; no account services started")
            frameTimer?.invalidate()
            print("RECORDED native view frames: \(frameIndex)")
            fflush(stdout)
            exit(0)
        }
    }
    static func capture(_ name: String, caption: String) async {
        try? await Task.sleep(for: .seconds(1))
        guard let window = NSApp.windows.first(where: { $0.canBecomeMain }), let view = (window.attachedSheet ?? window).contentView?.superview else { fatalError("Missing native window") }
        window.title = "PR396 fixture · " + caption
        view.layoutSubtreeIfNeeded()
        guard let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { fatalError("No bitmap") }
        view.cacheDisplay(in: view.bounds, to: bitmap)
        let path = ProcessInfo.processInfo.environment["NANOCODEX_EVIDENCE_OUTPUT"]!
        try! bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: path + "/" + name + ".png"))
        print("CAPTURE \(name): \(caption) \(bitmap.pixelsWide)x\(bitmap.pixelsHigh)")
    }
}
