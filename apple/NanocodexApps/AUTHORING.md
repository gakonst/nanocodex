# Native Swift apps (`swift-v1`)

Apps are Swift source parsed by SwiftSyntax and interpreted by a bounded Swift runtime. Views are native SwiftUI controls. This is an explicit Swift subset, not a general Swift compiler; there is no HTML, JavaScript, WebKit, dynamic linking, or arbitrary operating-system access.

An app has one `struct Name: View` with `var body: some View`. `import SwiftUI` is optional. Give stored fields in the View explicit initial values. Plain record structs use typed fields without initializers, such as `struct Entry { var id: String; var count: Int }`; supply every field during memberwise construction, such as `Entry(id: UUID().uuidString, count: 0)`. `@State` lives for the session; `@Persisted("stable-key")` loads and saves JSON through the signed-in host. Persisted keys survive source changes. Keep keys stable across revisions.

```swift
import SwiftUI
struct ReadingTracker: View {
    @Persisted("books") var books = ["The Odyssey"]
    @State var title = ""
    @State var advice = ""
    var body: some View {
        Form {
            Section("Library") {
                TextField("Book title", text: $title)
                Button("Add book") {
                    if !title.isEmpty {
                        books.append(title)
                        title = ""
                    }
                }
                ForEach(books, id: \.self) { book in
                    Text(book)
                }
            }
            Section("Reading companion") {
                Button("Suggest a next read") {
                    Task {
                        advice = await Agent.run("Suggest a book based on: " + books.joined(separator: ", "))
                    }
                }
                Text(advice)
            }
        }
        .navigationTitle("Reading")
    }
}
```

## Nanocodex appearance

Use the existing Nanocodex design: adaptive neutral surfaces, primary text and accent, secondary supporting text, semantic system typography, and native controls. The renderer defaults to `.body`, a `.primary` tint, and the platform system background. These defaults adapt to light and dark appearance; semantic text styles support Dynamic Type. Explicit supported color, font, spacing, and control-style modifiers still apply normally.

Start with `Form` and `Section` for editable trackers or `List` for collections. Let these controls provide row spacing and surfaces. Use `.headline` for section emphasis, `.title2` or `.title3` for an in-content title, `.body` for content, and `.caption` or `.footnote` with `.foregroundStyle(.secondary)` for supporting text. Prefer `.navigationTitle` over a second decorative hero heading.

For custom layouts, use `VStack(alignment: .leading, spacing: 12)`, 8-point spacing for tightly related content, and `.padding()` for native outer insets. Omitted stack spacing and padding use SwiftUI defaults; omitted stack alignment follows SwiftUI (`VStack` centers horizontally). Avoid fixed text heights so larger text can wrap. Keep the native automatic input and list styles; use `.buttonStyle(.borderedProminent)` for a primary action and `.bordered` for secondary actions when needed. Buttons retain a minimum 44-point label height.

Do not invent an app-specific brand palette, colored page backgrounds, gradients, or decorative tinted cards. Omit `.tint` to use the Nanocodex accent. Use `.primary` and `.secondary` for text; reserve explicit colors for meaningful status or user-requested content, and pair status colors with text or a symbol. Supported explicit colors such as `.blue` retain their SwiftUI meaning; the renderer does not rewrite them into neutral colors.

## Supported source

- Stored `@State` and `@Persisted("key")` variables, ordinary constants, local `let`/`var`, user functions with positional or labeled calls, and plain record structs with stored fields and memberwise construction.
- Strings and interpolation, numbers, booleans, nil, arrays, string-keyed dictionaries; member access and collection subscripts; assignments and `+=`, `-=`, `*=`, `/=`; arithmetic, comparison, `&&`, `||`, `!`, `??`, ternaries, ranges.
- `if`/`else`, `for … in`, `while` (actions), `return`. View builders support expressions, local values, conditionals, and for loops. `ForEach(items, id: \.self) { item in … }` honors the supplied identity and diagnoses duplicates; `id: \.id` uses each record's id field.
- `String`, `Int`, `Double`, `Bool`, `Array`, `min`, `max`, `abs`, `round`, `Date()`, `Date(timeIntervalSince1970:)`, `UUID()`; collection `count`, `isEmpty`, `first`, `last`, `append`, `removeLast`, `removeAll`, `contains`, `joined(separator:)`, `reversed`, `sorted`, `prefix(count)`, `suffix(count)`, `dropFirst(count)`, and trailing-closure `map`, `filter`, `reduce`; string `contains`, `lowercased`, `uppercased`.
- Views: `VStack`, `HStack`, `ZStack`, `Form`, `Section`, `ScrollView`, `List`, `Group`, `ForEach`, `Text`, `Label`, `Image(systemName:)`, `Button("Title") { … }`, `TextField("Title", text: $field)`, `SecureField`, `TextEditor(text:)`, `Toggle("Title", isOn:)`, `Stepper("Title", value:, in:, step:)`, `Slider(value:, in:)`, `Picker`, `Divider`, `Spacer`, `ProgressView`, `Gauge`, and SDK charts `BarChart`, `LineChart`, `AreaChart`, `PointChart`, `PieChart` — each takes one data array (`[numbers]` or dictionaries with `label`/`x`, `value`/`y`, optional `series`/`group`) and an optional `title:`.
- Inline chat artifacts: a ```` ```swift-artifact ```` fence in an assistant response renders the same source inline on iOS. State is in-memory only, `Agent.run` is disabled, and content sizes to fit, so prefer stacks over `Form`/`List`/`ScrollView`.
- Modifiers: `padding`, `font`, `fontWeight`, `foregroundStyle`, `foregroundColor`, `tint`, `background`, `frame`, `cornerRadius`, `opacity`, `disabled`, `navigationTitle`, `buttonStyle`, `listStyle`, `textFieldStyle`, `lineLimit`, `multilineTextAlignment`, `tag`. Prefer simple enum values such as `.headline`, `.secondary`, `.borderedProminent`.
- Dates: `Date().timeIntervalSince1970` returns epoch seconds; `Date(timeIntervalSince1970: seconds).formatted()` returns a localized date/time string. `UUID().uuidString` gives stable IDs when created in an action. SDK `Clock.today()` returns the current local calendar day as `yyyy-MM-dd`; `Clock.dayKey(seconds)` converts an epoch timestamp to that same local day format. Store timestamps and stable record IDs; avoid generating new IDs while rendering.
- `Task { answer = await Agent.run("prompt") }` in an action awaits the host's authenticated agent. The return value is a string. The host owns credentials and logs the request. App source never receives credentials.

## Boundaries and failures

Unsupported syntax fails validation before initialization. Unsupported dynamic operations produce a visible diagnostic and roll back the action's local state changes. Function argument types and return annotations document intent; this interpreter is dynamically typed and does not run Swift's static type checker. No custom classes, protocols, extensions, generic declarations, macros, property observers, computed state, enum declarations, custom operators, arbitrary Foundation APIs, networking, filesystem, unstructured concurrency, arbitrary frameworks, or view lifecycle callbacks are supported. Conditions must be boolean expressions; optional binding (`if let`, `if var`, `guard let`) is unsupported. Use `??` with a suitable default where needed. Avoid optional chaining/force unwrap, multi-trailing-closure buttons, and framework-specific style constructors.

Defaults bound each operation to 500,000 evaluation steps, 64 nested calls, 2,000 view nodes, 2,000 collection entries, three agent calls, and 256 KiB per text value. Source is bounded to 256 KiB before parsing. The interpreter yields to the main actor every 1,024 steps so bounded heavy work stays cancellable and native controls remain responsive. Read-only value functions are memoized within each render (up to 128 argument/result entries); the cache is discarded between renders, and `Date()`/`Clock.today()` share the render's timestamp. Functions that create UUIDs keep producing fresh identities. Show a bounded window such as `entries.suffix(30)` or `entries.dropFirst(page * 30).prefix(30)` when histories exceed the view-node bound; keep full histories for totals and persistence. Paging counts must be nonnegative integers.

Actions and persisted saves are serialized. `@State` edits do not write storage. A failed save shows a diagnostic and restores prior state. Agent calls are external effects and cannot be undone when a later action statement fails. Such failures explicitly warn that the request may have completed external work and that agent activity should be checked before retrying. `Agent.run` and `Task` require an explicit button-action context; initializers, view rendering, and binding-triggered renders cannot invoke them.

## Host API

Link the `NanocodexApps` Swift package. Supply `NativeAppHost(loadState:saveState:runAgent:)`; state is `[String: AppValue]` with ordinary JSON encoding. Construct `try NativeAppSession(source:host:)`, call `try await session.start()`, and display `NativeAppView(session:)`. Optional synchronous `beginAction` and `commitAction` host callbacks bracket an explicit action. Commit is called only after action execution, rendering, and any persisted save all succeed; errors leave the receipt uncommitted, and bindings invoke neither callback. Hosts can use these callbacks to retain idempotent external-operation receipts across local failures. Keep these callbacks nonthrowing; if durable receipt cleanup fails, retain the receipt for recovery. Call `invalidate()` on teardown. `NativeAppSession.validate(source:)` checks source without executing it.

## Validate before publishing

The `apps` tool's `validate` operation runs the installed `swift-v1` parser and
interpreter on an online native Hand. Provide `runtime`, `source`, and optional
isolated `state` and `steps`. Steps support `tap` with a button `title`, `set` with
`binding` and `value`, `expect` with exact rendered `text`, and `reopen`. The
validator always initializes the app and reopens its persisted test state. Use
representative inputs and expected totals before claiming an app is ready.

Saves and restores also require native preflight. An edit is checked against a
copy of the current saved data; concurrent data changes invalidate that check
and require a retry. A validation failure includes its stage and native
line diagnostic and does not replace the saved source. An unavailable validator
blocks publication; open an updated native Nanocodex Hand and retry.

Validation never changes the app's real data or calls a live agent. An optional
`agent_response` supplies an explicit test fixture. Results describe only the
supplied journey and return bounded interpreter trees, not screenshots or a
full Swift compiler typecheck. Visual layout should also be inspected when
relevant.
