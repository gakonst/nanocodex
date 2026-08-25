import assert from "node:assert/strict";
import test from "node:test";
import {
  RESIDENT_IDS,
  WORLD_PROTOCOL,
  isWorldAgentCommand,
  isWorldAgentMessage,
  type WorldObservation,
} from "../src/monsterWorldProtocol.ts";

const observation = (agentId: "cinder" | "june"): WorldObservation => ({
  stateVersion: agentId === "cinder" ? 7 : 11,
  minuteOfDay: 480,
  weather: "clear",
  self: {
    id: agentId,
    name: agentId === "cinder" ? "Cinder" : "June",
    role: agentId === "cinder" ? "Rescue scout" : "Courier",
    kind: agentId === "cinder" ? "monster" : "human",
    scene: "town",
    x: agentId === "cinder" ? 16 : 18,
    y: 11,
    direction: "down",
    location: "Guild Plaza",
    energy: 80,
    curiosity: 75,
    social: 70,
  },
  nearby: [],
  roster: [],
  guildBoard: [],
  recentEvents: [],
  availableTargets: ["plaza", "bridge", "player"],
  supplies: {
    orchardBerries: 8,
    shopStock: 1,
    guildSupplies: 0,
    trainingMarks: 0,
  },
});

test("one World call can carry the full resident task-tree roster", () => {
  assert.equal(RESIDENT_IDS.length, 48);
  assert.equal(RESIDENT_IDS.includes("june"), true);
  assert.equal(RESIDENT_IDS.includes("guest24"), true);

  const cinder = {
    protocol: WORLD_PROTOCOL,
    type: "call",
    requestId: "cinder-7",
    agentId: "cinder",
    residentIds: RESIDENT_IDS,
    observation: observation("cinder"),
  } as const;
  const june = { ...cinder, requestId: "june-11", agentId: "june", observation: observation("june") } as const;
  assert.equal(isWorldAgentCommand(cinder), true);
  assert.equal(isWorldAgentCommand(june), true);
  assert.equal(isWorldAgentCommand({ ...cinder, observation: undefined }), false);
});

test("resident settlements carry a typed failure reason", () => {
  assert.equal(isWorldAgentMessage({
    protocol: WORLD_PROTOCOL,
    type: "settled",
    requestId: "cinder-7",
    agentId: "cinder",
    outcome: "failed",
    failure: "transient",
    message: "temporary provider failure",
  }), true);
  assert.equal(isWorldAgentMessage({
    protocol: WORLD_PROTOCOL,
    type: "settled",
    requestId: "cinder-7",
    agentId: "cinder",
    outcome: "failed",
    failure: "made_up",
  }), false);
});

test("one resident action is correlated to its owning turn and fresh reducer result", () => {
  const action = {
    protocol: WORLD_PROTOCOL,
    type: "action",
    actionId: "cinder-action-1",
    requestId: "cinder-circle",
    agentId: "cinder",
    heardCallId: 12,
    action: { kind: "move_relative", anchor: "player", dx_pixels: 64, dy_pixels: 0 },
  } as const;
  assert.equal(isWorldAgentMessage(action), true);
  assert.equal(isWorldAgentMessage({ ...action, agentId: "june" }), true);
  assert.equal(isWorldAgentMessage({ ...action, action: { ...action.action, dx_pixels: 0 } }), false);
  assert.equal(isWorldAgentMessage({ ...action, action: { ...action.action, anchor: "nobody" } }), false);

  const maintain = {
    ...action,
    actionId: "cinder-maintain-1",
    action: {
      kind: "maintain_relative",
      anchor: "player",
      dx_pixels: -48,
      dy_pixels: 32,
      tolerance_pixels: 8,
    },
  } as const;
  assert.equal(isWorldAgentMessage(maintain), true);
  assert.equal(isWorldAgentMessage({
    ...maintain,
    action: { ...maintain.action, tolerance_pixels: 7 },
  }), false);

  const current = observation("cinder");
  const result = {
    protocol: WORLD_PROTOCOL,
    type: "action_result",
    actionId: action.actionId,
    requestId: action.requestId,
    agentId: action.agentId,
    result: {
      worldRevision: current.stateVersion,
      outcome: { status: "in_progress", action: action.action, detail: "moving into position" },
      self: current.self,
      nearby: current.nearby,
      roster: current.roster,
      relevantEvents: ["Cinder moved east."],
    },
  } as const;
  assert.equal(isWorldAgentCommand(result), true);
  assert.equal(isWorldAgentCommand({ ...result, agentId: "june" }), false);
  assert.equal(isWorldAgentCommand({ ...result, result: { ...result.result, worldRevision: -1 } }), false);
  assert.equal(isWorldAgentCommand({
    ...result,
    result: { ...result.result, roster: [{ id: "nobody" }] },
  }), false);
});

test("runtime action messages reject malformed physical actions", () => {
  const action = {
    protocol: WORLD_PROTOCOL,
    type: "action",
    actionId: "cinder-action",
    requestId: "cinder-turn",
    agentId: "cinder",
    action: { kind: "say", text: "On my way!" },
  } as const;
  assert.equal(isWorldAgentMessage(action), true);
  assert.equal(isWorldAgentMessage({ ...action, actionId: "" }), false);
  assert.equal(isWorldAgentMessage({ ...action, action: { kind: "say", text: "" } }), false);
  assert.equal(isWorldAgentMessage({ ...action, action: { kind: "wait", duration_ms: 300 } }), false);
});

test("the caller supplies one bounded authoritative participant roster", () => {
  const base = {
    protocol: WORLD_PROTOCOL,
    type: "call",
    requestId: "cinder-7",
    agentId: "cinder",
    residentIds: ["cinder", "june"],
    observation: observation("cinder"),
  } as const;
  assert.equal(isWorldAgentCommand(base), true);
  const sparseIds = new Array<string>(1);
  const invalidRosters: readonly unknown[] = [
    undefined,
    "cinder",
    [],
    ["cinder", "cinder"],
    ["june"],
    sparseIds,
    [...RESIDENT_IDS, "nobody"],
  ];
  for (const residentIds of invalidRosters) {
    assert.equal(isWorldAgentCommand({
      ...base,
      residentIds,
    }), false, JSON.stringify(residentIds));
  }
});

test("resident turns deeply reject malformed observations", () => {
  const valid = detailedObservation();
  const accepts = (candidate: unknown) => isWorldAgentCommand({
    protocol: WORLD_PROTOCOL,
    type: "call",
    requestId: "cinder-observation",
    agentId: "cinder",
    residentIds: ["cinder"],
    observation: candidate,
  });
  assert.equal(accepts(valid), true);

  const nearby = valid.nearby[0];
  const roster = valid.roster[0];
  const board = valid.guildBoard[0];
  assert.ok(nearby && roster && board && valid.guildCall);
  const sparseRoster = new Array<unknown>(2);
  sparseRoster[0] = roster;
  const malformed: readonly unknown[] = [
    { ...valid, stateVersion: -1 },
    { ...valid, minuteOfDay: 24 * 60 },
    { ...valid, weather: "hail" },
    { ...valid, self: { ...valid.self, kind: "player" } },
    { ...valid, self: { ...valid.self, energy: Number.NaN } },
    { ...valid, nearby: [{ ...nearby, distance: -1 }] },
    { ...valid, roster: [{ ...roster, activity: 12 }] },
    { ...valid, roster: sparseRoster },
    { ...valid, playerOrder: { id: 4, text: "", requestedTarget: "plaza" } },
    { ...valid, playerOrder: { id: 4, text: "Go", requestedTarget: "nowhere" } },
    { ...valid, guildCall: { ...valid.guildCall, requestedTarget: "nowhere" } },
    { ...valid, guildCall: { ...valid.guildCall, coListeners: [] } },
    { ...valid, guildCall: { ...valid.guildCall, coListeners: ["cinder", "cinder"] } },
    { ...valid, guildBoard: [{ ...board, fromName: 9 }] },
    { ...valid, recentEvents: ["Bell rang", 7] },
    { ...valid, availableTargets: ["plaza", "nowhere"] },
    { ...valid, supplies: { ...valid.supplies, shopStock: -1 } },
    { ...valid, supplies: Object.assign([], valid.supplies) },
  ];
  for (const candidate of malformed) assert.equal(accepts(candidate), false);
});

function detailedObservation(): WorldObservation {
  return {
    ...observation("cinder"),
    nearby: [{
      id: "player",
      name: "Scout",
      kind: "player",
      scene: "town",
      x: 16,
      y: 13,
      relativeX: 0,
      relativeY: 2,
      distance: 2.5,
      direction: "up",
      activity: "checking the board",
    }],
    roster: [{
      id: "cinder",
      name: "Cinder",
      kind: "monster",
      scene: "town",
      x: 16,
      y: 11,
      direction: "down",
      location: "Guild Plaza",
      activity: "waiting",
    }],
    guildCall: {
      id: 12,
      text: "Check Bell Bridge",
      voice: "call",
      distance: 2.5,
      radius: 12,
      guildWide: false,
      coListeners: ["cinder", "june"],
      requestedTarget: "bridge",
    },
    guildBoard: [{
      id: 4,
      fromId: "player",
      fromName: "Scout",
      toId: "cinder",
      toName: "Cinder",
      text: "Check Bell Bridge",
      minuteOfDay: 479,
      origin: "player",
      scope: "spatial",
    }],
    recentEvents: ["Scout called from the plaza"],
  };
}
