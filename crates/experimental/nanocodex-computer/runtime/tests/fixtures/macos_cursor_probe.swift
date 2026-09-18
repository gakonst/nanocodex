import AppKit
import CoreGraphics
let pids = Set(CommandLine.arguments.dropFirst().compactMap { Int($0) })
let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly,.excludeDesktopElements], kCGNullWindowID) as? [[String:Any]] ?? []
let point = CGEvent(source: nil)?.location ?? .zero
let result: [String:Any] = ["front": NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1, "cursor": [point.x,point.y], "windows": windows.filter { pids.contains($0[kCGWindowOwnerPID as String] as? Int ?? -1) }]
let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
print(String(data:data,encoding:.utf8)!)
