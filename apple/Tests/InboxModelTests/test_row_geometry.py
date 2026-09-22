#!/usr/bin/env python3
"""Exercise realized transcript rows supplied in viewport coordinates."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
view = (root / "NanocodexInbox/InboxView.swift").read_text()
start = view.index("private final class ConversationRowGeometry {")
end = view.index("private struct ConversationContentPosition:", start)
source = "import Foundation\nimport CoreGraphics\n" + view[start:end] + r'''
private let index = ConversationRowGeometry()
let frames: [String: CGRect] = [
    "above": CGRect(x: 20, y: -80, width: 350, height: 80),
    "partial": CGRect(x: 20, y: -30, width: 350, height: 70),
    "inside": CGRect(x: 20, y: 60, width: 350, height: 180),
    "bottom": CGRect(x: 20, y: 690, width: 350, height: 100),
    "below": CGRect(x: 20, y: 700, width: 350, height: 30)
]
index.updateViewportFrames(frames)
precondition(Set(index.visibleFrames(height: 700).keys) == ["partial", "inside", "bottom"])
precondition(index["partial"] == frames["partial"], "Viewport frames must not be transformed")
precondition(index.firstFrame(where: { _ in true })?.key == "partial")
precondition(index.firstFrame(where: { $0 == "inside" })?.key == "inside")
precondition(index.firstFrame(where: { $0 == "above" }) == nil)
// The native layout reports new viewport coordinates on scrolling/overscroll.
let overscrolled = frames.mapValues { $0.offsetBy(dx: 0, dy: 120) }
index.updateViewportFrames(overscrolled)
precondition(index["partial"] == overscrolled["partial"])
precondition(Set(index.visibleFrames(height: 700).keys) == ["above", "partial", "inside"])
precondition(index.firstFrame(where: { _ in true })?.key == "above")
index.updateViewportFrames(["replacement": CGRect(x: 0, y: -50, width: 200, height: 300)])
precondition(index["inside"] == nil)
precondition(index.visibleFrames(height: 200) == ["replacement": CGRect(x: 0, y: -50, width: 200, height: 300)])
index.updateViewportFrames([:])
precondition(index.visibleFrames(height: 700).isEmpty)
precondition(index.firstFrame(where: { _ in true }) == nil)
print("PASS: viewport coordinates, boundaries, overscroll, working-set replacement, and empty viewport")
'''
with tempfile.TemporaryDirectory(prefix="nanocodex-geometry-") as temp:
    path = Path(temp)
    (path / "main.swift").write_text(source)
    subprocess.run(["swiftc", "-O", str(path / "main.swift"), "-o", str(path / "check")], check=True)
    subprocess.run([str(path / "check")], check=True)
