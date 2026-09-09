import AppKit
let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 960, pixelsHigh: 600, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
func box(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat, _ color: NSColor) {
    color.setFill(); NSRect(x: x, y: y, width: w, height: h).fill()
}
func text(_ value: String, _ x: CGFloat, _ y: CGFloat, _ size: CGFloat, _ color: NSColor = .white) {
    (value as NSString).draw(at: NSPoint(x: x, y: y), withAttributes: [.font: NSFont.monospacedSystemFont(ofSize: size, weight: .regular), .foregroundColor: color])
}
box(0, 0, 960, 600, NSColor(red: 0.07, green: 0.10, blue: 0.15, alpha: 1))
box(0, 542, 960, 58, NSColor(red: 0.13, green: 0.18, blue: 0.25, alpha: 1))
text("LOCAL SCREEN FIXTURE   /   Workspace", 28, 560, 20)
text("Dashboard preview", 36, 464, 32)
text("The agent's screen stays beside your conversation.", 36, 423, 18, .lightGray)
box(36, 268, 888, 112, NSColor(red: 0.12, green: 0.23, blue: 0.22, alpha: 1))
text("Build status", 60, 332, 18, .lightGray)
text("All checks passing", 60, 291, 27, .systemGreen)
text("$ pnpm test", 36, 199, 22)
text("  Layout, keyboard and session checks", 36, 159, 19, .lightGray)
text("  Ready for review", 36, 122, 19, .systemGreen)
text("View-only fixture - no remote input is executed", 36, 35, 16, .gray)
NSGraphicsContext.restoreGraphicsState()
try bitmap.representation(using: .jpeg, properties: [.compressionFactor: 0.85])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
