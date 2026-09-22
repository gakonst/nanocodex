#!/usr/bin/env python3
"""Exercise the actual voice queue method with deterministic model dependencies."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
model = (root / 'NanocodexInbox/InboxModel.swift').read_text()
start = model.index('    func sendQuickVoice(')
method = model[start:model.index('    private var scope', start)]
source = r'''
import Foundation
@MainActor final class Model {
    struct Card { var id: String }
    var connected = true, isDemo = false, generation = UUID()
    var cards: [Card] = [Card(id: "existing")]
    var focused: Card? = Card(id: "existing")
    var createdAgentIDs: [String: String] = [:]
    var draft = "", creations = 0, sentTo: [String] = [], succeeds = false
    func newAgent() {
        creations += 1
        let card = Card(id: "draft-\(creations)")
        cards.append(card); focused = card
    }
    func select(_ id: String) { focused = cards.first { $0.id == id } }
    func send() -> Bool { if succeeds { sentTo.append(focused!.id) }; return succeeds }
'''
source += method
source += r'''
}
@main struct Tests {
    @MainActor static func main() {
        let model = Model()
        var target: String?
        precondition(!model.sendQuickVoice("request", generation: UUID(), targetID: &target))
        precondition(target == nil && model.creations == 0 && model.draft.isEmpty)
        model.connected = false
        precondition(!model.sendQuickVoice("request", generation: model.generation, targetID: &target))
        model.connected = true; model.isDemo = true
        precondition(!model.sendQuickVoice("request", generation: model.generation, targetID: &target))
        precondition(model.creations == 0)
        model.isDemo = false
        precondition(!model.sendQuickVoice("Στείλε εργασία", generation: model.generation, targetID: &target))
        precondition(target == "draft-1" && model.creations == 1 && model.draft == "Στείλε εργασία")
        model.createdAgentIDs["draft-1"] = "remote-1"
        model.cards.removeAll { $0.id == "draft-1" }
        model.cards.append(.init(id: "remote-1"))
        model.select("existing")
        model.succeeds = true
        precondition(model.sendQuickVoice("Στείλε εργασία", generation: model.generation, targetID: &target))
        precondition(model.creations == 1 && model.sentTo == ["remote-1"])
        let oldGeneration = model.generation
        model.generation = UUID()
        precondition(!model.sendQuickVoice("stale", generation: oldGeneration, targetID: &target))
        precondition(model.sentTo == ["remote-1"])
        model.cards.removeAll { $0.id == "remote-1" }
        precondition(!model.sendQuickVoice("missing target", generation: model.generation, targetID: &target))
        precondition(model.creations == 1 && model.sentTo == ["remote-1"])
        print("PASS: account generation, readiness, transcript retention, remapped retry target")
    }
}
'''
with tempfile.TemporaryDirectory(prefix='quick-voice-test-') as directory:
    path = Path(directory)
    (path / 'Tests.swift').write_text(source)
    subprocess.run(['swiftc', '-parse-as-library', str(path / 'Tests.swift'), '-o', str(path / 'tests')], check=True)
    subprocess.run([str(path / 'tests')], check=True)
