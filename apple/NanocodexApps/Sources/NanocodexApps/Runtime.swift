import Foundation
#if os(Linux)
import OpenCombine
#else
import Combine
#endif

@MainActor
public final class NativeAppSession: ObservableObject {
    @Published public private(set) var nodes: [AppNode] = []
    @Published public private(set) var diagnostic: AppDiagnostic?
    @Published public private(set) var isBusy = false
    public private(set) var state: [String: AppValue] = [:]
    private let program: AppProgram
    private let host: NativeAppHost
    private let limits: AppLimits
    private var actions: [String: CapturedAction] = [:]
    private var steps = 0
    private var depth = 0
    private var rendered = 0
    private var agentCalls = 0
    private var started = false
    private var starting = false
    private var actionSequence = 0
    private var rendering = false
    private var executingAction = false
    private var renderTime = Date().timeIntervalSince1970
    private var volatileCalls = 0
    private struct FunctionKey: Hashable { let name: String; let arguments: [AppValue] }
    private var renderCache: [FunctionKey: AppValue] = [:]
    private var invalidated = false
    private var storedState: [String: AppValue] = [:]
    private var waiters: [CheckedContinuation<Void, Never>] = []
    private func acquire() async -> Bool {
        guard waiters.count < 128 else { diagnostic = AppDiagnostic("Too many pending app actions."); return false }
        if isBusy { await withCheckedContinuation { waiters.append($0) } } else { isBusy = true }
        return !invalidated
    }
    private func release() {
        if waiters.isEmpty { isBusy = false } else { waiters.removeFirst().resume() }
    }
    private struct CapturedAction { var closure: Closure; var locals: [String: AppValue] }
    private enum Flow: Error { case returned(AppValue) }

    public init(source: String, host: NativeAppHost, limits: AppLimits = AppLimits()) throws {
        self.program = try AppSourceParser.parse(source)
        self.host = host
        self.limits = limits
    }
    public static func validate(source: String) throws { _ = try AppSourceParser.parse(source) }
    public func value(for name: String) -> AppValue { state[name] ?? .null }

    public func invalidate() {
        invalidated = true; actions = [:]; nodes = []; isBusy = false
        let pending = waiters; waiters = []
        for waiter in pending { waiter.resume() }
    }

    public func start() async throws {
        guard !invalidated, !started, !starting else { return }
        starting = true; isBusy = true
        defer { starting = false; isBusy = false }
        do {
            resetBudget()
            let stored = try await host.loadState()
            guard !invalidated else { throw AppDiagnostic("App session closed.") }
            storedState = stored
            var locals: [String: AppValue] = [:]
            for declaration in program.states {
                let initial = try await evaluate(declaration.initial, locals: &locals)
                state[declaration.name] = declaration.persistedKey.flatMap { stored[$0] } ?? initial
                try await validateValue(state[declaration.name]!)
            }
            try await render()
            started = true
            diagnostic = nil
        } catch { fail(error); throw error }
    }

    public func perform(actionID: String) async {
        guard !invalidated, started, let action = actions[actionID] else { return }
        guard await acquire() else { return }
        defer { release() }
        guard !invalidated else { return }
        diagnostic = nil
        let before = state
        host.beginAction()
        do {
            resetBudget()
            var locals = action.locals
            executingAction = true
            do {
                defer { executingAction = false }
                do { try await execute(action.closure.body, locals: &locals) } catch Flow.returned { }
            }
            try await render()
            try await persist(ifChangedFrom: before)
            host.commitAction()
        } catch {
            let hadAgentCall = agentCalls > 0
            state = before
            // A failed action cannot leave partially changed UI state.
            resetBudget()
            try? await render()
            if hadAgentCall && host.agentRequestsHaveExternalEffects {
                let detail = (error as? AppDiagnostic)?.message ?? error.localizedDescription
                fail(AppDiagnostic(detail + " Local changes were rolled back, but an agent request was already sent and may have completed external work. Check agent activity before retrying."))
            } else { fail(error) }
        }
    }

    public func setBinding(_ name: String, value: AppValue) async {
        guard !invalidated, started, state[name] != nil else { return }
        guard await acquire() else { return }
        defer { release() }
        guard !invalidated else { return }
        diagnostic = nil
        let before = state
        do {
            try await validateValue(value)
            state[name] = value
            resetBudget()
            try await render()
            try await persist(ifChangedFrom: before)
        } catch {
            state = before
            resetBudget(); try? await render(); fail(error)
        }
    }

    private func fail(_ error: Error) { diagnostic = error as? AppDiagnostic ?? AppDiagnostic(error.localizedDescription) }
    private func resetBudget() { steps = 0; depth = 0; rendered = 0; agentCalls = 0 }
    private func tick() async throws {
        steps += 1
        // Keep interpreted work cooperative on the main actor without removing its bound.
        if steps.isMultiple(of: 1_024) { await Task.yield() }
        if invalidated { throw AppDiagnostic("App session closed.") }
        if Task.isCancelled { throw AppDiagnostic("App operation cancelled.") }
        guard steps <= limits.steps else { throw AppDiagnostic("Execution step limit exceeded.") }
    }
    private func enter() async throws {
        try await tick(); depth += 1
        guard depth <= limits.depth else { depth -= 1; throw AppDiagnostic("Execution depth limit exceeded.") }
    }
    private func validateValue(_ value: AppValue, level: Int = 0, recursive: Bool = true) async throws {
        try await tick()
        guard level <= limits.depth else { throw AppDiagnostic("Stored value depth limit exceeded.") }
        switch value {
        case .string(let s): guard s.utf8.count <= 262_144 else { throw AppDiagnostic("Text size limit exceeded.") }
        case .number(let n): guard n.isFinite else { throw AppDiagnostic("Numbers must be finite.") }
        case .array(let a): guard a.count <= limits.collectionCount else { throw AppDiagnostic("Collection limit exceeded.") }; if recursive { for v in a { try await validateValue(v, level: level + 1) } }
        case .object(let o): guard o.count <= limits.collectionCount else { throw AppDiagnostic("Collection limit exceeded.") }; if recursive { for v in o.values { try await validateValue(v, level: level + 1) } }
        default: break
        }
    }
    private func persist(ifChangedFrom previous: [String: AppValue]) async throws {
        guard program.states.contains(where: { $0.persistedKey != nil && state[$0.name] != previous[$0.name] }) else { return }
        var values = storedState
        for declaration in program.states { if let key = declaration.persistedKey { values[key] = state[declaration.name] } }
        for value in values.values { try await validateValue(value) }
        guard !invalidated else { throw AppDiagnostic("App session closed.") }
        guard values != storedState else { return }
        try await host.saveState(values)
        storedState = values
    }
    private func resolve(_ name: String, locals: inout [String: AppValue]) async throws -> AppValue {
        if let value = locals[name] ?? state[name] { return value }
        if let expression = program.constants[name] { return try await evaluate(expression, locals: &locals) }
        if name == "true" { return .bool(true) }
        if name == "false" { return .bool(false) }
        if name == "nil" { return .null }
        throw AppDiagnostic("Unknown value ‘\(name)’.")
    }

    private func evaluate(_ expression: Expr, locals: inout [String: AppValue]) async throws -> AppValue {
        try await enter(); defer { depth -= 1 }
        let result: AppValue
        switch expression {
        case .value(let value): result = value
        case .variable(let name): result = try await resolve(name, locals: &locals)
        case .binding(let name): result = state[name] ?? .null
        case .array(let expressions):
            guard expressions.count <= limits.collectionCount else { throw AppDiagnostic("Collection limit exceeded.") }
            var values: [AppValue] = []; for expression in expressions { values.append(try await evaluate(expression, locals: &locals)) }; result = .array(values)
        case .dictionary(let elements):
            var values: [String: AppValue] = [:]
            for (key, value) in elements { let k = try await evaluate(key, locals: &locals).text; values[k] = try await evaluate(value, locals: &locals) }; result = .object(values)
        case .interpolation(let parts):
            var output = ""; for part in parts { output += try await evaluate(part, locals: &locals).text }; result = .string(output)
        case .member(let base, let member):
            if case .variable("") = base { return .string(member) }
            if case .variable("Color") = base { return .string(member) }
            if case .variable("Font") = base { return .string(member) }
            if case .variable("self") = base { return try await resolve(member, locals: &locals) }
            let value = try await evaluate(base, locals: &locals)
            switch (value, member) {
            case (.array(let values), "count"): result = .number(Double(values.count))
            case (.object(let values), "count"): result = .number(Double(values.count))
            case (.string(let string), "count"): result = .number(Double(string.count))
            case (.array(let values), "isEmpty"): result = .bool(values.isEmpty)
            case (.string(let string), "isEmpty"): result = .bool(string.isEmpty)
            case (.array(let values), "first"): result = values.first ?? .null
            case (.array(let values), "last"): result = values.last ?? .null
            case (.object(let values), _): result = values[member] ?? .null
            default: throw AppDiagnostic("Unsupported member ‘\(member)’.")
            }
        case .index(let base, let index):
            let value = try await evaluate(base, locals: &locals), key = try await evaluate(index, locals: &locals)
            if case .array(let values) = value {
                let i = try integerIndex(key)
                guard values.indices.contains(i) else { throw AppDiagnostic("Array index out of bounds.") }; result = values[i]
            } else if case .object(let values) = value { result = values[key.text] ?? .null }
            else { throw AppDiagnostic("Subscript requires an array or dictionary.") }
        case .unary(let op, let operand):
            let value = try await evaluate(operand, locals: &locals)
            switch op { case "!": result = .bool(!value.truth); case "-": result = .number(-value.number); case "+": result = .number(value.number); default: throw AppDiagnostic("Unsupported unary operator \(op).") }
        case .binary(let op, let lhs, let rhs):
            let left = try await evaluate(lhs, locals: &locals)
            if op == "&&", !left.truth { return .bool(false) }
            if op == "||", left.truth { return .bool(true) }
            if op == "??", left != .null { return left }
            let right = try await evaluate(rhs, locals: &locals)
            result = try binary(op, left, right)
        case .conditional(let condition, let yes, let no):
            let test = try await evaluate(condition, locals: &locals)
            result = try await evaluate(test.truth ? yes : no, locals: &locals)
        case .awaitValue(let expression): result = try await evaluate(expression, locals: &locals)
        case .call(let callee, let arguments, let closure): result = try await call(callee, arguments: arguments, closure: closure, locals: &locals)
        }
        // Children were validated when constructed or loaded; reading an array must not
        // recursively revalidate every record on every member/subscript access.
        try await validateValue(result, recursive: false)
        return result
    }
    private func integerIndex(_ value: AppValue) throws -> Int {
        let n = value.number
        guard n.isFinite, n.rounded() == n, n >= 0, n < Double(Int.max) else { throw AppDiagnostic("Invalid array index.") }
        return Int(n)
    }
    private func binary(_ op: String, _ lhs: AppValue, _ rhs: AppValue) throws -> AppValue {
        switch op {
        case "+":
            if case .string = lhs { return .string(lhs.text + rhs.text) }
            if case .string = rhs { return .string(lhs.text + rhs.text) }
            if case .array(let a) = lhs, case .array(let b) = rhs { guard a.count + b.count <= limits.collectionCount else { throw AppDiagnostic("Collection limit exceeded.") }; return .array(a + b) }
            return .number(lhs.number + rhs.number)
        case "-": return .number(lhs.number - rhs.number)
        case "*": return .number(lhs.number * rhs.number)
        case "/": guard rhs.number != 0 else { throw AppDiagnostic("Division by zero.") }; return .number(lhs.number / rhs.number)
        case "%": guard rhs.number != 0 else { throw AppDiagnostic("Division by zero.") }; return .number(lhs.number.truncatingRemainder(dividingBy: rhs.number))
        case "==": return .bool(lhs == rhs)
        case "!=": return .bool(lhs != rhs)
        case "<": return .bool(compare(lhs, rhs))
        case ">": return .bool(compare(rhs, lhs))
        case "<=": return .bool(!compare(rhs, lhs))
        case ">=": return .bool(!compare(lhs, rhs))
        case "&&": return .bool(lhs.truth && rhs.truth)
        case "||": return .bool(lhs.truth || rhs.truth)
        case "??": return lhs == .null ? rhs : lhs
        case "..<", "...":
            let lower = try integerIndex(lhs), upper = try integerIndex(rhs)
            let count = upper - lower + (op == "..." ? 1 : 0)
            guard count >= 0, count <= limits.collectionCount else { throw AppDiagnostic("Range collection limit exceeded.") }
            return .array((0..<count).map { .number(Double(lower + $0)) })
        default: throw AppDiagnostic("Unsupported operator ‘\(op)’.")
        }
    }
    private func compare(_ lhs: AppValue, _ rhs: AppValue) -> Bool {
        if case .string(let a) = lhs, case .string(let b) = rhs { return a < b }; return lhs.number < rhs.number
    }

    private func call(_ callee: Expr, arguments: [Argument], closure: Closure?, locals: inout [String: AppValue]) async throws -> AppValue {
        if case .variable("Task") = callee {
            guard executingAction && !rendering, let closure else { throw AppDiagnostic("Task is only allowed in actions.") }
            try await execute(closure.body, locals: &locals); return .null
        }
        var values: [AppValue] = []
        for argument in arguments { values.append(try await evaluate(argument.value, locals: &locals)) }
        let first = values.first ?? .null
        if case .variable(let name) = callee {
            if let function = program.functions[name] {
                guard values.count == function.parameters.count else { throw AppDiagnostic("\(name) expects \(function.parameters.count) arguments.") }
                let key = FunctionKey(name: name, arguments: values)
                if rendering, let cached = renderCache[key] { return cached }
                let volatility = volatileCalls
                var scope = Dictionary(uniqueKeysWithValues: zip(function.parameters, values))
                let result: AppValue
                do { try await execute(function.body, locals: &scope); result = .null }
                catch Flow.returned(let value) { result = value }
                // Render state cannot mutate. Date/Clock share this render's snapshot;
                // UUID calls (including nested calls) must retain their fresh identity.
                if rendering, volatility == volatileCalls, renderCache.count < 128 { renderCache[key] = result }
                return result
            }
            if let fields = program.records[name] {
                guard arguments.count == fields.count else { throw AppDiagnostic("\(name) requires fields: \(fields.joined(separator: ", ")).") }
                var record: [String: AppValue] = [:]
                for (index, field) in fields.enumerated() {
                    guard arguments[index].label == field else { throw AppDiagnostic("Expected field \(field) in \(name).") }; record[field] = values[index]
                }
                return .object(record)
            }
            switch name {
            case "String": return .string(first.text)
            case "Int":
                if case .string(let text) = first { return Int(text).map { .number(Double($0)) } ?? .null }
                let n = first.number; guard n.isFinite else { throw AppDiagnostic("Invalid integer.") }; return .number(n.rounded(.towardZero))
            case "Double":
                if case .string(let text) = first { return Double(text).map(AppValue.number) ?? .null }
                return .number(first.number)
            case "Bool":
                if case .string(let text) = first { return Bool(text).map(AppValue.bool) ?? .null }
                return .bool(first.truth)
            case "min": return .number(values.map(\.number).min() ?? 0)
            case "max": return .number(values.map(\.number).max() ?? 0)
            case "abs": return .number(abs(first.number))
            case "round": return .number(first.number.rounded())
            case "Array": return first
            case "UUID": volatileCalls += 1; return .object(["uuidString": .string(UUID().uuidString)])
            case "Date":
                let timestamp = values.isEmpty ? (rendering ? renderTime : Date().timeIntervalSince1970) : first.number
                guard timestamp.isFinite, abs(timestamp) < 8_640_000_000_000 else { throw AppDiagnostic("Date is outside the supported range.") }
                return .object(["timeIntervalSince1970": .number(timestamp)])
            default: throw AppDiagnostic("Unknown function ‘\(name)’.")
            }
        }
        if case .member(let base, let name) = callee {
            if case .variable("Agent") = base, name == "run" {
                guard executingAction && !rendering else { throw AppDiagnostic("Agent.run is only allowed in actions.") }
                agentCalls += 1
                guard agentCalls <= limits.agentCalls else { throw AppDiagnostic("Agent call limit exceeded.") }
                guard !first.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw AppDiagnostic("Agent prompt must not be empty.") }
                let response = try await host.runAgent(first.text)
                try await tick()
                return .string(response)
            }
            if case .variable("Clock") = base {
                let timestamp = name == "today" ? (rendering ? renderTime : Date().timeIntervalSince1970) : first.number
                guard timestamp.isFinite, abs(timestamp) < 8_640_000_000_000 else { throw AppDiagnostic("Date is outside the supported range.") }
                let parts = Calendar.current.dateComponents([.year, .month, .day], from: Date(timeIntervalSince1970: timestamp))
                return .string(String(format: "%04d-%02d-%02d", parts.year ?? 0, parts.month ?? 0, parts.day ?? 0))
            }
            let value = try await evaluate(base, locals: &locals)
            if case .array(let array) = value, ["map", "filter", "reduce"].contains(name), let closure {
                var output: [AppValue] = []
                var accumulated = first
                for item in array {
                    try await tick()
                    var scope = locals
                    let params = closure.parameters.isEmpty ? ["$0", "$1"] : closure.parameters
                    if name == "reduce" {
                        guard params.count >= 2 else { throw AppDiagnostic("reduce needs two closure parameters.") }
                        scope[params[0]] = accumulated; scope[params[1]] = item
                    } else { scope[params[0]] = item }
                    let evaluated = try await closureValue(closure.body, locals: &scope)
                    if name == "map" { output.append(evaluated) }
                    else if name == "filter", evaluated.truth { output.append(item) }
                    else if name == "reduce" { accumulated = evaluated }
                }
                return name == "reduce" ? accumulated : .array(output)
            }
            switch (value, name) {
            case (.array(var array), "append"):
                guard array.count < limits.collectionCount else { throw AppDiagnostic("Collection limit exceeded.") }
                array.append(first); try await assign(base, value: .array(array), locals: &locals); return .null
            case (.array(var array), "removeLast"):
                guard !array.isEmpty else { throw AppDiagnostic("Cannot remove from an empty array.") }
                let removed = array.removeLast(); try await assign(base, value: .array(array), locals: &locals); return removed
            case (.array, "removeAll"): try await assign(base, value: .array([]), locals: &locals); return .null
            case (.array(let array), "prefix"): return .array(Array(array.prefix(try integerIndex(first))))
            case (.array(let array), "suffix"): return .array(Array(array.suffix(try integerIndex(first))))
            case (.array(let array), "dropFirst"): return .array(Array(array.dropFirst(try integerIndex(first))))
            case (.array(let array), "contains"): return .bool(array.contains(first))
            case (.array(let array), "joined"): return .string(array.map(\.text).joined(separator: first == .null ? "" : first.text))
            case (.array(let array), "reversed"): return .array(array.reversed())
            case (.array(let array), "sorted"): return .array(array.sorted(by: compare))
            case (.string(let text), "contains"): return .bool(text.contains(first.text))
            case (.string(let text), "lowercased"): return .string(text.lowercased())
            case (.string(let text), "uppercased"): return .string(text.uppercased())
            case (.string(let text), "trimmingCharacters"): return .string(text.trimmingCharacters(in: .whitespacesAndNewlines))
            case (.number(let n), "rounded"): return .number(n.rounded())
            case (.object(let date), "formatted"):
                guard let timestamp = date["timeIntervalSince1970"] else { throw AppDiagnostic("formatted() requires a Date.") }
                return .string(Date(timeIntervalSince1970: timestamp.number).formatted(date: .abbreviated, time: .shortened))
            default: throw AppDiagnostic("Unsupported method ‘\(name)’.")
            }
        }
        throw AppDiagnostic("Unsupported function call.")
    }
    private func assign(_ target: Expr, value: AppValue, locals: inout [String: AppValue]) async throws {
        try await validateValue(value)
        switch target {
        case .variable(let name):
            if locals[name] != nil { locals[name] = value }
            else if state[name] != nil {
                guard executingAction && !rendering else { throw AppDiagnostic("State can only change in actions or through a binding.") }
                state[name] = value
            }
            else { throw AppDiagnostic("Cannot assign unknown or constant value ‘\(name)’.") }
        case .member(let base, let name):
            if case .variable("self") = base { try await assign(.variable(name), value: value, locals: &locals); return }
            guard case .object(var object) = try await evaluate(base, locals: &locals) else { throw AppDiagnostic("Member assignment requires a record.") }
            object[name] = value; try await assign(base, value: .object(object), locals: &locals)
        case .index(let base, let index):
            let current = try await evaluate(base, locals: &locals), key = try await evaluate(index, locals: &locals)
            if case .array(var array) = current { let i = try integerIndex(key); guard array.indices.contains(i) else { throw AppDiagnostic("Array index out of bounds.") }; array[i] = value; try await assign(base, value: .array(array), locals: &locals) }
            else if case .object(var object) = current { object[key.text] = value; try await assign(base, value: .object(object), locals: &locals) }
            else { throw AppDiagnostic("Subscript assignment requires a collection.") }
        default: throw AppDiagnostic("Invalid assignment target.")
        }
    }
    private func closureValue(_ statements: [Statement], locals: inout [String: AppValue]) async throws -> AppValue {
        if statements.count == 1, case .expression(let expression) = statements[0] { return try await evaluate(expression, locals: &locals) }
        do { try await execute(statements, locals: &locals) } catch Flow.returned(let result) { return result }
        return .null
    }
    private func execute(_ statements: [Statement], locals: inout [String: AppValue]) async throws {
        try await enter(); defer { depth -= 1 }
        for statement in statements {
            try await tick()
            switch statement {
            case .expression(let expression): _ = try await evaluate(expression, locals: &locals)
            case .variable(let name, let expression): locals[name] = try await evaluate(expression, locals: &locals)
            case .assign(let target, let op, let expression):
                var value = try await evaluate(expression, locals: &locals)
                if op != "=" { value = try binary(String(op.dropLast()), try await evaluate(target, locals: &locals), value) }
                try await assign(target, value: value, locals: &locals)
            case .condition(let condition, let yes, let no):
                let test = try await evaluate(condition, locals: &locals); try await execute(test.truth ? yes : no, locals: &locals)
            case .forEach(let name, let expression, let body):
                guard case .array(let array) = try await evaluate(expression, locals: &locals) else { throw AppDiagnostic("for requires an array or range.") }
                for value in array { locals[name] = value; try await execute(body, locals: &locals) }
            case .whileLoop(let condition, let body):
                while try await evaluate(condition, locals: &locals).truth { try await execute(body, locals: &locals) }
            case .returnValue(let expression):
                if let expression { throw Flow.returned(try await evaluate(expression, locals: &locals)) }; throw Flow.returned(.null)
            }
        }
    }

    private func render() async throws {
        rendering = true
        renderTime = Date().timeIntervalSince1970
        renderCache = [:]
        defer { rendering = false; renderCache = [:] }
        rendered = 0; actions = [:]
        var locals: [String: AppValue] = [:]
        let output = try await renderStatements(program.body, locals: &locals, path: "body")
        nodes = output
    }
    private func renderStatements(_ statements: [Statement], locals: inout [String: AppValue], path: String) async throws -> [AppNode] {
        try await enter(); defer { depth -= 1 }
        var result: [AppNode] = []
        for (index, statement) in statements.enumerated() {
            try await tick()
            let childPath = "\(path).\(index)"
            switch statement {
            case .expression(let expression): result += try await renderExpression(expression, locals: &locals, path: childPath)
            case .variable(let name, let expression): locals[name] = try await evaluate(expression, locals: &locals)
            case .condition(let condition, let yes, let no):
                let test = try await evaluate(condition, locals: &locals)
                result += try await renderStatements(test.truth ? yes : no, locals: &locals, path: childPath + (test.truth ? ".yes" : ".no"))
            case .forEach(let name, let expression, let body):
                guard case .array(let values) = try await evaluate(expression, locals: &locals) else { throw AppDiagnostic("View loop requires an array.") }
                for (index, value) in values.enumerated() { var scope = locals; scope[name] = value; result += try await renderStatements(body, locals: &scope, path: childPath + ".\(index)") }
            case .returnValue(let expression): if let expression { result += try await renderExpression(expression, locals: &locals, path: childPath) }
            default: throw AppDiagnostic("Only view expressions, local values, conditionals, and for loops are allowed in a view body.")
            }
        }
        return result
    }
    private static let viewNames: Set<String> = ["VStack", "HStack", "ZStack", "Form", "Section", "ScrollView", "List", "Group", "ForEach", "Text", "Label", "Image", "Button", "TextField", "SecureField", "TextEditor", "Toggle", "Stepper", "Slider", "Picker", "Divider", "Spacer", "ProgressView", "Gauge", "BarChart", "LineChart", "AreaChart", "PointChart", "PieChart", "NavigationStack"]
    private func renderExpression(_ expression: Expr, locals: inout [String: AppValue], path: String) async throws -> [AppNode] {
        try await enter(); defer { depth -= 1 }
        guard case .call(let callee, let arguments, let closure) = expression else { throw AppDiagnostic("Expected a SwiftUI view expression.") }
        if case .member(let base, let modifier) = callee {
            var output = try await renderExpression(base, locals: &locals, path: path)
            var values: [AppValue] = []
            for argument in arguments { values.append(try await evaluate(argument.value, locals: &locals)) }
            for index in output.indices {
                if modifier == "frame" {
                    for (argIndex, argument) in arguments.enumerated() {
                        if let label = argument.label { output[index].properties["frame" + label.prefix(1).uppercased() + label.dropFirst()] = values[argIndex] }
                    }
                } else if modifier == "padding" {
                    if let first = values.first, case .string = first {
                        output[index].properties["paddingEdges"] = first
                        output[index].properties[modifier] = values.count > 1 ? values[1] : .null
                    } else { output[index].properties[modifier] = values.first ?? .null }
                } else { output[index].properties[modifier] = values.first ?? .bool(true) }
            }
            return output
        }
        guard case .variable(let kind) = callee else { throw AppDiagnostic("Expected a named view.") }
        if let function = program.functions[kind] {
            var scope: [String: AppValue] = [:]
            guard function.parameters.count == arguments.count else { throw AppDiagnostic("Wrong argument count for \(kind).") }
            for (index, argument) in arguments.enumerated() { scope[function.parameters[index]] = try await evaluate(argument.value, locals: &locals) }
            return try await renderStatements(function.body, locals: &scope, path: path + ".function")
        }
        guard Self.viewNames.contains(kind) else { throw AppDiagnostic("Unsupported view ‘\(kind)’.") }
        if kind == "ForEach" {
            guard let first = arguments.first, let closure, case .array(let values) = try await evaluate(first.value, locals: &locals) else { throw AppDiagnostic("ForEach requires an array and closure.") }
            var children: [AppNode] = []
            var seen = Set<String>()
            let identityArgument = arguments.first(where: { $0.label == "id" })
            var identity: String?
            if let identityArgument { identity = try await evaluate(identityArgument.value, locals: &locals).text }
            for (index, value) in values.enumerated() {
                var scope = locals; scope[closure.parameters.first ?? "$0"] = value
                var key = String(index)
                if let identity {
                    let idValue: AppValue
                    if identity == "self" { idValue = value }
                    else if case .object(let record) = value, let id = record[identity] { idValue = id }
                    else { throw AppDiagnostic("ForEach record is missing its identity field.") }
                    let encoder = JSONEncoder(); encoder.outputFormatting = .sortedKeys
                    key = try encoder.encode(idValue).base64EncodedString()
                    guard seen.insert(key).inserted else { throw AppDiagnostic("ForEach identities must be unique.") }
                }
                children += try await renderStatements(closure.body, locals: &scope, path: path + "." + key)
            }
            return children
        }
        rendered += 1
        guard rendered <= limits.nodes else { throw AppDiagnostic("View node limit exceeded.") }
        var node = AppNode(id: path, kind: kind)
        var positional: [AppValue] = []
        for argument in arguments {
            if argument.label == "in", case .binary(let op, let lower, let upper) = argument.value, ["...", "..<"].contains(op) {
                node.properties["min"] = try await evaluate(lower, locals: &locals)
                node.properties["max"] = try await evaluate(upper, locals: &locals)
                continue
            }
            if case .binding(let name) = argument.value {
                guard state[name] != nil else { throw AppDiagnostic("Binding ‘\(name)’ is not a state property.") }
                node.binding = name; node.properties["value"] = state[name]; continue
            }
            let value = try await evaluate(argument.value, locals: &locals)
            if let label = argument.label { node.properties[label] = value } else { positional.append(value) }
        }
        node.text = positional.first?.text ?? node.properties["title"]?.text ?? ""
        if let range = node.properties["in"], case .array(let values) = range { node.properties["min"] = values.first; node.properties["max"] = values.last }
        if kind.hasSuffix("Chart"), let first = positional.first { node.properties["values"] = first; node.text = "" }
        if kind == "ScrollView", let first = positional.first { node.properties["axis"] = first }
        if kind == "Image" { node.text = node.properties["systemName"]?.text ?? node.text }
        if let closure {
            if kind == "Button" {
                actionSequence += 1
                let id = "\(path)#\(actionSequence)"
                actions[id] = CapturedAction(closure: closure, locals: locals); node.actionID = id
            } else { var scope = locals; node.children = try await renderStatements(closure.body, locals: &scope, path: path + ".children") }
        }
        return [node]
    }
}
