#if canImport(SwiftUI)
import SwiftUI
import Charts

// Match the Inbox's adaptive system surface and neutral accent without importing
// the chat renderer. App-authored modifiers remain local overrides of these defaults.
private enum NativeAppAppearance {
    static var background: Color {
        #if os(macOS)
        Color(nsColor: .windowBackgroundColor)
        #else
        Color(uiColor: .systemBackground)
        #endif
    }
}

/// Renders the app's evaluated view tree using platform-native controls.
@MainActor
public struct NativeAppView: View {
    @ObservedObject public var session: NativeAppSession
    private let background: Color
    private struct PendingBinding { var id: UUID; var value: AppValue }
    @State private var pendingBindings: [String: PendingBinding] = [:]

    public init(session: NativeAppSession, background: Color? = nil) {
        self.session = session
        self.background = background ?? NativeAppAppearance.background
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let diagnostic = session.diagnostic {
                VStack(alignment: .leading, spacing: 6) {
                    Label("Something needs attention", systemImage: "exclamationmark.circle.fill")
                        .font(.headline)
                    Text(diagnostic.message)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding()
                .background(Color.primary.opacity(0.06))
                .accessibilityElement(children: .combine)
            }
            if session.nodes.isEmpty {
                if session.isBusy {
                    ProgressView("Getting ready…")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if session.diagnostic == nil {
                    ContentUnavailableView("Ready when you are", systemImage: "square.grid.2x2")
                }
            } else {
                children(session.nodes)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .font(.body)
        .tint(.primary)
        .background(background)
        .overlay(alignment: .topTrailing) {
            if session.isBusy && !session.nodes.isEmpty {
                ProgressView()
                    .controlSize(.small)
                    .padding(10)
                    .background(.regularMaterial, in: Capsule())
                    .padding(8)
                    .accessibilityLabel("Updating")
                    .allowsHitTesting(false)
            }
        }
        .task { try? await session.start() }
    }

    private func children(_ nodes: [AppNode]) -> some View {
        ForEach(nodes) { node in render(node) }
    }

    private func render(_ node: AppNode) -> AnyView {
        var view = content(node)
        let props = node.properties
        if let value = props["font"] {
            view = AnyView(view.font(font(value)))
        }
        if let value = props["fontWeight"] {
            view = AnyView(view.fontWeight(weight(value)))
        }
        if let value = props["lineLimit"] {
            let limit: Int? = value == .null ? nil : Int(finite(value.number, fallback: 1).clamped(to: 1...100_000))
            view = AnyView(view.lineLimit(limit))
        }
        if let value = props["multilineTextAlignment"] {
            let alignment: TextAlignment = name(value) == "center" ? .center : name(value) == "trailing" ? .trailing : .leading
            view = AnyView(view.multilineTextAlignment(alignment))
        }
        if let value = props["foregroundColor"] {
            view = AnyView(view.foregroundStyle(color(value)))
        }
        if let value = props["foregroundStyle"] {
            view = AnyView(view.foregroundStyle(color(value)))
        }
        if let value = props["tint"] {
            view = AnyView(view.tint(color(value)))
        }
        if let value = props["padding"] {
            let amount: CGFloat?
            if case .number(let number) = value, number.isFinite {
                amount = CGFloat(max(0, number))
            } else {
                amount = value == .bool(false) ? 0 : nil
            }
            view = AnyView(view.padding(edges(props["paddingEdges"]), amount))
        }
        if let value = props["buttonStyle"] { view = buttonStyle(view, value) }
        if let value = props["listStyle"] { view = listStyle(view, value) }
        if let value = props["textFieldStyle"] { view = textFieldStyle(view, value) }
        let frameAlignment = alignment(props["frameAlignment"])
        let width = dimension(props["frameWidth"])
        let height = dimension(props["frameHeight"])
        if width != nil || height != nil {
            view = AnyView(view.frame(width: width, height: height, alignment: frameAlignment))
        }
        if props["frameWidth"]?.text.contains("infinity") == true {
            view = AnyView(view.frame(maxWidth: .infinity))
        }
        if props["frameHeight"]?.text.contains("infinity") == true {
            view = AnyView(view.frame(maxHeight: .infinity))
        }
        if ["frameMinWidth", "frameIdealWidth", "frameMaxWidth", "frameMinHeight", "frameIdealHeight", "frameMaxHeight", "frameAlignment"].contains(where: { props[$0] != nil }) {
            view = AnyView(view.frame(
                minWidth: dimension(props["frameMinWidth"]),
                idealWidth: dimension(props["frameIdealWidth"]),
                maxWidth: maximumDimension(props["frameMaxWidth"]),
                minHeight: dimension(props["frameMinHeight"]),
                idealHeight: dimension(props["frameIdealHeight"]),
                maxHeight: maximumDimension(props["frameMaxHeight"]),
                alignment: frameAlignment
            ))
        }
        if let value = props["background"] {
            view = AnyView(view.background(color(value)))
        }
        if let radius = dimension(props["cornerRadius"]) {
            view = AnyView(view.clipShape(RoundedRectangle(cornerRadius: radius, style: .continuous)))
        }
        if let value = props["opacity"] {
            view = AnyView(view.opacity(finite(value.number, fallback: 1).clamped(to: 0...1)))
        }
        if let value = props["disabled"] {
            view = AnyView(view.disabled(value.truth))
        }
        if let value = props["navigationTitle"] {
            view = AnyView(view.navigationTitle(value.text))
        }
        return view
    }

    private func content(_ node: AppNode) -> AnyView {
        let props = node.properties
        let title = label(node)
        switch node.kind {
        case "NavigationStack":
            return AnyView(NavigationStack { children(node.children) })
        case "VStack":
            return AnyView(VStack(alignment: horizontal(props["alignment"]), spacing: spacing(node)) {
                children(node.children)
            })
        case "HStack":
            return AnyView(HStack(alignment: vertical(props["alignment"]), spacing: spacing(node)) {
                children(node.children)
            })
        case "ZStack":
            return AnyView(ZStack(alignment: alignment(props["alignment"])) { children(node.children) })
        case "Form":
            return AnyView(Form { children(node.children) }.formStyle(.grouped))
        case "Section":
            return AnyView(Section {
                children(node.children)
            } header: {
                if !title.isEmpty { Text(title) }
            })
        case "ScrollView":
            let axis: Axis.Set = props["axis"]?.text.contains("horizontal") == true ? .horizontal : .vertical
            return AnyView(ScrollView(axis, showsIndicators: props["showsIndicators"]?.truth ?? true) {
                VStack(alignment: .leading, spacing: spacing(node)) {
                    children(node.children)
                }
                .frame(maxWidth: axis == .vertical ? .infinity : nil, alignment: .leading)
            })
        case "List":
            return AnyView(List { children(node.children) })
        case "Group", "ForEach":
            return AnyView(Group { children(node.children) })
        case "Text":
            return AnyView(Text(node.text.isEmpty ? props["value"]?.text ?? title : node.text)
                .fixedSize(horizontal: false, vertical: true))
        case "Label":
            return AnyView(Label(title, systemImage: symbol(node, fallback: "circle.fill")))
        case "Image":
            return AnyView(Image(systemName: symbol(node, fallback: "photo"))
                .accessibilityLabel(props["label"]?.text ?? node.text))
        case "Button":
            let role: ButtonRole? = props["role"]?.text.contains("destructive") == true ? .destructive : nil
            return AnyView(Button(role: role) {
                guard let actionID = node.actionID else { return }
                Task { await session.perform(actionID: actionID) }
            } label: {
                if node.children.isEmpty {
                    Text(title.isEmpty ? "Continue" : title)
                        .frame(minHeight: 44)
                } else {
                    children(node.children).frame(minHeight: 44)
                }
            }
            .disabled(session.isBusy || !pendingBindings.isEmpty || node.actionID == nil))
        case "TextField":
            if case .number = current(node) {
                return AnyView(TextField(title.isEmpty ? "Value" : title, value: numberBinding(node), format: .number))
            }
            return AnyView(TextField(title.isEmpty ? "Text" : title, text: textBinding(node)))
        case "SecureField":
            return AnyView(SecureField(title.isEmpty ? "Password" : title, text: textBinding(node)))
        case "TextEditor":
            return AnyView(VStack(alignment: .leading, spacing: 8) {
                if !title.isEmpty { Text(title).font(.subheadline).foregroundStyle(.secondary) }
                TextEditor(text: textBinding(node))
                    .frame(minHeight: 120)
                    .padding(4)
                    .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 10))
                    .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Color.primary.opacity(0.12)))
                    .accessibilityLabel(title.isEmpty ? "Text" : title)
            })
        case "Toggle":
            return AnyView(Toggle(title.isEmpty ? "Enabled" : title, isOn: Binding(
                get: { current(node).truth },
                set: { write(node, .bool($0)) }
            )))
        case "Stepper":
            let range = bounds(node, fallback: -1_000_000...1_000_000)
            return AnyView(Stepper(value: numberBinding(node), in: range, step: step(node)) {
                Text(title)
            })
        case "Slider":
            let range = bounds(node, fallback: 0...1)
            return AnyView(VStack(alignment: .leading, spacing: 8) {
                HStack {
                    if !title.isEmpty { Text(title) }
                    Spacer(minLength: 12)
                    Text(current(node).text).monospacedDigit().foregroundStyle(.secondary)
                }
                slider(node, range: range)
                    .accessibilityLabel(title.isEmpty ? "Value" : title)
            })
        case "Picker":
            return picker(node)
        case "Divider":
            return AnyView(Divider())
        case "Spacer":
            return AnyView(Spacer(minLength: dimension(props["minLength"])))
        case "ProgressView":
            if props["value"] != nil || node.binding != nil {
                let total = max(0.000001, finite(props["max"]?.number ?? props["total"]?.number ?? 1, fallback: 1))
                return AnyView(ProgressView(value: finite(current(node).number, fallback: 0).clamped(to: 0...total), total: total) {
                    if !title.isEmpty { Text(title) }
                })
            }
            return AnyView(ProgressView { if !title.isEmpty { Text(title) } })
        case "Gauge":
            let range = bounds(node, fallback: 0...1)
            return AnyView(Gauge(value: finite(current(node).number, fallback: range.lowerBound).clamped(to: range), in: range) {
                Text(title)
            } currentValueLabel: {
                Text(current(node).text).monospacedDigit()
            })
        case "BarChart", "LineChart", "AreaChart", "PointChart", "PieChart":
            return chart(node)
        default:
            return AnyView(ContentUnavailableView(
                "Unable to show this content",
                systemImage: "exclamationmark.triangle",
                description: Text("Try editing the app and opening it again.")
            ))
        }
    }

    private func slider(_ node: AppNode, range: ClosedRange<Double>) -> AnyView {
        let binding = Binding<Double>(
            get: { finite(current(node).number, fallback: range.lowerBound).clamped(to: range) },
            set: { write(node, .number($0)) }
        )
        if node.properties["step"] != nil {
            return AnyView(Slider(value: binding, in: range, step: step(node)))
        }
        return AnyView(Slider(value: binding, in: range))
    }

    private func picker(_ node: AppNode) -> AnyView {
        let options = pickerOptions(node.children)
        let selection = Binding<String>(
            get: {
                let value = current(node)
                return options.first(where: { optionValue($0) == value })?.id ?? ""
            },
            set: { id in
                if let option = options.first(where: { $0.id == id }) {
                    write(node, optionValue(option))
                }
            }
        )
        return AnyView(Picker(label(node).isEmpty ? "Choose an option" : label(node), selection: selection) {
            if !options.contains(where: { optionValue($0) == current(node) }) {
                Text("Choose…").tag("")
            }
            ForEach(options) { option in
                render(option).tag(option.id)
            }
        })
    }

    private func chart(_ node: AppNode) -> AnyView {
        let entries = ChartEntry.entries(node.properties["data"] ?? node.properties["values"] ?? .null)
            ?? node.children.enumerated().map { ChartEntry(index: $0.offset, label: label($0.element), value: finite(current($0.element).number, fallback: 0), series: "") }
        let title = label(node)
        let multiSeries = Set(entries.map(\.series)).count > 1
        let lower = min(entries.map(\.value).min() ?? 0, 0)
        let upper = max(entries.map(\.value).max() ?? 0, lower + 1)
        return AnyView(VStack(alignment: .leading, spacing: 8) {
            if !title.isEmpty { Text(title).font(.headline) }
            if entries.isEmpty {
                Text("No data yet").font(.callout).foregroundStyle(.secondary)
            } else if node.kind == "PieChart" {
                Chart(entries) { item in
                    SectorMark(angle: .value("Value", max(0, item.value)), innerRadius: .ratio(0.55), angularInset: 1.5)
                        .foregroundStyle(by: .value("Category", item.label))
                        .cornerRadius(4)
                        .accessibilityLabel(item.label)
                        .accessibilityValue(AppValue.number(item.value).text)
                }
                .chartLegend(position: .bottom, alignment: .leading)
                .frame(minHeight: 200, idealHeight: 220)
            } else {
                Chart(entries) { item in
                    mark(node.kind, item, multiSeries: multiSeries)
                }
                .chartYScale(domain: lower...upper)
                .chartLegend(multiSeries ? .visible : .hidden)
                .frame(minHeight: 160, idealHeight: 200)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(title.isEmpty ? "Chart" : title))
    }

    @ChartContentBuilder
    private func mark(_ kind: String, _ item: ChartEntry, multiSeries: Bool) -> some ChartContent {
        let x = PlottableValue.value("Label", item.label)
        let y = PlottableValue.value("Value", item.value)
        let series = PlottableValue.value("Series", item.series)
        switch kind {
        case "LineChart":
            LineMark(x: x, y: y, series: series)
                .foregroundStyle(by: series)
                .interpolationMethod(.catmullRom)
                .symbol(by: series)
                .accessibilityLabel(item.label).accessibilityValue(AppValue.number(item.value).text)
        case "AreaChart":
            AreaMark(x: x, y: y, stacking: multiSeries ? .standard : .unstacked)
                .foregroundStyle(by: series)
                .interpolationMethod(.catmullRom)
                .opacity(multiSeries ? 0.85 : 0.6)
                .accessibilityLabel(item.label).accessibilityValue(AppValue.number(item.value).text)
        case "PointChart":
            PointMark(x: x, y: y)
                .foregroundStyle(by: series)
                .accessibilityLabel(item.label).accessibilityValue(AppValue.number(item.value).text)
        default:
            BarMark(x: x, y: y)
                .foregroundStyle(by: series)
                .position(by: series)
                .cornerRadius(3)
                .accessibilityLabel(item.label).accessibilityValue(AppValue.number(item.value).text)
        }
    }

    private func current(_ node: AppNode) -> AppValue {
        if let binding = node.binding { return pendingBindings[binding]?.value ?? session.value(for: binding) }
        return node.properties["value"] ?? .null
    }

    private func write(_ node: AppNode, _ value: AppValue) {
        guard let binding = node.binding else { return }
        // Native text controls read their binding again within the same edit event.
        // Echo the accepted edit synchronously while the runtime serializes its save.
        let id = UUID()
        pendingBindings[binding] = PendingBinding(id: id, value: value)
        Task {
            await session.setBinding(binding, value: value)
            if pendingBindings[binding]?.id == id { pendingBindings[binding] = nil }
        }
    }

    private func textBinding(_ node: AppNode) -> Binding<String> {
        Binding(get: { current(node).text }, set: { write(node, .string($0)) })
    }

    private func numberBinding(_ node: AppNode) -> Binding<Double> {
        Binding(get: { finite(current(node).number, fallback: 0) }, set: { write(node, .number($0)) })
    }

    private func pickerOptions(_ nodes: [AppNode]) -> [AppNode] {
        nodes.flatMap { node in
            node.kind == "ForEach" || node.kind == "Group" ? pickerOptions(node.children) : [node]
        }
    }

    private func optionValue(_ node: AppNode) -> AppValue {
        node.properties["tag"] ?? node.properties["value"] ?? .string(node.text)
    }

    private func label(_ node: AppNode) -> String {
        node.properties["label"]?.text ?? node.properties["title"]?.text ?? node.text
    }

    private func symbol(_ node: AppNode, fallback: String) -> String {
        let value = node.properties["systemImage"]?.text ?? node.properties["systemName"]?.text ?? (node.kind == "Image" ? node.text : "")
        return value.isEmpty ? fallback : value
    }

    private func bounds(_ node: AppNode, fallback: ClosedRange<Double>) -> ClosedRange<Double> {
        let lower = finite(node.properties["min"]?.number ?? fallback.lowerBound, fallback: fallback.lowerBound)
        let proposed = finite(node.properties["max"]?.number ?? fallback.upperBound, fallback: fallback.upperBound)
        if proposed > lower { return lower...proposed }
        let upper = lower + max(1, abs(lower) * 0.000001)
        return upper.isFinite && upper > lower ? lower...upper : fallback
    }

    private func step(_ node: AppNode) -> Double {
        let value = finite(node.properties["step"]?.number ?? 1, fallback: 1)
        return value > 0 ? value : 1
    }

    private func spacing(_ node: AppNode) -> CGFloat? {
        // nil preserves SwiftUI's context-sensitive spacing; explicit values win.
        dimension(node.properties["spacing"])
    }

    private func dimension(_ value: AppValue?) -> CGFloat? {
        guard case .number(let number) = value, number.isFinite, number >= 0 else { return nil }
        return CGFloat(number)
    }

    private func finite(_ value: Double, fallback: Double) -> Double {
        value.isFinite ? value : fallback
    }

    private func name(_ value: AppValue) -> String {
        value.text.split(separator: ".").last.map(String.init) ?? value.text
    }

    private func horizontal(_ value: AppValue?) -> HorizontalAlignment {
        switch value.map(name) {
        case "leading": return .leading
        case "trailing": return .trailing
        default: return .center
        }
    }

    private func vertical(_ value: AppValue?) -> VerticalAlignment {
        switch value.map(name) {
        case "top": return .top
        case "bottom": return .bottom
        case "firstTextBaseline": return .firstTextBaseline
        case "lastTextBaseline": return .lastTextBaseline
        default: return .center
        }
    }

    private func edges(_ value: AppValue?) -> Edge.Set {
        guard let value else { return .all }
        if case .array(let values) = value {
            return values.reduce(Edge.Set()) { $0.union(edges($1)) }
        }
        switch name(value) {
        case "horizontal": return .horizontal
        case "vertical": return .vertical
        case "top": return .top
        case "bottom": return .bottom
        case "leading": return .leading
        case "trailing": return .trailing
        default: return .all
        }
    }

    private func maximumDimension(_ value: AppValue?) -> CGFloat? {
        if let value, value.text.contains("infinity") || value == .number(.infinity) { return .infinity }
        return dimension(value)
    }

    private func alignment(_ value: AppValue?) -> Alignment {
        switch value.map(name) {
        case "leading": return .leading
        case "trailing": return .trailing
        case "top": return .top
        case "bottom": return .bottom
        case "topLeading": return .topLeading
        case "topTrailing": return .topTrailing
        case "bottomLeading": return .bottomLeading
        case "bottomTrailing": return .bottomTrailing
        default: return .center
        }
    }

    private func weight(_ value: AppValue) -> Font.Weight {
        switch name(value) {
        case "ultraLight": return .ultraLight
        case "thin": return .thin
        case "light": return .light
        case "medium": return .medium
        case "semibold": return .semibold
        case "bold": return .bold
        case "heavy": return .heavy
        case "black": return .black
        default: return .regular
        }
    }

    private func unsupportedStyle() -> AnyView {
        AnyView(Label("This appearance is unavailable", systemImage: "exclamationmark.triangle")
            .foregroundStyle(.secondary))
    }

    private func buttonStyle(_ view: AnyView, _ value: AppValue) -> AnyView {
        switch name(value) {
        case "automatic": return AnyView(view.buttonStyle(.automatic))
        case "plain": return AnyView(view.buttonStyle(.plain))
        case "borderless": return AnyView(view.buttonStyle(.borderless))
        case "bordered": return AnyView(view.buttonStyle(.bordered))
        case "borderedProminent": return AnyView(view.buttonStyle(.borderedProminent))
        default: return unsupportedStyle()
        }
    }

    private func textFieldStyle(_ view: AnyView, _ value: AppValue) -> AnyView {
        switch name(value) {
        case "automatic": return AnyView(view.textFieldStyle(.automatic))
        case "plain": return AnyView(view.textFieldStyle(.plain))
        case "roundedBorder": return AnyView(view.textFieldStyle(.roundedBorder))
        #if os(macOS)
        case "squareBorder": return AnyView(view.textFieldStyle(.squareBorder))
        #endif
        default: return unsupportedStyle()
        }
    }

    private func listStyle(_ view: AnyView, _ value: AppValue) -> AnyView {
        switch name(value) {
        case "automatic": return AnyView(view.listStyle(.automatic))
        case "plain": return AnyView(view.listStyle(.plain))
        case "inset": return AnyView(view.listStyle(.inset))
        case "sidebar": return AnyView(view.listStyle(.sidebar))
        #if os(iOS)
        case "grouped": return AnyView(view.listStyle(.grouped))
        case "insetGrouped": return AnyView(view.listStyle(.insetGrouped))
        #elseif os(macOS)
        case "grouped", "insetGrouped": return AnyView(view.listStyle(.inset))
        #endif
        default: return unsupportedStyle()
        }
    }

    private func font(_ value: AppValue) -> Font {
        switch name(value) {
        case "largeTitle": return .largeTitle
        case "title": return .title
        case "title2": return .title2
        case "title3": return .title3
        case "headline": return .headline
        case "subheadline": return .subheadline
        case "callout": return .callout
        case "caption": return .caption
        case "caption2": return .caption2
        case "footnote": return .footnote
        case "monospaced": return .body.monospaced()
        default: return .body
        }
    }

    private func color(_ value: AppValue) -> Color {
        switch name(value).lowercased() {
        case "secondary": return .secondary
        case "accentcolor", "accent", "tint": return .accentColor
        case "red": return .red
        case "orange": return .orange
        case "yellow": return .yellow
        case "green": return .green
        case "mint": return .mint
        case "teal": return .teal
        case "cyan": return .cyan
        case "blue": return .blue
        case "indigo": return .indigo
        case "purple": return .purple
        case "pink": return .pink
        case "brown": return .brown
        case "gray", "grey": return .gray
        case "black": return .black
        case "white": return .white
        case "clear": return .clear
        default: return .primary
        }
    }
}

private extension Double {
    func clamped(to range: ClosedRange<Double>) -> Double {
        min(max(self, range.lowerBound), range.upperBound)
    }
}
#endif
