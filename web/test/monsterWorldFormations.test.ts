import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { WORLD_FORMATION_PROMPTS } from "../src/monsterWorldFormations.ts";
import { RESIDENT_IDS } from "../src/monsterWorldProtocol.ts";
import {
  createWorldState,
  observationFor,
  playerSpeak,
} from "../src/monsterWorldSimulation.ts";

test("Formation Lab presets are natural-language prompt helpers only", () => {
  assert.deepEqual(WORLD_FORMATION_PROMPTS.map(({ id }) => id), [
    "triangle",
    "square",
    "circle",
    "star",
    "double-ring",
  ]);
  for (const helper of WORLD_FORMATION_PROMPTS) {
    assert.ok(helper.prompt.length <= 140, `${helper.id} exceeds the World speech boundary`);
    assert.doesNotMatch(helper.prompt, /assigned|offset|slot|target point|answer key/i);
  }

  const architecture = [
    source("../src/MonsterWorld.tsx"),
    source("../src/monsterWorldAgent.worker.ts"),
    source("../src/monsterWorldFormations.ts"),
    source("../src/monsterWorldProtocol.ts"),
    source("../src/monsterWorldSimulation.ts"),
    source("../src/monsterWorldRenderer.ts"),
  ].join("\n");
  assert.doesNotMatch(
    architecture,
    /formationKindForPrompt|formationOffset|formationPathGroups|WorldFormationFeedback|assignedOffset|assignedPosition|worldFormationProgress|drawFormationGuide/,
  );

  const component = source("../src/MonsterWorld.tsx");
  assert.match(component, /const submitDialogue[\s\S]*?issueDialogue\(draft\)/);
  assert.match(component, /const sendFormationPrompt[\s\S]*?issueDialogue\(preset\.prompt\)/);
  assert.match(component, /playerSpeak\(worldRef\.current, input, voiceLevel\)/);
  assert.doesNotMatch(component, /playerSpeak\(worldRef\.current, input, voiceLevel,\s*"reducer"\)/);
  assert.doesNotMatch(component, /speech\.order/);

  const worker = source("../src/monsterWorldAgent.worker.ts");
  assert.match(worker, /persistent task tree/);
  assert.match(worker, /const SQUADS = Object\.freeze\(Array\.from\(\{ length: 6 \}/);
  assert.match(worker, /canonical subagent messages/);
  assert.match(worker, /anchor-relative maintenance/);
  assert.match(worker, /Subagents\.create\(\{ maxConcurrency: 48 \}\)/);
  assert.match(worker, /Cover every supplied squad leader exactly once/);
  assert.match(worker, /The setup must implement the raw objective rather than assuming a circle/);
  assert.doesNotMatch(worker, /exec_command|messages\.jsonl|CALL-<id> CONTRACT/);
  assert.doesNotMatch(worker, /independently derive your group and place/);
  assert.doesNotMatch(worker, /phasePositionAction|distance_pixels|Math\.cos\(radians\)/);
});

test("helper and arbitrary formation language never becomes reducer-owned destinations", () => {
  const prompts = [
    ...WORLD_FORMATION_PROMPTS.map(({ prompt }) => prompt),
    "Form 6 groups of 8 residents. Each group of 8 should make its own square.",
    "Everyone move into a circle around me.",
    "Everyone gather in a square around Scout.",
    "Everyone walk into six groups around me.",
  ];

  for (const prompt of prompts) {
    const state = createWorldState();
    for (const id of RESIDENT_IDS) state.actors[id].presence = "active";
    const speech = playerSpeak(state, prompt, "call");
    assert.ok(speech, prompt);
    assert.equal(speech.order, undefined, prompt);
    assert.equal(speech.liveAddressed.length, 48, prompt);
    assert.deepEqual(state.orders, [], prompt);

    for (const id of RESIDENT_IDS) {
      const actor = state.actors[id];
      const observation = observationFor(state, id);
      assert.equal(observation.playerOrder?.text, prompt, `${prompt}: ${id}`);
      assert.deepEqual(observation.playerOrder?.coListeners, speech.liveAddressed, `${prompt}: ${id}`);
      assert.equal(observation.playerOrder?.requestedTarget, undefined, `${prompt}: ${id}`);
      assert.equal(observation.guildCall?.requestedTarget, undefined, `${prompt}: ${id}`);
      assert.equal(actor.activeOrderId, undefined, `${prompt}: ${id}`);
      assert.equal(actor.tasks.length, 0, `${prompt}: ${id}`);
      assert.equal(actor.movement, undefined, `${prompt}: ${id}`);
      assert.equal("formation" in observation, false, `${prompt}: ${id}`);

    }
  }
});

test("task-tree completion requires current reducer evidence from every resident", () => {
  const worker = source("../src/monsterWorldAgent.worker.ts");
  assert.match(worker, /feedback: Map<ResidentId, ResidentActEvidence>/);
  assert.match(worker, /validateCoordinationCompletion\(active, result\.finalMessage\)/);
  assert.match(worker, /World coordination completed without fresh action evidence/);
  assert.match(worker, /result\.remainingGaps\.length !== 0/);
  assert.match(worker, /evidence\.worldRevision !== latest\.result\.worldRevision/);
  assert.doesNotMatch(worker, /full roster, current order/);
});

function source(path: string): string {
  return readFileSync(new URL(path, import.meta.url), "utf8");
}
