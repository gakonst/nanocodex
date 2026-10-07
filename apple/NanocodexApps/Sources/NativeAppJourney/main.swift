import Foundation
import NanocodexApps
#if os(macOS)
import AppKit
import SwiftUI
#endif

private struct JourneyFailure: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

private enum Operation {
    case action(String)
    case set(String, AppValue)
}

private struct Options {
    var source: String?
    var state: String?
    var screenshot: String?
    var agentResponse: String?
    var operations: [Operation] = []
    var selfTest = false
    var help = false

    init(_ arguments: [String]) throws {
        selfTest = arguments.isEmpty
        var index = 0
        func next(_ flag: String) throws -> String {
            index += 1
            guard index < arguments.count else { throw JourneyFailure(message: "Missing value for \(flag). Run --help.") }
            return arguments[index]
        }
        while index < arguments.count {
            let flag = arguments[index]
            switch flag {
            case "--help", "-h": help = true
            case "--self-test": selfTest = true
            case "--source": source = try next(flag)
            case "--state": state = try next(flag)
            case "--screenshot": screenshot = try next(flag)
            case "--agent-response": agentResponse = try next(flag)
            case "--action": operations.append(.action(try next(flag)))
            case "--set":
                let name = try next(flag)
                let json = try next(flag)
                do { operations.append(.set(name, try JSONDecoder().decode(AppValue.self, from: Data(json.utf8)))) }
                catch { throw JourneyFailure(message: "Invalid JSON for --set \(name): \(error.localizedDescription)") }
            default: throw JourneyFailure(message: "Unknown option \(flag). Run --help.")
            }
            index += 1
        }
        if help { return }
        if selfTest {
            guard source == nil, state == nil, operations.isEmpty, agentResponse == nil else {
                throw JourneyFailure(message: "--self-test accepts only --screenshot FILE; it creates isolated state and fixtures.")
            }
        } else if source == nil || state == nil {
            throw JourneyFailure(message: "Supply --source FILE and --state FILE, or use --self-test.")
        }
    }
}

/// The only simulated service is the external Agent.run response. Storage is ordinary JSON on disk.
@MainActor
private final class FileHost {
    let url: URL
    let response: String?
    private(set) var prompts: [String] = []
    private(set) var saves = 0
    private(set) var begunActions = 0
    private(set) var committedActions = 0
    private(set) var activeSaves = 0
    private(set) var maximumActiveSaves = 0
    private(set) var savedCounters: [AppValue] = []
    let saveDelay: UInt64
    let agentDelay: UInt64

    init(url: URL, response: String?, saveDelay: UInt64 = 0, agentDelay: UInt64 = 0) {
        self.url = url
        self.response = response
        self.saveDelay = saveDelay
        self.agentDelay = agentDelay
    }

    func read() throws -> [String: AppValue] {
        guard FileManager.default.fileExists(atPath: url.path) else { return [:] }
        return try JSONDecoder().decode([String: AppValue].self, from: Data(contentsOf: url))
    }

    func host() -> NativeAppHost {
        NativeAppHost(loadState: { try self.read() }, saveState: { values in
            self.activeSaves += 1
            self.maximumActiveSaves = max(self.maximumActiveSaves, self.activeSaves)
            defer { self.activeSaves -= 1 }
            if self.saveDelay > 0 { try await Task.sleep(nanoseconds: self.saveDelay) }
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            let data = try encoder.encode(values)
            try FileManager.default.createDirectory(at: self.url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try data.write(to: self.url, options: .atomic)
            self.saves += 1
            if let counter = values["count"] { self.savedCounters.append(counter) }
            print("HOST save #\(self.saves) \(self.url.path)")
        }, runAgent: { prompt in
            self.prompts.append(prompt)
            print("HOST Agent.run prompt=\(quoted(prompt))")
            guard let response = self.response else {
                throw JourneyFailure(message: "Agent.run needs --agent-response STRING in this local journey host.")
            }
            // Yield across the actual async host boundary before completing the response.
            await Task.yield()
            if self.agentDelay > 0 { try await Task.sleep(nanoseconds: self.agentDelay) }
            print("HOST Agent.run response=\(quoted(response))")
            return response
        }, beginAction: { self.begunActions += 1 }, commitAction: { self.committedActions += 1 })
    }
}

private func quoted(_ text: String) -> String {
    String(data: (try? JSONEncoder().encode(text)) ?? Data(), encoding: .utf8) ?? "\"\""
}

private func flatten(_ nodes: [AppNode]) -> [AppNode] {
    nodes.flatMap { [$0] + flatten($0.children) }
}

@MainActor
private func trace(_ label: String, _ session: NativeAppSession) {
    print("TRACE \(label)")
    func printNodes(_ nodes: [AppNode], indent: String) {
        for node in nodes {
            let binding = node.binding.map { " binding=\($0) value=\(quoted(session.value(for: $0).text))" } ?? ""
            print("\(indent)\(node.kind) \(quoted(node.text))\(binding)\(node.actionID == nil ? "" : " actionable")")
            printNodes(node.children, indent: indent + "  ")
        }
    }
    printNodes(session.nodes, indent: "  ")
    if let diagnostic = session.diagnostic { print("  DIAGNOSTIC \(diagnostic.localizedDescription)") }
}

@MainActor
private func press(_ title: String, in session: NativeAppSession) async throws {
    let matches = flatten(session.nodes).filter { $0.kind == "Button" && ($0.text == title || $0.properties["title"]?.text == title || $0.properties["label"]?.text == title) }
    guard matches.count == 1, let node = matches.first, let actionID = node.actionID else {
        throw JourneyFailure(message: "Expected one actionable button titled \(quoted(title)); found \(matches.count).")
    }
    guard node.properties["disabled"]?.truth != true else { throw JourneyFailure(message: "Button \(quoted(title)) is disabled.") }
    print("INPUT button \(quoted(title))")
    let clock = ContinuousClock()
    let began = clock.now
    await session.perform(actionID: actionID)
    print("TIMING action \(quoted(title)) \(began.duration(to: clock.now))")
    trace("after button \(quoted(title))", session)
}

@MainActor
private func set(_ name: String, _ value: AppValue, in session: NativeAppSession) async throws {
    guard flatten(session.nodes).contains(where: { $0.binding == name }) else {
        throw JourneyFailure(message: "No rendered control binds to \(name).")
    }
    print("INPUT binding \(name)=\(String(data: try JSONEncoder().encode(value), encoding: .utf8)!)")
    let clock = ContinuousClock()
    let began = clock.now
    await session.setBinding(name, value: value)
    print("TIMING binding \(name) \(began.duration(to: clock.now))")
    trace("after binding \(name)", session)
}

@MainActor
private func openSession(source: String, host: NativeAppHost, limits: AppLimits = AppLimits()) async throws -> NativeAppSession {
    let clock = ContinuousClock()
    let parseBegan = clock.now
    let session = try NativeAppSession(source: source, host: host, limits: limits)
    print("TIMING parse \(parseBegan.duration(to: clock.now)) sourceBytes=\(source.utf8.count)")
    let startBegan = clock.now
    try await session.start()
    print("TIMING start \(startBegan.duration(to: clock.now)) nodes=\(flatten(session.nodes).count)")
    return session
}

@MainActor
private func healthy(_ session: NativeAppSession) throws {
    if let diagnostic = session.diagnostic { throw diagnostic }
}

private func expect(_ condition: @autoclosure () throws -> Bool, _ evidence: String) throws {
    guard try condition() else { throw JourneyFailure(message: "ASSERTION FAILED: \(evidence)") }
    print("PASS \(evidence)")
}

@MainActor
private func expectText(_ text: String, in session: NativeAppSession) throws {
    try expect(flatten(session.nodes).contains { $0.kind == "Text" && $0.text == text }, "rendered Text \(quoted(text))")
}

@MainActor
private func screenshot(_ path: String, session: NativeAppSession) async throws {
    #if os(macOS)
    let app = NSApplication.shared
    app.setActivationPolicy(.prohibited)
    app.appearance = NSAppearance(named: .aqua)
    app.finishLaunching()
    // A detached command-line window has no inherited appearance or active control state.
    // Supply the same opaque surface and active controls as an ordinary app window.
    let root = NativeAppView(session: session)
        .background(Color(nsColor: .windowBackgroundColor))
        .environment(\.colorScheme, .light)
        .environment(\.controlActiveState, .active)
    let view = NSHostingView(rootView: root)
    view.appearance = NSAppearance(named: .aqua)
    let rect = NSRect(x: 0, y: 0, width: 900, height: 1100)
    let window = NSWindow(contentRect: rect, styleMask: [.borderless], backing: .buffered, defer: false)
    window.isReleasedWhenClosed = false
    window.appearance = NSAppearance(named: .aqua)
    window.backgroundColor = .windowBackgroundColor
    window.isOpaque = true
    window.hasShadow = false
    window.contentView = view
    view.frame = rect
    window.orderFrontRegardless()
    // Allow SwiftUI's initial transaction to reach AppKit before caching the native view.
    try await Task.sleep(nanoseconds: 150_000_000)
    view.layoutSubtreeIfNeeded()
    view.displayIfNeeded()
    guard let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else {
        throw JourneyFailure(message: "Could not allocate native screenshot bitmap.")
    }
    view.cacheDisplay(in: view.bounds, to: bitmap)
    guard let data = bitmap.representation(using: .png, properties: [:]) else {
        throw JourneyFailure(message: "Could not encode native screenshot PNG.")
    }
    let url = URL(fileURLWithPath: path)
    try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    try data.write(to: url, options: .atomic)
    window.orderOut(nil)
    print("SCREENSHOT \(url.path) \(bitmap.pixelsWide)x\(bitmap.pixelsHigh), \(data.count) bytes; NSHostingView<NativeAppView>")
    #else
    throw JourneyFailure(message: "--screenshot requires macOS with AppKit and SwiftUI.")
    #endif
}

private let ledgerSource = #"""
import SwiftUI
struct Entry {
    var title: String
    var points: Int
}
struct JourneyLedger: View {
    @Persisted("entries") var entries = [Entry(title: "Seed", points: 2)]
    @State var draft = ""
    @State var answer = ""
    func score(_ items: [Entry]) -> Int {
        var total = 0
        for item in items {
            total += item.points * 2
        }
        return total
    }
    var body: some View {
        VStack {
            Text("Journey ledger").font(.title)
            TextField("Entry title", text: $draft)
            Button("Add entry") {
                if !draft.isEmpty {
                    entries.append(Entry(title: draft, points: 3))
                    draft = ""
                }
            }
            ForEach(entries, id: \.self) { item in
                Text("\(item.title): \(item.points)")
            }
            Text("Score: \(score(entries))")
            Button("Ask agent") {
                Task {
                    answer = await Agent.run("Explain score " + String(score(entries)))
                }
            }
            Text(answer)
            Button("Fail action") {
                entries.append(Entry(title: "Must roll back", points: 99))
                let impossible = 1 / 0
                answer = String(impossible)
            }
        }.padding()
    }
}
"""#

private let limitSource = #"""
struct BoundedJourney: View {
    @State var count = 7
    func recurse(_ value: Int) -> Int {
        return recurse(value + 1)
    }
    var body: some View {
        VStack {
            Text("Count: \(count)")
            Button("Spin") {
                count = 0
                while true { count += 1 }
            }
            Button("Recurse") { count = recurse(0) }
            Button("Recover") { count += 1 }
        }
    }
}
"""#

private let chartArtifactSource = """
struct Revenue: View {
    let quarters = [["label": "Q1", "value": 12, "series": "2025"], ["label": "Q2", "value": 18, "series": "2025"], ["label": "Q1", "value": 9, "series": "2024"]]
    @State var scale = 1
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Revenue").font(.headline)
            BarChart(quarters, title: "By quarter")
            LineChart([3, 5, 4, 8].map { $0 * scale })
            AreaChart(quarters)
            PointChart([1, 4, 2])
            PieChart([["label": "Search", "value": 60], ["label": "Direct", "value": 40]], title: "Traffic")
            Stepper("Scale \\(scale)", value: $scale, in: 1...5, step: 1)
        }
    }
}
"""

@MainActor
private func artifactSelfTest() async throws {
    let reply = "Here you go:\n\n```swift-artifact\n\(chartArtifactSource)\n```\n\nDone. ```swift\nlet x = 1\n```"
    let segments = ChatArtifactSegment.split(reply)
    try expect(segments.count == 3, "artifact reply splits into prose, artifact, prose: \(segments.count)")
    guard case .artifact(let source, true) = segments[1] else { throw JourneyFailure(message: "Expected completed artifact segment.") }
    if case .markdown(let tail) = segments[2] { try expect(tail.contains("```swift"), "ordinary code fences stay Markdown") }
    let streaming = ChatArtifactSegment.split("Plot:\n```swift-artifact\nstruct A: View {")
    try expect(streaming.last == .artifact(source: "struct A: View {", complete: false), "open artifact fence is pending while streaming")
    let nested = ChatArtifactSegment.split("````markdown\n```swift-artifact\nx\n```\n````")
    try expect(nested.count == 1, "artifact examples nested in another fence stay Markdown")
    try expect(ChatArtifactSegment.split("plain") == [.markdown("plain")], "plain text is one Markdown segment")
    let host = NativeAppHost(loadState: { [:] }, saveState: { _ in }, runAgent: { _ in throw AppDiagnostic("disabled") }, agentRequestsHaveExternalEffects: false)
    let session = try NativeAppSession(source: source, host: host, limits: AppLimits(agentCalls: 0))
    try await session.start()
    try healthy(session)
    func charts(_ nodes: [AppNode]) -> [AppNode] { nodes.flatMap { ($0.kind.hasSuffix("Chart") ? [$0] : []) + charts($0.children) } }
    let found = charts(session.nodes)
    try expect(found.map(\.kind) == ["BarChart", "LineChart", "AreaChart", "PointChart", "PieChart"], "all chart kinds render: \(found.map(\.kind))")
    let bar = ChartEntry.entries(found[0].properties["values"] ?? .null) ?? []
    try expect(bar.count == 3 && bar[0].label == "Q1" && bar[0].value == 12 && bar[2].series == "2024", "record chart data maps label/value/series")
    try expect(found[0].properties["title"] == .string("By quarter"), "chart title is retained")
    try await set("scale", .number(2), in: session)
    try healthy(session)
    let line = ChartEntry.entries(charts(session.nodes)[1].properties["values"] ?? .null) ?? []
    try expect(line.map(\.value) == [6, 10, 8, 16], "chart data re-evaluates from state: \(line.map(\.value))")
    do { _ = try NativeAppSession(source: "struct A: View { var body: some View { LineChart([1], color: 2) } }", host: host); throw JourneyFailure(message: "unexpected chart label accepted") }
    catch is AppDiagnostic { }
    print("PASS inline artifacts and charts")
}

@MainActor
private func selfTest(screenshotPath: String?) async throws {
    try await artifactSelfTest()
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent("native-app-journey-" + UUID().uuidString)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    print("EVIDENCE \(directory.path)")
    try ledgerSource.write(to: directory.appendingPathComponent("Ledger.swift"), atomically: true, encoding: .utf8)
    try limitSource.write(to: directory.appendingPathComponent("Limits.swift"), atomically: true, encoding: .utf8)
    let stateURL = directory.appendingPathComponent("state.json")
    let host = FileHost(url: stateURL, response: "Your score is 16.")
    let session = try await openSession(source: ledgerSource, host: host.host())
    trace("initial real parsed source", session)
    try expectText("Seed: 2", in: session)
    try expectText("Score: 4", in: session)
    try await set("draft", .string("Alpha"), in: session)
    try healthy(session)
    try expect(host.saves == 0, "@State binding does not save persisted defaults")
    try await press("Add entry", in: session)
    try healthy(session)
    try await set("draft", .string("Beta"), in: session)
    try await press("Add entry", in: session)
    try healthy(session)
    try expectText("Alpha: 3", in: session)
    try expectText("Beta: 3", in: session)
    try expectText("Score: 16", in: session)
    try expect(session.value(for: "draft") == .string(""), "button clears the text binding")
    let saved = try host.read()
    try expect(saved["draft"] == nil && saved["answer"] == nil, "disk JSON excludes session-only state")
    guard case .array(let entries) = saved["entries"] else { throw JourneyFailure(message: "Expected plain JSON entries array on disk.") }
    try expect(entries.count == 3 && entries.last == .object(["title": .string("Beta"), "points": .number(3)]), "disk JSON has three memberwise records")
    print("DISK \(String(data: try Data(contentsOf: stateURL), encoding: .utf8)!)")
    try await press("Ask agent", in: session)
    try healthy(session)
    try expect(host.prompts == ["Explain score 16"], "async Agent.run crosses host boundary with evaluated prompt")
    try expectText("Your score is 16.", in: session)
    try expect(host.begunActions == 3 && host.committedActions == 3, "action lifecycle commits after successful renders and saves; bindings do not begin actions")

    // Rapid input stays on the public binding boundary; every update must be observable.
    let inputClock = ContinuousClock()
    let inputBegan = inputClock.now
    for index in 0..<20 {
        try await set("draft", .string("Rapid \(index)"), in: session)
        try healthy(session)
    }
    try expect(session.value(for: "draft") == .string("Rapid 19"), "rapid repeated bindings preserve the latest edit")
    print("TIMING 20 binding updates \(inputBegan.duration(to: inputClock.now))")

    let beforeFailure = try Data(contentsOf: stateURL)
    try await press("Fail action", in: session)
    try expect(session.diagnostic?.message.localizedCaseInsensitiveContains("zero") == true, "division failure exposes diagnostic")
    try expectText("Score: 16", in: session)
    try expect(try Data(contentsOf: stateURL) == beforeFailure, "failed action leaves durable state byte-for-byte unchanged")
    try expect(!flatten(session.nodes).contains { $0.text.contains("Must roll back") }, "failed action rolls back rendered records")
    session.invalidate()

    let restartHost = FileHost(url: stateURL, response: "Restart host response")
    let restarted = try await openSession(source: ledgerSource, host: restartHost.host())
    trace("restart from the same JSON file", restarted)
    try expectText("Alpha: 3", in: restarted)
    try expectText("Beta: 3", in: restarted)
    try expectText("Score: 16", in: restarted)
    try expect(restarted.value(for: "answer") == .string(""), "restart resets @State and restores @Persisted")

    // Force a real filesystem save failure, then repair the file and retry the same public action.
    try await set("draft", .string("Recovered"), in: restarted)
    try FileManager.default.removeItem(at: stateURL)
    try FileManager.default.createDirectory(at: stateURL, withIntermediateDirectories: false)
    try await press("Add entry", in: restarted)
    try expect(restarted.diagnostic != nil, "filesystem save failure exposes a diagnostic")
    try expectText("Score: 16", in: restarted)
    try expect(restarted.value(for: "draft") == .string("Recovered"), "save failure restores binding and records")
    try FileManager.default.removeItem(at: stateURL)
    try beforeFailure.write(to: stateURL, options: .atomic)
    try await press("Add entry", in: restarted)
    try healthy(restarted)
    try expectText("Recovered: 3", in: restarted)
    try expectText("Score: 22", in: restarted)

    let reopened = try await openSession(source: ledgerSource, host: FileHost(url: stateURL, response: nil).host())
    try expectText("Recovered: 3", in: reopened)
    try expectText("Score: 22", in: reopened)
    trace("reopened after save-error recovery", reopened)
    reopened.invalidate()

    let boundedHost = FileHost(url: directory.appendingPathComponent("limits.json"), response: nil)
    let bounded = try await openSession(source: limitSource, host: boundedHost.host(), limits: AppLimits(steps: 600, depth: 24))
    for (button, diagnostic) in [("Spin", "step limit"), ("Recurse", "depth limit")] {
        try await press(button, in: bounded)
        try expect(bounded.diagnostic?.message.localizedCaseInsensitiveContains(diagnostic) == true, "\(button) stops at \(diagnostic)")
        try expectText("Count: 7", in: bounded)
    }
    try await press("Recover", in: bounded)
    try healthy(bounded)
    try expectText("Count: 8", in: bounded)
    let repeatClock = ContinuousClock()
    let repeatBegan = repeatClock.now
    for _ in 0..<4 {
        try await press("Recover", in: bounded)
        try healthy(bounded)
    }
    try expectText("Count: 12", in: bounded)
    print("TIMING 4 repeated button actions \(repeatBegan.duration(to: repeatClock.now))")
    bounded.invalidate()

    let queuedSource = #"""
    struct QueuedCounter: View {
        @Persisted("count") var count = 0
        var body: some View {
            VStack {
                Stepper("Count", value: $count, in: 0...100)
                Text("Queued count: \(count)")
            }
        }
    }
    """#
    try queuedSource.write(to: directory.appendingPathComponent("Queued.swift"), atomically: true, encoding: .utf8)
    let queuedHost = FileHost(url: directory.appendingPathComponent("queued.json"), response: nil, saveDelay: 2_000_000)
    let queued = try await openSession(source: queuedSource, host: queuedHost.host())
    var accepted: [Int] = []
    let queuedClock = ContinuousClock()
    let queuedBegan = queuedClock.now
    var updates: [Task<Void, Never>] = []
    for value in 1...20 {
        updates.append(Task { @MainActor in
            accepted.append(value)
            print("INPUT queued binding count=\(value)")
            await queued.setBinding("count", value: .number(Double(value)))
        })
    }
    for update in updates { await update.value }
    try healthy(queued)
    let expectedCounters = accepted.map { AppValue.number(Double($0)) }
    try expect(accepted.count == 20, "all 20 overlapping main-actor binding tasks were accepted")
    try expect(queuedHost.savedCounters == expectedCounters, "overlapping persisted bindings save in acceptance order: \(accepted)")
    try expect(queuedHost.maximumActiveSaves == 1 && queuedHost.saves == 20, "20 actual delayed JSON saves serialize without overlap")
    let expectedCounter = expectedCounters.last!
    try expect(queued.value(for: "count") == expectedCounter, "overlapping input preserves the last accepted value")
    try expect(try queuedHost.read()["count"] == expectedCounter, "queued input final value reaches the JSON file")
    trace("after overlapping binding tasks", queued)
    print("TIMING 20 overlapping persisted bindings \(queuedBegan.duration(to: queuedClock.now))")
    queued.invalidate()
    let queuedRestart = try await openSession(source: queuedSource, host: queuedHost.host())
    try expectText("Queued count: " + expectedCounter.text, in: queuedRestart)
    queuedRestart.invalidate()

    let lateHost = FileHost(url: directory.appendingPathComponent("late-agent.json"), response: "Must not appear after close", agentDelay: 50_000_000)
    let closing = try await openSession(source: ledgerSource, host: lateHost.host())
    guard let agentAction = flatten(closing.nodes).first(where: { $0.kind == "Button" && $0.text == "Ask agent" })?.actionID else {
        throw JourneyFailure(message: "Missing Ask agent action for invalidation journey.")
    }
    let pending = Task { @MainActor in await closing.perform(actionID: agentAction) }
    for _ in 0..<1000 {
        if !lateHost.prompts.isEmpty { break }
        await Task.yield()
    }
    try expect(lateHost.prompts == ["Explain score 4"], "Agent.run entered the delayed external host before invalidation")
    closing.invalidate()
    await pending.value
    trace("closed session after late agent response", closing)
    try expect(closing.nodes.isEmpty, "late Agent.run completion does not render a closed session")
    try expect(closing.value(for: "answer") == .string(""), "late Agent.run completion rolls back its pending state change")
    try expect(lateHost.saves == 0, "late Agent.run completion does not persist after close")

    // Host calls require a dispatched action, including calls hidden in functions.
    let forbiddenSources = [
        ("InitializerAgent", #"struct Blocked: View { @State var answer = await Agent.run("not authorized"); var body: some View { Text(answer) } }"#),
        ("InitializerFunction", #"struct Blocked: View { @State var answer = fetch(); func fetch() -> String { return await Agent.run("not authorized") }; var body: some View { Text(answer) } }"#),
        ("InitializerTask", #"struct Blocked: View { @State var answer = Task { let response = await Agent.run("not authorized") }; var body: some View { Text("Blocked") } }"#),
        ("RenderAgent", #"struct Blocked: View { func fetch() -> String { return await Agent.run("not authorized") }; var body: some View { Text(fetch()) } }"#)
    ]
    for (name, source) in forbiddenSources {
        try source.write(to: directory.appendingPathComponent(name + ".swift"), atomically: true, encoding: .utf8)
        let deniedHost = FileHost(url: directory.appendingPathComponent(name + ".json"), response: "Must not run")
        var rejection = ""
        do { let denied = try NativeAppSession(source: source, host: deniedHost.host()); try await denied.start(); denied.invalidate() }
        catch { rejection = error.localizedDescription; print("EXPECTED \(name): \(rejection)") }
        try expect(rejection.contains("actions") || rejection.contains("Actions"), "\(name) exposes action-context diagnostic")
        try expect(deniedHost.prompts.isEmpty && deniedHost.saves == 0, "\(name) makes zero agent or save calls")
    }
    let bindingGuardSource = #"struct Guarded: View { @State var enabled = false; var body: some View { VStack { Toggle("Enable", isOn: $enabled); if enabled { Text(await Agent.run("not authorized")) } } } }"#
    try bindingGuardSource.write(to: directory.appendingPathComponent("BindingGuard.swift"), atomically: true, encoding: .utf8)
    let bindingGuardHost = FileHost(url: directory.appendingPathComponent("binding-guard.json"), response: "Must not run")
    var bindingRejected = false
    do { _ = try NativeAppSession(source: bindingGuardSource, host: bindingGuardHost.host()) }
    catch { bindingRejected = true; print("EXPECTED binding render: \(error.localizedDescription)") }
    try expect(bindingRejected && bindingGuardHost.prompts.isEmpty, "source that would request an agent during a binding render is rejected before host access")

    let externalFailureSource = #"struct ExternalFailure: View { @Persisted("answer") var answer = ""; var body: some View { VStack { Text(answer); Button("Run") { answer = await Agent.run("Synthetic operation"); let bad = 1 / 0 } } } }"#
    try externalFailureSource.write(to: directory.appendingPathComponent("ExternalFailure.swift"), atomically: true, encoding: .utf8)
    let externalHost = FileHost(url: directory.appendingPathComponent("external-failure.json"), response: "Synthetic operation completed")
    let externalFailure = try await openSession(source: externalFailureSource, host: externalHost.host())
    try await press("Run", in: externalFailure)
    try expect(externalFailure.diagnostic?.message.contains("may have completed external work") == true, "post-agent action failure discloses external effect before retry")
    try expect(externalHost.prompts.count == 1 && externalHost.saves == 0, "failed action never automatically retries a completed agent request")
    try expect(externalHost.begunActions == 1 && externalHost.committedActions == 0, "post-agent failure keeps host receipt uncommitted for recovery")
    externalFailure.invalidate()

    let memoSource = #"struct Summaries: View { @State var count = 2; func summary() -> Int { return count * 3 }; func identity() -> String { return UUID().uuidString }; var body: some View { VStack { Text("First: \(summary())"); Text("Second: \(summary())"); Text(identity()); Text(identity()); Button("Increment") { count += 1 } } } }"#
    try memoSource.write(to: directory.appendingPathComponent("Summaries.swift"), atomically: true, encoding: .utf8)
    let summaries = try await openSession(source: memoSource, host: FileHost(url: directory.appendingPathComponent("summaries.json"), response: nil).host())
    try expectText("First: 6", in: summaries)
    try expectText("Second: 6", in: summaries)
    let identifiers = flatten(summaries.nodes).filter { $0.kind == "Text" && UUID(uuidString: $0.text) != nil }.map(\.text)
    try expect(identifiers.count == 2 && Set(identifiers).count == 2, "render memoization preserves fresh UUID function results")
    try await press("Increment", in: summaries)
    try healthy(summaries)
    try expectText("First: 9", in: summaries)
    try expectText("Second: 9", in: summaries)
    summaries.invalidate()

    let pagingSource = #"struct Pages: View { @Persisted("items") var items = [1, 2, 3, 4, 5]; @State var page = 0; var body: some View { VStack { Text("Recent: \(items.suffix(2).joined(separator: ","))"); Text("Page: \(items.dropFirst(page * 2).prefix(2).joined(separator: ","))"); Button("Next") { page += 1 }; Button("Bad page") { page = -1 } } } }"#
    try pagingSource.write(to: directory.appendingPathComponent("Pages.swift"), atomically: true, encoding: .utf8)
    let pagingHost = FileHost(url: directory.appendingPathComponent("pages.json"), response: nil)
    let pages = try await openSession(source: pagingSource, host: pagingHost.host())
    try expectText("Recent: 4,5", in: pages)
    try expectText("Page: 1,2", in: pages)
    try await press("Next", in: pages)
    try healthy(pages)
    try expectText("Page: 3,4", in: pages)
    try await press("Bad page", in: pages)
    try expect(pages.diagnostic != nil && pages.value(for: "page") == .number(1), "negative page count diagnoses and restores preceding page")
    try expect(pages.value(for: "items") == .array([1,2,3,4,5].map { .number(Double($0)) }) && pagingHost.saves == 0, "paging keeps complete history without writes")
    pages.invalidate()

    let cooperative = try await openSession(source: limitSource, host: boundedHost.host(), limits: AppLimits(steps: 10_000))
    let spinID = flatten(cooperative.nodes).first { $0.text == "Spin" }!.actionID!
    let spinning = Task { @MainActor in await cooperative.perform(actionID: spinID) }
    var observedDuringWork = false
    for _ in 0..<1000 {
        await Task.yield()
        if cooperative.isBusy && cooperative.value(for: "count") != .number(7) { observedDuringWork = true; break }
        if cooperative.diagnostic != nil { break }
    }
    await spinning.value
    try expect(observedDuringWork, "main actor services another task during bounded interpreted work")
    try expect(cooperative.diagnostic?.message.contains("step limit") == true && cooperative.value(for: "count") == .number(7), "cooperative execution still stops at budget and rolls back")
    cooperative.invalidate()

    let invalid = "import WebKit\nstruct Invalid: View { var body: some View { Text(\"Forbidden import\") } }"
    try invalid.write(to: directory.appendingPathComponent("Invalid.swift"), atomically: true, encoding: .utf8)
    var rejected = false
    do { _ = try NativeAppSession(source: invalid, host: restartHost.host()) }
    catch { rejected = true; print("EXPECTED invalid source: \(error.localizedDescription)") }
    try expect(rejected, "unsupported source rejected at public session initialization")
    if let screenshotPath { try await screenshot(screenshotPath, session: restarted) }
    restarted.invalidate()
    print("PASS SELF-TEST: parsed Swift, binding/buttons, records/functions/loops, JSON restart, async agent, invalid source, execution limits, rollback, queued saves, late response invalidation and recovery")
}

@main
private struct NativeAppJourney {
    @MainActor
    static func main() async {
        if Array(CommandLine.arguments.dropFirst()) == ["--validate-json"] {
            // Read one bounded request; stdout contains only the structured validation result.
            var data = Data()
            while data.count <= NativeAppPreflight.maximumInputBytes {
                let chunk = (try? FileHandle.standardInput.read(upToCount: min(65_536, NativeAppPreflight.maximumInputBytes + 1 - data.count))) ?? Data()
                if chunk.isEmpty { break }
                data.append(chunk)
            }
            let result = await NativeAppPreflight.validate(json: data)
            let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys]
            if let output = try? encoder.encode(result) {
                FileHandle.standardOutput.write(output)
                FileHandle.standardOutput.write(Data("\n".utf8))
            }
            return
        }
        do {
            let options = try Options(Array(CommandLine.arguments.dropFirst()))
            if options.help {
                print("""
                native-app-journey — run Swift source through the public native app session

                Usage:
                  native-app-journey --validate-json < request.json
                  native-app-journey --self-test [--screenshot FILE]
                  native-app-journey --source FILE --state FILE [operations] [--screenshot FILE]

                Operations execute in the exact order supplied:
                  --set NAME JSON          Set a currently rendered binding (repeatable).
                  --action TITLE           Press the uniquely titled current button (repeatable).
                  --agent-response STRING  Stub only the external Agent.run service.
                  --screenshot FILE        Capture real NativeAppView through NSHostingView to PNG (macOS).
                  --help                   Show this help.

                No arguments runs --self-test with isolated temporary files.
                --state reads/writes ordinary JSON; a missing file starts with declared defaults.
                Trace output includes controls, inputs, host calls and diagnostics. Errors exit nonzero.
                Without --agent-response, an Agent.run action produces an explicit host error.
                Example:
                  native-app-journey --source Tracker.swift --state output/state.json \\
                    --set title '\"The Odyssey\"' --action 'Add book' \\
                    --set title '\"Dune\"' --action 'Add book' --screenshot output/tracker.png
                """)
                return
            }
            if options.selfTest { try await selfTest(screenshotPath: options.screenshot); return }
            let sourceURL = URL(fileURLWithPath: options.source!)
            let host = FileHost(url: URL(fileURLWithPath: options.state!), response: options.agentResponse)
            let session = try await openSession(source: String(contentsOf: sourceURL, encoding: .utf8), host: host.host())
            defer { session.invalidate() }
            trace("loaded \(sourceURL.path)", session)
            for operation in options.operations {
                switch operation {
                case .action(let title): try await press(title, in: session)
                case .set(let name, let value): try await set(name, value, in: session)
                }
                try healthy(session)
            }
            if let path = options.screenshot { try await screenshot(path, session: session) }
            print("PASS journey completed; state=\(host.url.path)")
        } catch {
            FileHandle.standardError.write(Data("FAIL \(error.localizedDescription)\n".utf8))
            exit(1)
        }
    }
}
