import Foundation
import SwiftSyntax
import SwiftParser
import SwiftParserDiagnostics
import SwiftOperators

/// Parses the deliberately bounded native app language. No source is executed here.
struct AppSourceParser {
    static func parse(_ source: String) throws -> AppProgram {
        guard source.utf8.count <= 262_144 else { throw AppDiagnostic("Source exceeds the 256 KiB limit") }
        var parser = Parser(source, maximumNestingLevel: 128)
        let tree = SourceFileSyntax.parse(from: &parser)
        let locations = SourceLocationConverter(fileName: "App.swift", tree: tree)
        if let error = ParseDiagnosticsGenerator.diagnostics(for: tree).first(where: { $0.diagMessage.severity == .error }) {
            throw AppDiagnostic(error.message, line: locations.location(for: error.position).line)
        }
        // Bound nesting before recursive lowering, including otherwise valid deeply nested input.
        var pending: [(Syntax, Int)] = [(Syntax(tree), 0)]
        while let (node, depth) = pending.popLast() {
            guard depth <= 128 else { throw AppDiagnostic("Source nesting exceeds 128 levels", line: locations.location(for: node.positionAfterSkippingLeadingTrivia).line) }
            pending.append(contentsOf: node.children(viewMode: .sourceAccurate).map { ($0, depth + 1) })
        }
        let folded = try OperatorTable.standardOperators.foldAll(tree) { error in
            let diagnostic = error.asDiagnostic
            throw AppDiagnostic(diagnostic.message, line: locations.location(for: diagnostic.position).line)
        }
        guard let file = folded.as(SourceFileSyntax.self) else { throw AppDiagnostic("Could not parse source") }
        return try Lowerer(locations: locations).program(file)
    }
}

private final class Lowerer {
    let locations: SourceLocationConverter
    var globals = Set<String>()
    var stateNames = Set<String>()
    var recordFields: [String: [String]] = [:]
    var functionParameters: [String: [String?]] = [:]
    var scopes: [Set<String>] = []
    var mutableScopes: [Set<String>] = []
    var functionDepth = 0
    var closureDepth = 0
    var expressionDepth = 0
    var inViewBuilder = false
    var viewFunctions = Set<String>()
    var readOnlyContext = true
    var currentFunction: String?
    var effectfulFunctions = Set<String>()
    var functionCalls: [String: Set<String>] = [:]
    var readOnlyCalls: [(String, Syntax)] = []

    // Keep this vocabulary aligned with the runtime and native renderer.
    static let charts: Set<String> = ["BarChart", "LineChart", "AreaChart", "PointChart", "PieChart"]
    static let views: Set<String> = ["VStack", "HStack", "ZStack", "Form", "Section", "ScrollView", "List", "Group", "ForEach", "Text", "Label", "Image", "Button", "TextField", "SecureField", "TextEditor", "Toggle", "Stepper", "Slider", "Picker", "Divider", "Spacer", "ProgressView", "Gauge", "BarChart", "LineChart", "AreaChart", "PointChart", "PieChart", "NavigationStack"]
    static let functions: Set<String> = ["String", "Int", "Double", "Bool", "Array", "min", "max", "abs", "round", "Task", "Date", "UUID"]
    static let namespaces: Set<String> = ["Color", "Font", "Alignment", "HorizontalAlignment", "VerticalAlignment", "Agent", "Clock"]
    static let modifiers: Set<String> = ["padding", "font", "fontWeight", "foregroundStyle", "foregroundColor", "tint", "background", "frame", "cornerRadius", "opacity", "disabled", "navigationTitle", "buttonStyle", "listStyle", "textFieldStyle", "lineLimit", "multilineTextAlignment", "tag"]
    static let methods: Set<String> = ["append", "removeLast", "removeAll", "contains", "joined", "reversed", "sorted", "lowercased", "uppercased", "rounded", "trimmingCharacters", "run", "formatted", "today", "dayKey", "map", "filter", "reduce", "prefix", "suffix", "dropFirst"]
    static let properties: Set<String> = ["count", "isEmpty", "first", "last", "uuidString", "timeIntervalSince1970"]
    static let symbols: Set<String> = ["red", "orange", "yellow", "green", "mint", "teal", "cyan", "blue", "indigo", "purple", "pink", "brown", "white", "gray", "black", "clear", "primary", "secondary", "accentColor", "largeTitle", "title", "title2", "title3", "headline", "subheadline", "body", "callout", "footnote", "caption", "caption2", "ultraLight", "thin", "light", "regular", "medium", "semibold", "bold", "heavy", "black", "leading", "trailing", "center", "top", "bottom", "topLeading", "topTrailing", "bottomLeading", "bottomTrailing", "horizontal", "vertical", "all", "automatic", "inline", "large", "plain", "bordered", "borderedProminent", "borderless", "roundedBorder", "decimalPad", "numberPad", "emailAddress", "default", "never", "words", "sentences", "characters", "done", "go", "search", "send", "next", "return", "infinity", "whitespacesAndNewlines", "destructive", "cancel", "firstTextBaseline", "lastTextBaseline", "insetGrouped", "grouped", "sidebar", "number"]

    init(locations: SourceLocationConverter) { self.locations = locations }
    func fail(_ message: String, _ node: some SyntaxProtocol) -> AppDiagnostic {
        AppDiagnostic(message, line: locations.location(for: node.positionAfterSkippingLeadingTrivia).line)
    }
    func checkModifiers(_ modifiers: DeclModifierListSyntax, allowed: Set<String> = ["private", "fileprivate", "internal", "public"]) throws {
        for modifier in modifiers {
            guard allowed.contains(modifier.name.text), modifier.detail == nil else { throw fail("Unsupported modifier '\(modifier.trimmedDescription)'", modifier) }
        }
    }
    func name(_ pattern: PatternSyntax) throws -> String {
        guard let identifier = pattern.as(IdentifierPatternSyntax.self) else { throw fail("Only named bindings are supported", pattern) }
        return identifier.identifier.text
    }
    func reserve(_ value: String, _ node: some SyntaxProtocol) throws {
        guard !globals.contains(value), !Self.views.contains(value), !Self.functions.contains(value), !Self.namespaces.contains(value) else { throw fail("Duplicate or reserved declaration '\(value)'", node) }
        globals.insert(value)
    }
    func type(_ node: TypeSyntax) throws {
        if let array = node.as(ArrayTypeSyntax.self) { try type(array.element); return }
        if let dict = node.as(DictionaryTypeSyntax.self) { try type(dict.key); try type(dict.value); return }
        if let optional = node.as(OptionalTypeSyntax.self) { try type(optional.wrappedType); return }
        if let identifier = node.as(IdentifierTypeSyntax.self) {
            let primitive: Set<String> = ["String", "Int", "Double", "Float", "Bool", "UUID", "Date", "Void", "View"]
            if primitive.contains(identifier.name.text) || recordFields[identifier.name.text] != nil {
                guard identifier.genericArgumentClause == nil else { throw fail("Generic type arguments are unsupported here", node) }; return
            }
        }
        if let opaque = node.as(SomeOrAnyTypeSyntax.self), opaque.someOrAnySpecifier.text == "some", opaque.constraint.trimmedDescription == "View" { return }
        throw fail("Unsupported type '\(node.trimmedDescription)'", node)
    }

    func program(_ file: SourceFileSyntax) throws -> AppProgram {
        var declarations: [StructDeclSyntax] = []
        for item in file.statements {
            if let imp = item.item.as(ImportDeclSyntax.self) {
                guard imp.attributes.isEmpty, imp.modifiers.isEmpty, imp.importKindSpecifier == nil,
                      ["SwiftUI", "Foundation"].contains(imp.path.trimmedDescription) else { throw fail("Only import SwiftUI and import Foundation are supported", imp) }
            } else if let structure = item.item.as(StructDeclSyntax.self) {
                try checkModifiers(structure.modifiers)
                guard structure.attributes.isEmpty, structure.genericParameterClause == nil, structure.genericWhereClause == nil else { throw fail("Struct attributes and generics are unsupported", structure) }
                declarations.append(structure)
            } else { throw fail("Only imports and struct declarations are allowed at file scope", item) }
        }
        let views = declarations.filter { $0.inheritanceClause?.inheritedTypes.contains(where: { $0.type.trimmedDescription == "View" }) == true }
        guard views.count == 1, let view = views.first else { throw fail("Declare exactly one struct conforming to View", file) }
        var result = AppProgram()
        try reserve(view.name.text, view)
        for structure in declarations where structure.id != view.id {
            let record = structure.name.text
            try reserve(record, structure)
            if let inheritance = structure.inheritanceClause {
                for inherited in inheritance.inheritedTypes {
                    guard ["Identifiable", "Codable", "Equatable", "Hashable", "Sendable"].contains(inherited.type.trimmedDescription) else { throw fail("Unsupported record conformance", inherited) }
                }
            }
            var fields: [String] = []
            for member in structure.memberBlock.members {
                guard let variable = member.decl.as(VariableDeclSyntax.self), variable.attributes.isEmpty else { throw fail("Records support stored fields only", member) }
                try checkModifiers(variable.modifiers)
                for binding in variable.bindings {
                    let field = try name(binding.pattern)
                    guard !fields.contains(field), binding.accessorBlock == nil, binding.initializer == nil, binding.typeAnnotation != nil else { throw fail("Record fields need a type, unique name, and no initializer or accessor", binding) }
                    fields.append(field)
                }
            }
            guard !fields.isEmpty else { throw fail("A record must declare at least one field", structure) }
            recordFields[record] = fields
        }
        for structure in declarations where structure.id != view.id {
            for member in structure.memberBlock.members {
                for binding in member.decl.cast(VariableDeclSyntax.self).bindings { try type(binding.typeAnnotation!.type) }
            }
        }
        guard view.inheritanceClause?.inheritedTypes.count == 1 else { throw fail("The app struct may conform only to View", view) }
        var bodyCount = 0
        var persistedKeys = Set<String>()
        for member in view.memberBlock.members {
            if let variable = member.decl.as(VariableDeclSyntax.self) {
                for binding in variable.bindings {
                    let identifier = try name(binding.pattern)
                    if identifier == "body" { bodyCount += 1 } else { try reserve(identifier, binding) }
                    if !variable.attributes.isEmpty { stateNames.insert(identifier) }
                }
            } else if let function = member.decl.as(FunctionDeclSyntax.self) {
                let identifier = function.name.text
                try reserve(identifier, function)
                if function.signature.returnClause?.type.trimmedDescription == "some View" { viewFunctions.insert(identifier) }
                functionParameters[identifier] = function.signature.parameterClause.parameters.map { $0.firstName.text == "_" ? nil : $0.firstName.text }
            } else { throw fail("Unsupported View member declaration", member) }
        }
        guard bodyCount == 1 else { throw fail("The View must declare exactly one body", view) }
        for member in view.memberBlock.members {
            if let variable = member.decl.as(VariableDeclSyntax.self) {
                try checkModifiers(variable.modifiers)
                let wrapper = try stateWrapper(variable)
                if let key = wrapper.key, !persistedKeys.insert(key).inserted { throw fail("Duplicate persisted key '\(key)'", variable) }
                for binding in variable.bindings {
                    let identifier = try name(binding.pattern)
                    if identifier == "body" {
                        guard variable.bindingSpecifier.text == "var", variable.attributes.isEmpty, variable.bindings.count == 1,
                              binding.initializer == nil, let annotation = binding.typeAnnotation, annotation.type.trimmedDescription == "some View",
                              let accessor = binding.accessorBlock, let statements = accessor.accessors.as(CodeBlockItemListSyntax.self) else { throw fail("body must be declared as var body: some View { ... }", binding) }
                        result.body = try block(statements, isBuilder: true)
                    } else {
                        guard binding.accessorBlock == nil, let initializer = binding.initializer else { throw fail("Stored values require an initializer", binding) }
                        if let annotation = binding.typeAnnotation { try type(annotation.type) }
                        if wrapper.present {
                            guard variable.bindingSpecifier.text == "var" else { throw fail("State must use var", variable) }
                            result.states.append(StateDeclaration(name: identifier, initial: try expression(initializer.value), persistedKey: wrapper.key))
                        } else {
                            guard variable.bindingSpecifier.text == "let" else { throw fail("Mutable app fields require @State or @Persisted", variable) }
                            result.constants[identifier] = try expression(initializer.value)
                        }
                    }
                }
            } else if let function = member.decl.as(FunctionDeclSyntax.self) {
                try checkModifiers(function.modifiers)
                guard function.attributes.isEmpty, function.genericParameterClause == nil, function.genericWhereClause == nil, let body = function.body else { throw fail("Function attributes, generics, and declarations without bodies are unsupported", function) }
                var parameters: [String] = []
                for parameter in function.signature.parameterClause.parameters {
                    guard parameter.attributes.isEmpty, parameter.modifiers.isEmpty, parameter.ellipsis == nil, parameter.defaultValue == nil else { throw fail("Parameter attributes, modifiers, variadics, and defaults are unsupported", parameter) }
                    try type(parameter.type)
                    let identifier = (parameter.secondName ?? parameter.firstName).text
                    guard identifier != "_", !parameters.contains(identifier) else { throw fail("Function parameters need unique local names", parameter) }
                    parameters.append(identifier)
                }
                if let returnType = function.signature.returnClause { try type(returnType.type) }
                if let effects = function.signature.effectSpecifiers {
                    guard effects.asyncSpecifier == nil || effects.asyncSpecifier?.text == "async", effects.throwsClause == nil || effects.throwsClause?.trimmedDescription == "throws" else { throw fail("Only async and throws function effects are supported", effects) }
                }
                functionDepth += 1
                currentFunction = function.name.text
                readOnlyContext = viewFunctions.contains(function.name.text)
                var lowered = try block(body.statements, parameters: parameters, isBuilder: viewFunctions.contains(function.name.text))
                functionDepth -= 1
                currentFunction = nil
                readOnlyContext = true
                if !viewFunctions.contains(function.name.text), function.signature.returnClause != nil,
                   lowered.count == 1, case .expression(let value) = lowered[0] { lowered = [.returnValue(value)] }
                result.functions[function.name.text] = FunctionDeclaration(parameters: parameters, body: lowered)
            }
        }
        result.records = recordFields
        var changed = true
        while changed {
            changed = false
            for (caller, callees) in functionCalls where !effectfulFunctions.contains(caller) && !callees.isDisjoint(with: effectfulFunctions) {
                effectfulFunctions.insert(caller); changed = true
            }
        }
        for (name, node) in readOnlyCalls where effectfulFunctions.contains(name) {
            throw fail("Function '\(name)' performs actions and cannot run in initializers or view builders", node)
        }
        return result
    }

    func stateWrapper(_ variable: VariableDeclSyntax) throws -> (present: Bool, key: String?) {
        guard !variable.attributes.isEmpty else { return (false, nil) }
        guard variable.attributes.count == 1, let attribute = variable.attributes.first?.as(AttributeSyntax.self), variable.bindings.count == 1 else { throw fail("Use one @State or @Persisted wrapper per field", variable) }
        switch attribute.attributeName.trimmedDescription {
        case "State":
            guard attribute.arguments == nil else { throw fail("@State does not accept arguments", attribute) }
            return (true, nil)
        case "Persisted":
            guard let arguments = attribute.arguments?.as(LabeledExprListSyntax.self), arguments.count == 1, let argument = arguments.first, argument.label == nil,
                  let literal = argument.expression.as(StringLiteralExprSyntax.self), let key = literal.representedLiteralValue, !key.isEmpty else { throw fail("@Persisted requires one nonempty literal key", attribute) }
            return (true, key)
        default: throw fail("Unsupported property wrapper '\(attribute.attributeName.trimmedDescription)'", attribute)
        }
    }

    func block(_ items: CodeBlockItemListSyntax, parameters: [String] = [], isBuilder: Bool? = nil) throws -> [Statement] {
        let previousBuilder = inViewBuilder
        if let isBuilder { inViewBuilder = isBuilder }
        defer { inViewBuilder = previousBuilder }
        scopes.append(Set(parameters)); mutableScopes.append([])
        defer { scopes.removeLast(); mutableScopes.removeLast() }
        var result: [Statement] = []
        for item in items {
            if let variable = item.item.as(VariableDeclSyntax.self) {
                guard variable.attributes.isEmpty, variable.modifiers.isEmpty else { throw fail("Local declarations cannot have attributes or modifiers", variable) }
                for binding in variable.bindings {
                    let identifier = try name(binding.pattern)
                    guard let initializer = binding.initializer, binding.accessorBlock == nil, !scopes[scopes.count - 1].contains(identifier) else { throw fail("Local bindings require a unique name and initializer", binding) }
                    if let annotation = binding.typeAnnotation { try type(annotation.type) }
                    let value = try expression(initializer.value)
                    scopes[scopes.count - 1].insert(identifier)
                    if variable.bindingSpecifier.text == "var" { mutableScopes[mutableScopes.count - 1].insert(identifier) }
                    result.append(.variable(identifier, value))
                }
            } else if let loop = item.item.as(ForStmtSyntax.self) {
                guard loop.tryKeyword == nil, loop.awaitKeyword == nil, loop.caseKeyword == nil, loop.unsafeKeyword == nil, loop.whereClause == nil, loop.typeAnnotation == nil else { throw fail("Only for name in collection loops are supported", loop) }
                let identifier = try name(loop.pattern)
                result.append(.forEach(identifier, try expression(loop.sequence), try block(loop.body.statements, parameters: [identifier])))
            } else if let loop = item.item.as(WhileStmtSyntax.self) {
                guard !inViewBuilder else { throw fail("while loops are allowed only in actions and value functions", loop) }
                result.append(.whileLoop(try condition(loop.conditions), try block(loop.body.statements)))
            } else if let returned = item.item.as(ReturnStmtSyntax.self) {
                guard functionDepth > 0 || closureDepth > 0 else { throw fail("return is allowed only inside functions and closures", returned) }
                if inViewBuilder, let value = returned.expression { try requireView(value) }
                result.append(.returnValue(try returned.expression.map(expression)))
            } else if let expr = item.item.as(ExprSyntax.self) ?? item.item.as(ExpressionStmtSyntax.self)?.expression {
                if let branch = expr.as(IfExprSyntax.self) { result.append(try conditional(branch)) }
                else if let binary = expr.as(InfixOperatorExprSyntax.self), Self.assignmentOperators.contains(binary.operator.trimmedDescription) {
                    guard !inViewBuilder else { throw fail("Assignments are not allowed in view builders", binary) }
                    try assignmentTarget(binary.leftOperand)
                    result.append(.assign(try expression(binary.leftOperand), binary.operator.trimmedDescription, try expression(binary.rightOperand)))
                } else {
                    if inViewBuilder { try requireView(expr) }
                    result.append(.expression(try expression(expr)))
                }
            } else { throw fail("Unsupported statement '\(item.item.kind)'", item) }
        }
        return result
    }
    static let assignmentOperators: Set<String> = ["=", "+=", "-=", "*=", "/=", "%="]
    func assignmentTarget(_ node: ExprSyntax) throws {
        if let variable = node.as(DeclReferenceExprSyntax.self) {
            let identifier = variable.baseName.text
            for index in scopes.indices.reversed() where scopes[index].contains(identifier) {
                guard mutableScopes[index].contains(identifier) else { throw fail("Cannot mutate immutable binding '\(identifier)'", node) }; return
            }
            guard stateNames.contains(identifier) else { throw fail("Assignment requires mutable state or a local var", node) }; try effect(node); return
        }
        if let member = node.as(MemberAccessExprSyntax.self), let base = member.base {
            if base.trimmedDescription == "self" {
                let field = member.declName.baseName.text
                guard stateNames.contains(field) else { throw fail("Assignment requires mutable state", member) }
                try effect(member); return
            }
            try assignmentTarget(base); return
        }
        if let index = node.as(SubscriptCallExprSyntax.self) { try assignmentTarget(index.calledExpression); return }
        throw fail("Unsupported assignment target", node)
    }
    func condition(_ conditions: ConditionElementListSyntax) throws -> Expr {
        var combined: Expr?
        for element in conditions {
            guard let expr = element.condition.as(ExprSyntax.self) else { throw fail("Conditions must be boolean expressions; optional binding is unsupported", element) }
            let lowered = try expression(expr)
            combined = combined.map { .binary("&&", $0, lowered) } ?? lowered
        }
        guard let combined else { throw fail("Missing condition", conditions) }
        return combined
    }
    func conditional(_ branch: IfExprSyntax) throws -> Statement {
        var alternative: [Statement] = []
        if let other = branch.elseBody {
            if let nested = other.as(IfExprSyntax.self) { alternative = [try conditional(nested)] }
            else if let block = other.as(CodeBlockSyntax.self) { alternative = try self.block(block.statements) }
            else { throw fail("Unsupported else body", other) }
        }
        return .condition(try condition(branch.conditions), try block(branch.body.statements), alternative)
    }
    func closure(_ node: ClosureExprSyntax, isBuilder: Bool, arity: Int, action: Bool = false, implicitReturn: Bool = false) throws -> Closure {
        var parameters: [String] = []
        if let signature = node.signature {
            guard signature.attributes.isEmpty, signature.capture == nil, signature.effectSpecifiers == nil, signature.returnClause == nil else { throw fail("Closure capture lists, attributes, effects, and return annotations are unsupported", signature) }
            if let simple = signature.parameterClause?.as(ClosureShorthandParameterListSyntax.self) { parameters = simple.map { $0.name.text } }
            else if let clause = signature.parameterClause?.as(ClosureParameterClauseSyntax.self) {
                for parameter in clause.parameters {
                    guard parameter.attributes.isEmpty, parameter.modifiers.isEmpty, parameter.ellipsis == nil, parameter.secondName == nil else { throw fail("Unsupported closure parameter", parameter) }
                    if let annotation = parameter.type { try type(annotation) }
                    parameters.append(parameter.firstName.text)
                }
            }
            guard parameters.count == Set(parameters).count else { throw fail("Closure parameters must be unique", signature) }
        }
        guard parameters.isEmpty || parameters.count == arity else { throw fail("Closure expects \(arity) parameters", node) }
        let previousReadOnly = readOnlyContext, previousFunction = currentFunction
        if action { readOnlyContext = false; currentFunction = nil }
        closureDepth += 1
        defer { closureDepth -= 1; readOnlyContext = previousReadOnly; currentFunction = previousFunction }
        let implicit = (0..<arity).map { "$\($0)" }
        var statements = try block(node.statements, parameters: parameters.isEmpty ? implicit : parameters, isBuilder: isBuilder)
        if implicitReturn, statements.count == 1, case .expression(let value) = statements[0] { statements = [.returnValue(value)] }
        return Closure(parameters: parameters, body: statements)
    }

    func expression(_ node: ExprSyntax) throws -> Expr {
        expressionDepth += 1
        defer { expressionDepth -= 1 }
        guard expressionDepth <= 128 else { throw fail("Expression nesting exceeds 128 levels", node) }
        if let integer = node.as(IntegerLiteralExprSyntax.self) {
            guard let value = integer.representedLiteralValue else { throw fail("Integer literal is out of range", node) }
            return .value(.number(Double(value)))
        }
        if let floating = node.as(FloatLiteralExprSyntax.self) {
            guard let value = floating.representedLiteralValue, value.isFinite else { throw fail("Invalid or non-finite number", node) }
            return .value(.number(value))
        }
        if let boolean = node.as(BooleanLiteralExprSyntax.self) { return .value(.bool(boolean.literal.text == "true")) }
        if node.is(NilLiteralExprSyntax.self) { return .value(.null) }
        if let string = node.as(StringLiteralExprSyntax.self) { return try stringExpression(string) }
        if let reference = node.as(DeclReferenceExprSyntax.self) {
            guard reference.argumentNames == nil else { throw fail("Function references with argument labels are unsupported", node) }
            let identifier = reference.baseName.text
            if identifier.hasPrefix("$") && !identifier.dropFirst().allSatisfy(\.isNumber) {
                let state = String(identifier.dropFirst())
                guard stateNames.contains(state) else { throw fail("Binding '\(identifier)' must refer to app state", node) }
                return .binding(state)
            }
            guard globals.contains(identifier) || scopes.contains(where: { $0.contains(identifier) }) || Self.namespaces.contains(identifier) || Self.functions.contains(identifier) || Self.views.contains(identifier) else { throw fail("Unknown name '\(identifier)'", node) }
            return .variable(identifier)
        }
        if let tuple = node.as(TupleExprSyntax.self), tuple.elements.count == 1, let element = tuple.elements.first, element.label == nil { return try expression(element.expression) }
        if let array = node.as(ArrayExprSyntax.self) { return .array(try array.elements.map { try expression($0.expression) }) }
        if let dictionary = node.as(DictionaryExprSyntax.self) {
            if let elements = dictionary.content.as(DictionaryElementListSyntax.self) { return .dictionary(try elements.map { (try expression($0.key), try expression($0.value)) }) }
            return .dictionary([])
        }
        if let prefix = node.as(PrefixOperatorExprSyntax.self) {
            guard ["!", "-", "+"].contains(prefix.operator.text) else { throw fail("Unsupported prefix operator", prefix) }
            return .unary(prefix.operator.text, try expression(prefix.expression))
        }
        if let binary = node.as(InfixOperatorExprSyntax.self) {
            let op = binary.operator.trimmedDescription
            guard ["+", "-", "*", "/", "%", "==", "!=", "<", ">", "<=", ">=", "&&", "||", "??", "...", "..<"].contains(op) else { throw fail("Unsupported expression operator '\(op)'", binary.operator) }
            return .binary(op, try expression(binary.leftOperand), try expression(binary.rightOperand))
        }
        if let ternary = node.as(TernaryExprSyntax.self) { return .conditional(try expression(ternary.condition), try expression(ternary.thenExpression), try expression(ternary.elseExpression)) }
        if let awaitExpr = node.as(AwaitExprSyntax.self) { return .awaitValue(try expression(awaitExpr.expression)) }
        if let tryExpr = node.as(TryExprSyntax.self) {
            guard tryExpr.questionOrExclamationMark == nil else { throw fail("try? and try! are unsupported; use try", node) }
            return try expression(tryExpr.expression)
        }
        if let member = node.as(MemberAccessExprSyntax.self) { return try memberExpression(member) }
        if let subscriptCall = node.as(SubscriptCallExprSyntax.self) {
            guard subscriptCall.arguments.count == 1, let argument = subscriptCall.arguments.first, argument.label == nil, subscriptCall.trailingClosure == nil, subscriptCall.additionalTrailingClosures.isEmpty else { throw fail("Subscripts require one unlabeled index", node) }
            return .index(try expression(subscriptCall.calledExpression), try expression(argument.expression))
        }
        if let call = node.as(FunctionCallExprSyntax.self) { return try callExpression(call) }
        if let path = node.as(KeyPathExprSyntax.self) {
            let value = path.trimmedDescription
            guard value == "\\.self" || value == "\\.id" else { throw fail("Only \\.self and \\.id key paths are supported", node) }
            return .value(.string(String(value.dropFirst(2))))
        }
        throw fail("Unsupported expression '\(node.kind)'", node)
    }
    func memberExpression(_ member: MemberAccessExprSyntax) throws -> Expr {
        guard member.declName.argumentNames == nil else { throw fail("Labeled member references are unsupported", member) }
        let name = member.declName.baseName.text
        if let base = member.base {
            if base.trimmedDescription == "self" {
                guard globals.contains(name), functionParameters[name] == nil, recordFields[name] == nil else { throw fail("Unknown stored property '\(name)'", member) }
                return .variable(name)
            }
            if let namespace = base.as(DeclReferenceExprSyntax.self), ["Color", "Font", "Alignment", "HorizontalAlignment", "VerticalAlignment"].contains(namespace.baseName.text), Self.symbols.contains(name) { return .value(.string(name)) }
            guard Self.properties.contains(name) || recordFields.values.contains(where: { $0.contains(name) }) else { throw fail("Unsupported property '\(name)'", member) }
            return .member(try expression(base), name)
        }
        guard Self.symbols.contains(name) else { throw fail("Unsupported implicit member '.\(name)'", member) }
        return .value(.string(name))
    }
    func callExpression(_ call: FunctionCallExprSyntax) throws -> Expr {
        guard call.additionalTrailingClosures.isEmpty else { throw fail("Multiple trailing closures are unsupported; use a title and one action closure", call) }
        let callee: Expr
        var builderClosure = false
        var closureArity = 0
        var actionClosure = false
        var returnsImplicitly = false
        let selfMember = call.calledExpression.as(MemberAccessExprSyntax.self).flatMap { $0.base?.trimmedDescription == "self" ? $0 : nil }
        if let reference = call.calledExpression.as(DeclReferenceExprSyntax.self) ?? selfMember?.declName {
            let name = reference.baseName.text
            if selfMember != nil, functionParameters[name] == nil { throw fail("Unknown app function '\(name)'", call) }
            guard reference.argumentNames == nil, Self.views.contains(name) || Self.functions.contains(name) || functionParameters[name] != nil || recordFields[name] != nil else { throw fail("Unsupported function or view '\(name)'", reference) }
            if Self.views.contains(name) || viewFunctions.contains(name), !inViewBuilder { throw fail("Views may be constructed only in view builders", call) }
            if Self.views.contains(name) || Self.functions.contains(name) { try callShape(name, call) }
            builderClosure = Self.views.contains(name) && name != "Button"
            closureArity = name == "ForEach" ? 1 : 0
            actionClosure = name == "Button"
            if name == "Task" { try effect(call) }
            if functionParameters[name] != nil {
                if let caller = currentFunction { functionCalls[caller, default: []].insert(name) }
                if readOnlyContext { readOnlyCalls.append((name, Syntax(call))) }
            }
            if let fields = recordFields[name] {
                guard call.trailingClosure == nil, call.arguments.map({ $0.label?.text }) == fields.map(Optional.some) else { throw fail("\(name) requires labeled arguments in field order: \(fields.joined(separator: ", "))", call) }
            }
            if let labels = functionParameters[name] {
                guard call.trailingClosure == nil, call.arguments.map({ $0.label?.text }) == labels else { throw fail("Arguments do not match function '\(name)'", call) }
            }
            callee = .variable(name)
        } else if let member = call.calledExpression.as(MemberAccessExprSyntax.self), let base = member.base {
            let name = member.declName.baseName.text
            guard member.declName.argumentNames == nil, Self.modifiers.contains(name) || Self.methods.contains(name) else { throw fail("Unsupported method or view modifier '\(name)'", member) }
            if let namespace = base.as(DeclReferenceExprSyntax.self), Self.namespaces.contains(namespace.baseName.text) {
                let allowed = namespace.baseName.text == "Agent" ? name == "run" : namespace.baseName.text == "Clock" && ["today", "dayKey"].contains(name)
                guard allowed else { throw fail("Unsupported namespace call", member) }
            }
            if Self.modifiers.contains(name) { try requireView(base) }
            else if call.trailingClosure != nil, !["map", "filter", "reduce"].contains(name) { throw fail("This method does not support a closure", call) }
            closureArity = name == "reduce" ? 2 : (["map", "filter"].contains(name) ? 1 : 0)
            returnsImplicitly = ["map", "filter", "reduce"].contains(name)
            try callShape(name, call)
            if ["append", "removeLast", "removeAll"].contains(name) { try assignmentTarget(base) }
            if name == "run" {
                try effect(call)
                guard base.trimmedDescription == "Agent" else { throw fail("run must be called on Agent", base) }
            }
            if ["today", "dayKey"].contains(name), base.trimmedDescription != "Clock" { throw fail("\(name) must be called on Clock", base) }
            callee = .member(try expression(base), name)
        } else { throw fail("Only named functions and supported member calls are allowed", call.calledExpression) }
        let arguments = try call.arguments.map { argument -> Argument in
            guard !argument.expression.is(ClosureExprSyntax.self) else { throw fail("Use a single trailing closure", argument) }
            return Argument(label: argument.label?.text, value: try expression(argument.expression))
        }
        return .call(callee, arguments, try call.trailingClosure.map { try closure($0, isBuilder: builderClosure, arity: closureArity, action: actionClosure, implicitReturn: returnsImplicitly) })
    }
    func effect(_ node: some SyntaxProtocol) throws {
        guard !readOnlyContext else { throw fail("Actions cannot run in state initializers, constants, or view builders", node) }
        if let currentFunction { effectfulFunctions.insert(currentFunction) }
    }
    func requireView(_ node: ExprSyntax) throws {
        guard let call = node.as(FunctionCallExprSyntax.self) else { throw fail("Expected a view expression", node) }
        if let reference = call.calledExpression.as(DeclReferenceExprSyntax.self), Self.views.contains(reference.baseName.text) || viewFunctions.contains(reference.baseName.text) { return }
        if let member = call.calledExpression.as(MemberAccessExprSyntax.self), let base = member.base, Self.modifiers.contains(member.declName.baseName.text) { try requireView(base); return }
        if let member = call.calledExpression.as(MemberAccessExprSyntax.self) {
            if member.base?.trimmedDescription == "self", viewFunctions.contains(member.declName.baseName.text) { return }
            throw fail("Unsupported view modifier '\(member.declName.baseName.text)'", member)
        }
        throw fail("Expected a supported view or a function returning some View: '\(call.calledExpression.trimmedDescription)'", node)
    }
    func callShape(_ name: String, _ call: FunctionCallExprSyntax) throws {
        let allowed: [String: Set<String>] = [
            "VStack": ["alignment", "spacing"], "HStack": ["alignment", "spacing"], "ZStack": ["alignment"],
            "Form": [], "Section": [], "ScrollView": ["showsIndicators"], "List": [], "Group": [], "ForEach": ["id"],
            "Text": [], "Label": ["systemImage"], "Image": ["systemName"], "Button": ["role"],
            "TextField": ["text", "value", "format"], "SecureField": ["text"], "TextEditor": ["text"], "Toggle": ["isOn"],
            "Stepper": ["value", "in", "step"], "Slider": ["value", "in", "step"], "Picker": ["selection"],
            "Divider": [], "Spacer": ["minLength"], "ProgressView": ["value", "total"], "Gauge": ["value", "in"], "BarChart": ["title"], "LineChart": ["title"], "AreaChart": ["title"], "PointChart": ["title"], "PieChart": ["title"], "NavigationStack": [],
            "padding": [], "font": [], "fontWeight": [], "foregroundStyle": [], "foregroundColor": [], "tint": [], "background": [],
            "frame": ["width", "height", "minWidth", "idealWidth", "maxWidth", "minHeight", "idealHeight", "maxHeight", "alignment"],
            "cornerRadius": [], "opacity": [], "disabled": [], "navigationTitle": [], "buttonStyle": [], "listStyle": [], "textFieldStyle": [],
            "lineLimit": [], "multilineTextAlignment": [], "tag": [], "String": [], "Int": [], "Double": [], "Bool": [], "Array": [],
            "min": [], "max": [], "abs": [], "round": [], "Task": [], "append": [], "removeLast": [], "removeAll": [], "contains": [],
            "prefix": [], "suffix": [], "dropFirst": [], "joined": ["separator"], "reversed": [], "sorted": [], "lowercased": [], "uppercased": [], "rounded": [], "trimmingCharacters": ["in"], "run": [],
            "Date": ["timeIntervalSince1970"], "UUID": [], "formatted": [], "today": [], "dayKey": [], "map": [], "filter": [], "reduce": []
        ]
        guard let labels = allowed[name] else { throw fail("Unsupported call '\(name)'", call) }
        var seen = Set<String>()
        for argument in call.arguments {
            if let label = argument.label?.text {
                guard labels.contains(label), seen.insert(label).inserted else { throw fail("Unsupported or repeated argument '\(label)' for \(name)", argument) }
            }
        }
        let containers: Set<String> = ["VStack", "HStack", "ZStack", "Form", "Section", "ScrollView", "List", "Group", "ForEach", "Picker", "NavigationStack", "Button", "Task", "map", "filter", "reduce"]
        if containers.contains(name) {
            guard call.trailingClosure != nil else { throw fail("\(name) requires a trailing closure", call) }
        } else if call.trailingClosure != nil { throw fail("\(name) does not support a trailing closure", call) }
        let noArguments: Set<String> = ["Task", "Divider", "Form", "List", "Group", "NavigationStack", "removeLast", "removeAll", "reversed", "sorted", "lowercased", "uppercased", "rounded", "UUID", "formatted", "today", "map", "filter"]
        if noArguments.contains(name), !call.arguments.isEmpty { throw fail("\(name) does not accept arguments", call) }
        let exactlyOne: Set<String> = ["String", "Int", "Double", "Bool", "Array", "abs", "round", "append", "contains", "trimmingCharacters", "run", "Text", "Image", "TextEditor", "dayKey", "reduce", "prefix", "suffix", "dropFirst"]
        if exactlyOne.contains(name), call.arguments.count != 1 { throw fail("\(name) requires one argument", call) }
        if ["min", "max"].contains(name), call.arguments.count < 2 { throw fail("\(name) requires at least two arguments", call) }
        if Self.modifiers.contains(name), !["padding", "frame"].contains(name), call.arguments.count != 1 { throw fail("\(name) requires one argument", call) }
        if name == "Date", call.arguments.count > 1 || (call.arguments.count == 1 && call.arguments.first?.label?.text != "timeIntervalSince1970") { throw fail("Date accepts no arguments or timeIntervalSince1970:", call) }
        if name == "padding", call.arguments.count > 2 { throw fail("padding accepts at most two arguments", call) }
        if name == "ForEach", !(1...2).contains(call.arguments.count) { throw fail("ForEach requires a collection and optional id", call) }
        if name == "Button", !(1...2).contains(call.arguments.count) { throw fail("Button requires a title and optional role", call) }
        let argumentLabels = call.arguments.map { $0.label?.text }
        let labeledOnly: Set<String> = ["VStack", "HStack", "ZStack", "Image", "TextEditor", "Slider", "Gauge", "Spacer", "frame"]
        if labeledOnly.contains(name), argumentLabels.contains(nil) { throw fail("\(name) requires labeled arguments", call) }
        let oneTitle: Set<String> = ["Text", "BarChart", "LineChart", "AreaChart", "PointChart", "PieChart", "Button", "Label", "TextField", "SecureField", "Toggle", "Stepper", "Picker", "ForEach"]
        if oneTitle.contains(name), argumentLabels.first != .some(nil) { throw fail("\(name) requires an unlabeled first argument", call) }
        let titleAllowed = oneTitle.union(["Section", "ProgressView", "ScrollView"])
        if titleAllowed.contains(name), argumentLabels.dropFirst().contains(nil) { throw fail("Unexpected positional argument for \(name)", call) }
        if Self.charts.contains(name), !(1...2).contains(call.arguments.count) || (call.arguments.count == 2 && argumentLabels.last != "title") { throw fail("\(name) requires data and an optional title:", call) }
        if name == "Section", call.arguments.count > 1 { throw fail("Section accepts only a title", call) }
        if name == "ScrollView", call.arguments.count > 2 { throw fail("ScrollView accepts axes and showsIndicators", call) }
        if name == "Label", argumentLabels != [nil, "systemImage"] { throw fail("Label requires a title and systemImage:", call) }
        if name == "joined", !(argumentLabels.isEmpty || argumentLabels == ["separator"]) { throw fail("joined accepts only separator:", call) }
        if name == "trimmingCharacters" {
            guard argumentLabels == ["in"], call.arguments.first?.expression.trimmedDescription == ".whitespacesAndNewlines" else { throw fail("Only trimmingCharacters(in: .whitespacesAndNewlines) is supported", call) }
        }
        if name == "ForEach", let id = call.arguments.first(where: { $0.label?.text == "id" }), !id.expression.is(KeyPathExprSyntax.self) { throw fail("ForEach id must be \\.self or \\.id", id) }
        let bindings: [String: Set<String>] = ["TextField": ["text", "value"], "SecureField": ["text"], "TextEditor": ["text"], "Toggle": ["isOn"], "Stepper": ["value"], "Slider": ["value"], "Picker": ["selection"]]
        if let expected = bindings[name] {
            let matching = call.arguments.filter { expected.contains($0.label?.text ?? "") }
            guard matching.count == 1, let binding = matching.first?.expression.as(DeclReferenceExprSyntax.self), binding.baseName.text.hasPrefix("$") else { throw fail("\(name) requires one state binding", call) }
        }

    }
    func stringExpression(_ string: StringLiteralExprSyntax) throws -> Expr {
        if let value = string.representedLiteralValue { return .value(.string(value)) }
        // SwiftParser performs escape and multiline-indent decoding. Decode text segments
        // with the same literal delimiters, retaining interpolation expressions separately.
        var parts: [Expr] = []
        for segment in string.segments {
            if let text = segment.as(StringSegmentSyntax.self) {
                var literal = string
                literal.segments = StringLiteralSegmentListSyntax([.stringSegment(text)])
                guard let value = literal.representedLiteralValue else { throw fail("Unsupported string literal segment", segment) }
                parts.append(.value(.string(value)))
            } else if let interpolation = segment.as(ExpressionSegmentSyntax.self) {
                guard interpolation.expressions.count == 1, let value = interpolation.expressions.first, value.label == nil else { throw fail("String interpolation accepts one expression without formatting arguments", interpolation) }
                parts.append(try expression(value.expression))
            } else { throw fail("Unsupported string segment", segment) }
        }
        return .interpolation(parts)
    }
}
