#!/usr/bin/env python3
"""Exercise the actual media projection and parser with synthetic tool results."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
model = (root / 'NanocodexInbox/InboxModel.swift').read_text()
projection = model[model.index('private struct InboxMediaProjection: Sendable {'):]
parser = (root / 'NanocodexUI/Sources/NanocodexUI/ChatGeneratedOutput.swift').read_text()
fixtures = r'''
typealias Cursor = Int
struct ToolPresentation {
    var isInspectionOutput = false
    var generatedResults: [String]?
    var isComputerScreenOutput = false
}
struct TranscriptRow {
    var id: String
    var tool: ToolPresentation?
    var cursor: Cursor?
    var completionCursor: Cursor? = nil
}
func result(_ value: String) -> String {
    "{\"type\":\"input_image\",\"image_url\":\"data:image/png;base64,\(value)\"}"
}
func row(_ id: String, _ cursor: Int, _ screen: Bool, _ bytes: String) -> TranscriptRow {
    TranscriptRow(id: id, tool: ToolPresentation(generatedResults: [result(bytes)], isComputerScreenOutput: screen), cursor: cursor)
}
private var media = InboxMediaProjection()
let outer = row("outer", 12, false, "AQ==")
let nested = row("nested", 10, true, "AQ==")
let ordinary = row("art", 11, false, "Ag==")
media.update([outer, ordinary])
precondition(media.outputs.count == 2 && media.latestScreen == nil)
media.update([outer, ordinary, nested])
precondition(media.outputs.count == 1 && media.outputs["art"]?.count == 1)
let first = media.latestScreen!.id
private var reversed = InboxMediaProjection()
reversed.update([nested, ordinary, outer])
precondition(reversed.latestScreen?.id == first && reversed.outputs == media.outputs)
// Same row/results with newly arrived attribution must invalidate the cached parse.
private var late = InboxMediaProjection()
late.update([outer]); var attributed = outer
attributed.tool!.isComputerScreenOutput = true
late.update([attributed])
precondition(late.outputs.isEmpty && late.latestScreen?.id == first)
// A newer screen advances; older paging and eviction must not regress it.
let newer = row("new", 20, true, "Aw==")
media.update([newer]); let latest = media.latestScreen!.id
precondition(latest != first)
media.update([nested]); precondition(media.latestScreen?.id == latest)
media.update([]); precondition(media.latestScreen?.id == latest)
// Previously attributed outer duplicates remain suppressed after nested row eviction.
media.update([outer, ordinary]); precondition(media.outputs.count == 1)
// Captures can finish out of admission order; completion defines the latest screen.
private var overlapping = InboxMediaProjection()
var earlierCall = row("capture-a", 30, true, "BA==")
var laterCall = row("capture-b", 31, true, "BQ==")
earlierCall.completionCursor = 33
laterCall.completionCursor = 32
overlapping.update([earlierCall, laterCall])
let completedLast = ChatGeneratedOutput.parse(results: earlierCall.tool!.generatedResults!, computerScreen: true).first!.id
precondition(overlapping.latestScreen?.id == completedLast)
overlapping.update([laterCall]); precondition(overlapping.latestScreen?.id == completedLast)
// New conversation/account state resets both attribution and retained image.
media = InboxMediaProjection(); media.update([outer])
precondition(media.latestScreen == nil && media.outputs.count == 1)
print("screen projection: all ordering, preservation, eviction, reset checks passed")
'''
with tempfile.TemporaryDirectory() as directory:
    script = Path(directory) / 'main.swift'
    script.write_text(parser + '\n' + projection + '\n' + fixtures)
    subprocess.run(['swift', str(script)], check=True)
