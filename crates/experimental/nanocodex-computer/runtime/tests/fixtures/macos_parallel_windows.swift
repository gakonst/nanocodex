// Nonactivating, uniquely sized windows for the single-companion live test.
import AppKit
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let logPath = CommandLine.arguments[1]
let instance = Int(CommandLine.arguments[2])!
func cursor() -> [Double] { let p = CGEvent(source: nil)!.location; return [p.x, p.y] }
func log(_ value: [String: Any]) {
    let data = try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    let handle = FileHandle(forWritingAtPath: logPath)!
    handle.seekToEndOfFile(); handle.write(data); handle.write(Data([10])); try! handle.close()
}
var windows: [NSWindow] = []
var editors: [NSTextView] = []
for i in 0..<2 {
    let width = 320 + (instance * 2 + i) * 40
    let w = NSWindow(contentRect: NSRect(x: 60 + i * 400, y: 60, width: width, height: 240), styleMask: [.titled], backing: .buffered, defer: false)
    w.title = "Parallel fixture \(instance) window \(i)"
    let editor = NSTextView(frame: NSRect(x: 0, y: 0, width: width, height: 240))
    w.contentView = editor; w.makeFirstResponder(editor); w.orderBack(nil)
    windows.append(w); editors.append(editor)
}
var previous = ["", ""]
Timer.scheduledTimer(withTimeInterval: 0.01, repeats: true) { _ in
    for i in 0..<2 where previous[i] != editors[i].string {
        previous[i] = editors[i].string
        log(["window": i, "text": previous[i]])
    }
}
log(["ready": ProcessInfo.processInfo.processIdentifier, "windows": windows.map { $0.windowNumber }, "front": NSWorkspace.shared.frontmostApplication!.processIdentifier, "cursor": cursor()])
Timer.scheduledTimer(withTimeInterval: 0.02, repeats: true) { _ in
    log(["front": NSWorkspace.shared.frontmostApplication!.processIdentifier, "cursor": cursor()])
}
Timer.scheduledTimer(withTimeInterval: 180, repeats: false) { _ in app.terminate(nil) }
app.run()
