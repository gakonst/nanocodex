#!/usr/bin/env python3
"""Exercise the app's actual geometry index with variable-height transcript rows."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
view = (root / "NanocodexInbox/InboxView.swift").read_text()
start = view.index("private final class ConversationRowGeometry {")
end = view.index("private struct ConversationRowFrames:", start)
source = "import Foundation\nimport CoreGraphics\n" + view[start:end] + r'''
private let index = ConversationRowGeometry()
var frames: [String: CGRect] = [:]
var y: CGFloat = 24
for i in 0..<10_000 {
    let height = CGFloat(20 + i % 37 * 19)
    frames["row-\(i)"] = CGRect(x: 20, y: y, width: 350, height: height)
    y += height + 18
}
index.updateContentFrames(frames)
for offset in [-40.0, 0, 24, 44, 62, 200, 17_000, Double(y - 500), Double(y), Double(y + 100)] {
    index.updateOffset(offset)
    let expected = frames.filter { $0.value.maxY > offset && $0.value.minY < offset + 700 }
        .mapValues { $0.offsetBy(dx: 0, dy: -offset) }
    precondition(index.visibleFrames(height: 700) == expected, "Viewport mismatch at \(offset)")
    precondition(index["row-5"] == frames["row-5"]!.offsetBy(dx: 0, dy: -offset))
    let first = frames.filter { $0.value.maxY > offset && $0.key != "row-0" }
        .min { $0.value.minY < $1.value.minY }
    precondition(index.firstFrame(where: { $0 != "row-0" })?.key == first?.key)
}
index.updateContentFrames(["replacement": CGRect(x: 0, y: 100, width: 200, height: 300)])
index.updateOffset(150)
precondition(index["row-5"] == nil)
precondition(index.visibleFrames(height: 200) == ["replacement": CGRect(x: 0, y: -50, width: 200, height: 300)])
print("PASS: 10,000 variable-height rows, viewport boundaries, overscroll, and layout replacement")
'''
with tempfile.TemporaryDirectory(prefix="nanocodex-geometry-") as temp:
    path = Path(temp)
    (path / "main.swift").write_text(source)
    subprocess.run(["swiftc", "-O", str(path / "main.swift"), "-o", str(path / "check")], check=True)
    subprocess.run([str(path / "check")], check=True)
