import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  RESIDENT_IDS,
  VOICE_RADIUS,
  WORLD_PROTOCOL,
  decodeStagedPlan,
  isWorldAgentMessage,
} from "../src/monsterWorldProtocol.ts";
import {
  BASE_RESIDENT_COUNT,
  MAX_RESIDENT_COUNT,
  activeResidentCount,
  actorWorldPosition,
  applyWorldPlan,
  applyWorldRoomSend,
  applyWorldToolAction,
  createWorldState,
  hasUnansweredGuildCall,
  hasUnansweredPlayerOrder,
  isGuildRelayActive,
  movePlayer,
  observationFor,
  playerSpeak,
  requestResidentExit,
  residentAtWorldPoint,
  serializeWorldState,
  setPopulationTarget,
  updateWorld,
} from "../src/monsterWorldSimulation.ts";

const component = source("../src/MonsterWorld.tsx");
const worldCss = source("../src/MonsterWorld.css");
const worker = source("../src/monsterWorldAgent.worker.ts");
const application = source("../src/NanocodexApp.tsx");
const routeLoaders = source("../src/routeLoaders.ts");
const attribution = source("../public/world/ATTRIBUTION.md");

test("world plans are bounded, versioned, and normalized before entering the simulation", () => {
  const expected = { requestId: "turn-1", agentId: "moss", stateVersion: 7 } as const;
  const plan = decodeStagedPlan({
    request_id: "turn-1",
    agent_id: "moss",
    state_version: 7,
    summary: "checks the bridge for silver dust",
    steps: [
      { kind: "move", target: "bridge" },
      { kind: "say", text: "  The current changed.\nLook east!  ", to: "rill" },
      { kind: "interact", target: "bridge", action: "inspect" },
    ],
  }, expected);

  assert.equal(plan.protocol, WORLD_PROTOCOL);
  assert.equal(plan.origin, "nanocodex");
  assert.equal(plan.steps[1]?.kind === "say" && plan.steps[1].text, "The current changed. Look east!");
  assert.ok(Object.isFrozen(plan));
  assert.throws(
    () => decodeStagedPlan({
      request_id: "turn-1",
      agent_id: "moss",
      state_version: 6,
      summary: "stale",
      steps: [{ kind: "move", target: "bridge" }],
    }, expected),
    /state_version is stale/,
  );
  assert.throws(
    () => decodeStagedPlan({
      request_id: "turn-1",
      agent_id: "moss",
      state_version: 7,
      summary: "escapes the map",
      steps: [{ kind: "move", target: "internet" }],
    }, expected),
    /not an allowed value/,
  );
  assert.throws(
    () => decodeStagedPlan({
      request_id: "turn-1",
      agent_id: "moss",
      state_version: 7,
      summary: "ignores Scout's destination",
      steps: [{ kind: "move", target: "bridge" }],
    }, { ...expected, requestedTarget: "player" }),
    /must physically act at player/,
  );
  assert.throws(
    () => decodeStagedPlan({
      request_id: "turn-1",
      agent_id: "moss",
      state_version: 7,
      summary: "pretends to move",
      steps: [{ kind: "move_relative", anchor: "player", dx_pixels: 0, dy_pixels: 0 }],
    }, expected),
    /must change at least one axis/,
  );
  assert.throws(
    () => decodeStagedPlan({
      request_id: "turn-1",
      agent_id: "moss",
      state_version: 7,
      summary: "returns an empty random branch",
      steps: [{
        kind: "random_choice",
        chance_percent: 50,
        true_label: "heads",
        false_label: "tails",
        if_true: [],
        if_false: [{ kind: "emote", icon: "?" }],
      }],
    }, expected),
    /must contain 1-3 physical actions/,
  );
});

test("the reducer owns movement, mission effects, stale rejection, and idempotency", () => {
  const state = createWorldState();
  const observation = observationFor(state, "moss");
  const plan = decodeStagedPlan({
    request_id: "moss-1",
    agent_id: "moss",
    state_version: observation.stateVersion,
    summary: "follows silver dust into the orchard",
    steps: [
      { kind: "move", target: "orchard" },
      { kind: "interact", target: "orchard", action: "gather" },
      { kind: "emote", icon: "spark" },
    ],
  }, {
    requestId: "moss-1",
    agentId: "moss",
    stateVersion: observation.stateVersion,
  });

  assert.deepEqual(applyWorldPlan(state, plan), { accepted: true });
  assert.deepEqual(applyWorldPlan(state, plan), { accepted: false, reason: "duplicate" });
  const stale = decodeStagedPlan({
    request_id: "moss-2",
    agent_id: "moss",
    state_version: observation.stateVersion,
    summary: "repeats an old thought",
    steps: [{ kind: "emote", icon: "?" }],
  }, {
    requestId: "moss-2",
    agentId: "moss",
    stateVersion: observation.stateVersion,
  });
  assert.deepEqual(applyWorldPlan(state, stale), { accepted: false, reason: "stale" });

  for (let index = 0; index < 1_000 && state.mission.stage < 1; index += 1) {
    updateWorld(state, 100);
  }
  assert.ok(state.mission.stage >= 1);
  assert.doesNotMatch(state.mission.title, /bell beneath the water/i);
  assert.ok(state.activities.some(({ origin, text }) =>
    origin === "nanocodex" && /Moss decided/.test(text)
  ));
  assert.ok(state.activities.some(({ text }) => /silver dust/i.test(text)));

  const before = { x: state.actors.player.x, y: state.actors.player.y };
  assert.equal(movePlayer(state, "left"), true);
  updateWorld(state, 100);
  updateWorld(state, 100);
  assert.deepEqual(
    { x: state.actors.player.x, y: state.actors.player.y },
    { x: before.x - 1, y: before.y },
  );
  assert.equal(JSON.parse(serializeWorldState(state)).version, 3);
});

test("voice is spatial off-center and guild-wide at the central relay", () => {
  const relay = createWorldState();
  assert.equal(Object.keys(relay.actors).length, RESIDENT_IDS.length + 1);
  assert.equal(RESIDENT_IDS.length, 48);
  assert.equal(BASE_RESIDENT_COUNT, 36);
  assert.equal(MAX_RESIDENT_COUNT, 48);
  assert.equal(activeResidentCount(relay), BASE_RESIDENT_COUNT);
  assert.equal(isGuildRelayActive(relay), true);
  const activeIds = RESIDENT_IDS.slice(0, BASE_RESIDENT_COUNT);
  const before = Object.fromEntries(activeIds.map((id) => [id, relay.decisionVersions[id]]));
  const broadcast = playerSpeak(relay, "Everyone meet at the bridge.", "whisper");
  assert.ok(broadcast);
  assert.equal(broadcast.guildWide, true);
  assert.equal(broadcast.heardBy.length, BASE_RESIDENT_COUNT);
  assert.deepEqual(broadcast.liveHeardBy, activeIds);
  for (const id of activeIds) {
    assert.equal(relay.decisionVersions[id], (before[id] ?? 0) + 1);
    const observation = observationFor(relay, id);
    assert.equal(observation.playerOrder?.text, "Everyone meet at the bridge.");
    assert.equal(observation.guildCall?.text, "Everyone meet at the bridge.");
    assert.equal(observation.guildBoard[0]?.text, "Everyone meet at the bridge.");
    assert.equal(relay.actors[id].listenerPulse?.callId, broadcast.callId);
    assert.equal(hasUnansweredGuildCall(relay, id), true);
  }

  const local = createWorldState();
  local.actors.player.x = 1;
  local.actors.player.y = 22;
  local.actors.cinder.x = 2;
  local.actors.cinder.y = 22;
  assert.equal(isGuildRelayActive(local), false);
  const mossVersion = local.decisionVersions.moss;
  const whisper = playerSpeak(local, "Cinder, stay close.", "whisper");
  assert.ok(whisper);
  assert.equal(whisper.guildWide, false);
  assert.equal(whisper.radius, VOICE_RADIUS.whisper);
  assert.deepEqual(whisper.liveHeardBy, ["cinder"]);
  const distant = observationFor(local, "moss");
  assert.equal(distant.playerOrder?.text, "Cinder, stay close.");
  assert.equal(distant.guildCall, undefined);
  assert.equal(distant.guildBoard.some(({ text }) => text === "Cinder, stay close."), true);
  assert.equal(distant.recentEvents.some((text) => text.includes("Cinder, stay close.")), false);
  assert.equal(local.actors.cinder.listenerPulse?.callId, whisper.callId);
  assert.equal(local.decisionVersions.moss, mossVersion + 1);
});

test("a reducer-owned Scout order blocks model overwrite until physical completion", () => {
  const state = createWorldState();
  const staleObservation = observationFor(state, "cinder");
  const speech = playerSpeak(state, "Cinder, inspect the gate.", "call", "reducer");
  assert.ok(speech);
  assert.equal(observationFor(state, "cinder").guildCall?.id, speech.callId);

  const stale = decodeStagedPlan({
    request_id: "old-cinder",
    agent_id: "cinder",
    state_version: staleObservation.stateVersion,
    summary: "continues an old patrol",
    steps: [{ kind: "emote", icon: "?" }],
  }, {
    requestId: "old-cinder",
    agentId: "cinder",
    stateVersion: staleObservation.stateVersion,
  });
  assert.deepEqual(applyWorldPlan(state, stale), { accepted: false, reason: "stale" });
  assert.equal(hasUnansweredGuildCall(state, "cinder"), true);

  const activeObservation = observationFor(state, "cinder");

  const premature = decodeStagedPlan({
    request_id: "premature-call-cinder",
    agent_id: "cinder",
    state_version: activeObservation.stateVersion,
    summary: "answers Scout and checks the gate",
    steps: [{ kind: "say", text: "On it!" }, { kind: "move", target: "dungeon_gate" }],
  }, {
    requestId: "premature-call-cinder",
    agentId: "cinder",
    stateVersion: activeObservation.stateVersion,
    heardCallId: activeObservation.guildCall?.id,
    requestedTarget: activeObservation.guildCall?.requestedTarget,
  });
  assert.deepEqual(applyWorldPlan(state, premature), { accepted: false, reason: "stale" });
  advanceOrderToTerminal(state, speech.order?.id);

  const completedObservation = observationFor(state, "cinder");
  const response = decodeStagedPlan({
    request_id: "call-cinder",
    agent_id: "cinder",
    state_version: completedObservation.stateVersion,
    summary: "reports the completed gate assignment",
    steps: [{ kind: "say", text: "At the gate, Scout!" }, { kind: "move", target: "dungeon_gate" }],
  }, {
    requestId: "call-cinder",
    agentId: "cinder",
    stateVersion: completedObservation.stateVersion,
    heardCallId: completedObservation.guildCall?.id,
    requestedTarget: completedObservation.guildCall?.requestedTarget,
  });
  assert.deepEqual(applyWorldPlan(state, response), { accepted: true });
  assert.equal(hasUnansweredGuildCall(state, "cinder"), false);
  assert.equal(observationFor(state, "cinder").guildCall, undefined);
});

test("exact gather and split orders physically complete offline for every active resident", () => {
  const state = createWorldState();
  const gather = playerSpeak(state, "Everyone come to me.", "whisper", "reducer");
  assert.ok(gather);
  assert.ok(gather.order);
  assert.equal(state.agentsOnline, false);
  assert.equal(gather.order.assigned.length, BASE_RESIDENT_COUNT);
  assert.equal(gather.order.rejected.length, 0);
  const activeIds = RESIDENT_IDS.slice(0, BASE_RESIDENT_COUNT);
  assert.deepEqual(gather.liveAddressed, activeIds);
  assert.ok(orderById(state, gather.order.id).assignments.every(({ status }) => status === "assigned"));
  for (const id of activeIds) {
    const observation = observationFor(state, id);
    assert.equal(observation.roster.length, BASE_RESIDENT_COUNT + 1);
    assert.equal(observation.roster.some(({ id: actorId }) => actorId === "player"), true);
    assert.equal(observation.guildBoard[0]?.text, "Everyone come to me.");
    assert.equal(observation.guildCall?.requestedTarget, "player");
  }
  updateWorld(state, 100);
  const gatherOrder = orderById(state, gather.order.id);
  assert.ok(gatherOrder.assignments.some(({ status }) => status === "moving"));
  advanceOrderToTerminal(state, gather.order.id);
  assertOrderCompletedAtGoals(state, gather.order.id);

  const split = createWorldState();
  const command = playerSpeak(
    split,
    "Cinder, Moss, and Rill go to the bridge; everyone else go to the pond.",
    "talk",
    "reducer",
  );
  assert.ok(command);
  assert.ok(command.order);
  assert.equal(command.order.assigned.length, BASE_RESIDENT_COUNT);
  assert.equal(command.order.rejected.length, 0);
  assert.deepEqual(command.liveAddressed, activeIds);
  for (const id of ["cinder", "moss", "rill"] as const) {
    assert.equal(observationFor(split, id).guildCall?.requestedTarget, "bridge");
  }
  for (const id of ["luma", "iris", "rook"] as const) {
    assert.equal(observationFor(split, id).guildCall?.requestedTarget, "pond");
  }
  const splitOrder = orderById(split, command.order.id);
  for (const assignment of splitOrder.assignments) {
    const expected = (["cinder", "moss", "rill"] as readonly string[]).includes(assignment.actorId)
      ? "bridge"
      : "pond";
    assert.equal(assignment.target, expected, assignment.actorId);
  }
  advanceOrderToTerminal(split, command.order.id);
  assertOrderCompletedAtGoals(split, command.order.id);
});

test("a recognized order preempts old tasks and an in-flight tile before moving", () => {
  const state = createWorldState();
  const observation = observationFor(state, "cinder");
  const oldPlan = decodeStagedPlan({
    request_id: "old-bridge-patrol",
    agent_id: "cinder",
    state_version: observation.stateVersion,
    summary: "continues an old bridge patrol",
    steps: [{ kind: "move", target: "bridge" }],
  }, {
    requestId: "old-bridge-patrol",
    agentId: "cinder",
    stateVersion: observation.stateVersion,
  });
  assert.deepEqual(applyWorldPlan(state, oldPlan), { accepted: true });
  updateWorld(state, 100);
  assert.ok(state.actors.cinder.movement);
  const affectedResidents = RESIDENT_IDS.slice(0, BASE_RESIDENT_COUNT);
  const versions = Object.fromEntries(affectedResidents.map((id) => [id, state.decisionVersions[id]]));

  const speech = playerSpeak(state, "Everyone come to me", "call", "reducer");
  assert.ok(speech?.order);
  assert.equal(state.actors.cinder.movement, undefined);
  assert.equal(state.actors.cinder.tasks.length, 1);
  assert.equal(state.actors.cinder.tasks[0]?.orderId, speech.order.id);
  assert.equal(state.actors.cinder.tasks[0]?.requestId.includes("old-bridge-patrol"), false);
  assert.equal(
    state.actors.cinder.tasks[0]?.action.kind === "move" && state.actors.cinder.tasks[0].action.target,
    "player",
  );
  for (const id of affectedResidents) {
    assert.equal(state.decisionVersions[id], (versions[id] ?? 0) + 1, id);
  }
});

test("a newer order preempts unfinished assignments and fences a late model result", () => {
  const state = createWorldState();
  const gather = playerSpeak(state, "Everyone come to me", "call", "reducer");
  assert.ok(gather?.order);
  updateWorld(state, 100);
  const oldObservation = observationFor(state, "cinder");
  const latePlan = decodeStagedPlan({
    request_id: "late-gather-response",
    agent_id: "cinder",
    state_version: oldObservation.stateVersion,
    summary: "continues the superseded gather",
    steps: [{ kind: "move", target: "player" }],
  }, {
    requestId: "late-gather-response",
    agentId: "cinder",
    stateVersion: oldObservation.stateVersion,
    heardCallId: oldObservation.guildCall?.id,
    requestedTarget: oldObservation.guildCall?.requestedTarget,
  });

  const split = playerSpeak(
    state,
    "Cinder, Moss, and Rill go to the bridge; everyone else go to the pond",
    "call",
    "reducer",
  );
  assert.ok(split?.order);
  const oldOrder = orderById(state, gather.order.id);
  assert.ok(oldOrder.assignments.every(({ status }) => status === "preempted" || status === "completed"));
  for (const id of RESIDENT_IDS.filter((id) => state.actors[id].presence === "active")) {
    assert.equal(state.actors[id].movement, undefined, id);
    assert.equal(state.actors[id].tasks[0]?.orderId, split.order.id, id);
  }
  assert.deepEqual(applyWorldPlan(state, latePlan), { accepted: false, reason: "stale" });
  assert.equal(state.actors.cinder.tasks[0]?.orderId, split.order.id);
});

test("raw speech reaches every resident mind without inventing a reducer destination", () => {
  const state = createWorldState();
  const versions = { ...state.decisionVersions };
  const speech = playerSpeak(state, "Everyone dance in a circle", "shout");
  assert.ok(speech);
  assert.equal(speech.order, undefined);
  const activeIds = RESIDENT_IDS.slice(0, BASE_RESIDENT_COUNT);
  assert.deepEqual(speech.liveAddressed, activeIds);
  assert.equal(state.orders.length, 0);
  for (const id of activeIds) {
    assert.equal(state.decisionVersions[id], versions[id] + 1, id);
    assert.equal(hasUnansweredPlayerOrder(state, id), true, id);
    assert.equal(hasUnansweredGuildCall(state, id), true, id);
    assert.equal(observationFor(state, id).guildCall?.text, "Everyone dance in a circle", id);
    assert.equal(observationFor(state, id).guildCall?.requestedTarget, undefined, id);
  }
  assert.ok(RESIDENT_IDS.every((id) => state.actors[id].activeOrderId === undefined));
  assert.equal(state.guildMessages[0]?.text, "Everyone dance in a circle");

  const casual = playerSpeak(state, "I like the water today.", "talk");
  assert.ok(casual);
  assert.equal(casual.order, undefined);
  assert.deepEqual(casual.liveAddressed, activeIds);

  const withArrival = createWorldState();
  setPopulationTarget(withArrival, BASE_RESIDENT_COUNT + 1);
  const rejected = playerSpeak(withArrival, "Mika go to the bridge", "call", "reducer");
  assert.ok(rejected?.order);
  assert.equal(rejected.order.assigned.length, 0);
  assert.deepEqual(rejected.order.rejected.map(({ actorId, reason }) => ({ actorId, reason })), [
    { actorId: "guest13", reason: "not-active" },
  ]);
  assert.equal(orderById(withArrival, rejected.order.id).assignments[0]?.status, "rejected");
});

test("a resident can interpret a coin flip into an independently sampled relative move", () => {
  const state = createWorldState();
  const speech = playerSpeak(
    state,
    "Everyone flip a coin: heads gather 50px to my right, tails vice versa.",
    "call",
  );
  assert.ok(speech);
  assert.equal(speech.order, undefined);
  const observation = observationFor(state, "cinder");
  assert.ok(observation.guildCall);
  const plan = decodeStagedPlan({
    request_id: "cinder-freeform-coin",
    agent_id: "cinder",
    state_version: observation.stateVersion,
    summary: "flips a coin and takes the matching side",
    steps: [{
      kind: "random_choice",
      chance_percent: 50,
      true_label: "heads",
      false_label: "tails",
      if_true: [{ kind: "move_relative", anchor: "player", dx_pixels: 50, dy_pixels: 0 }],
      if_false: [{ kind: "move_relative", anchor: "player", dx_pixels: -50, dy_pixels: 0 }],
    }],
  }, {
    requestId: "cinder-freeform-coin",
    agentId: "cinder",
    stateVersion: observation.stateVersion,
    heardCallId: observation.guildCall.id,
  });
  assert.deepEqual(applyWorldPlan(state, plan), { accepted: true });
  assert.equal(hasUnansweredGuildCall(state, "cinder"), false);

  for (let index = 0; index < 300; index += 1) {
    updateWorld(state, 100);
    if (!state.actors.cinder.movement && state.actors.cinder.tasks.length === 0) break;
  }
  const destination = actorWorldPosition(state.actors.cinder);
  assert.equal(destination.scene, "town");
  assert.equal(destination.y, 13);
  assert.ok(destination.x === 10 || destination.x === 22, JSON.stringify(destination));
  assert.ok(state.activities.some(({ text }) => /Cinder's random choice was (heads|tails)\./.test(text)));
});

test("imperative orders preserve interactions and can target another resident", () => {
  const interaction = createWorldState();
  const inspect = playerSpeak(interaction, "Cinder, inspect the gate.", "call", "reducer")?.order;
  assert.ok(inspect);
  assert.equal(inspect.assigned[0]?.actorId, "cinder");
  assert.equal(inspect.assigned[0]?.interaction, "inspect");
  assert.deepEqual(interaction.actors.cinder.tasks[0]?.action, {
    kind: "interact",
    target: "dungeon_gate",
    action: "inspect",
  });
  advanceOrderToTerminal(interaction, inspect.id);
  assert.equal(orderById(interaction, inspect.id).assignments[0]?.status, "completed");
  assert.ok(interaction.activities.some(({ text }) => /Cinder inspects Mystery Gate\./.test(text)));

  const actorTarget = createWorldState();
  const meet = playerSpeak(actorTarget, "Cinder, go to Moss.", "call", "reducer")?.order;
  assert.ok(meet);
  assert.deepEqual(meet.assigned.map(({ actorId, target }) => ({ actorId, target })), [
    { actorId: "cinder", target: "moss" },
  ]);
  advanceOrderToTerminal(actorTarget, meet.id);
  assertOrderCompletedAtGoals(actorTarget, meet.id);
});

test("mixed accepted and rejected assignments settle without claiming full completion", () => {
  const state = createWorldState();
  setPopulationTarget(state, BASE_RESIDENT_COUNT + 1);
  const receipt = playerSpeak(state, "Cinder and Mika go to the bridge.", "call", "reducer")?.order;
  assert.ok(receipt);
  assert.equal(receipt.assigned.length, 1);
  assert.equal(receipt.rejected.length, 1);
  advanceOrderToTerminal(state, receipt.id);
  assert.ok(state.activities.some(({ text }) =>
    text === `Scout's order ${receipt.id} settled: 1/2 completed; 1 rejected; 0 preempted.`
  ));
  assert.equal(state.activities.some(({ text }) =>
    text === `Scout's order ${receipt.id} complete: 1/1 residents arrived.`
  ), false);
});

test("local speech stays embodied while a room post enters the shared reducer log", () => {
  const state = createWorldState();
  const initialPosts = state.guildMessages.length;
  const observation = observationFor(state, "cinder");
  const plan = decodeStagedPlan({
    request_id: "cinder-to-moss",
    agent_id: "cinder",
    state_version: observation.stateVersion,
    summary: "asks Moss to compare the silver trail",
    steps: [{ kind: "say", text: "Moss, compare this dust with the orchard sample.", to: "moss" }],
  }, {
    requestId: "cinder-to-moss",
    agentId: "cinder",
    stateVersion: observation.stateVersion,
  });

  assert.deepEqual(applyWorldPlan(state, plan), { accepted: true });
  updateWorld(state, 100);
  assert.equal(state.actors.cinder.bubble?.text, "Moss, compare this dust with the orchard sample.");
  assert.equal(state.guildMessages.length, initialPosts);

  const roomPost = applyWorldRoomSend(state, {
    sendId: "cinder-room-post",
    requestId: "cinder-room-turn",
    agentId: "cinder",
    text: "Moss, compare this dust with the orchard sample.",
  });
  assert.equal(roomPost.accepted, true);
  const message = state.guildMessages[0];
  assert.equal(message?.fromId, "cinder");
  assert.equal(message?.toId, undefined);
  assert.equal(message?.origin, "nanocodex");
  assert.equal(observationFor(state, "moss").guildBoard[0]?.id, message?.id);
  assert.equal(observationFor(state, "moss").guildBoard[0]?.fromName, "Cinder");

  const restored = createWorldState(serializeWorldState(state));
  assert.equal(restored.guildMessages[0]?.text, message?.text);
  assert.ok(restored.nextGuildMessageId > (message?.id ?? 0));
});

test("population changes enter from outside, remain physical, and cross an edge before removal", () => {
  const state = createWorldState();
  assert.equal(activeResidentCount(state), BASE_RESIDENT_COUNT);
  assert.equal(setPopulationTarget(state, Number.NaN).target, BASE_RESIDENT_COUNT);
  assert.equal(state.populationTarget, BASE_RESIDENT_COUNT);
  assert.ok(RESIDENT_IDS.slice(0, BASE_RESIDENT_COUNT).every((id) => (
    state.actors[id].presence === "active"
  )));
  assert.ok(RESIDENT_IDS.slice(BASE_RESIDENT_COUNT).every((id) => (
    state.actors[id].presence === "absent"
  )));

  const increase = setPopulationTarget(state, BASE_RESIDENT_COUNT + 3);
  assert.equal(increase.entering.length, 3);
  for (const id of increase.entering) {
    const actor = state.actors[id];
    assert.equal(actor.presence, "entering");
    assert.ok(actor.x < 0 || actor.x >= 32 || actor.y < 0 || actor.y >= 24);
  }
  for (let index = 0; index < 20; index += 1) updateWorld(state, 100);
  assert.equal(activeResidentCount(state), BASE_RESIDENT_COUNT + 3);
  assert.ok(increase.entering.every((id) => state.actors[id].presence === "active"));
  const restored = createWorldState(serializeWorldState(state));
  assert.equal(restored.populationTarget, BASE_RESIDENT_COUNT + 3);
  assert.equal(activeResidentCount(restored), BASE_RESIDENT_COUNT + 3);

  const selected = increase.entering[0];
  assert.ok(selected);
  state.actors[selected].x = 30;
  state.actors[selected].y = 12;
  state.actors[selected].movement = undefined;
  assert.equal(residentAtWorldPoint(state, 30, 11), selected);
  assert.equal(requestResidentExit(state, selected), true);
  assert.equal(state.actors[selected].presence, "exiting");
  assert.match(state.actors[selected].bubble?.text ?? "", /gotta get out/i);
  assert.equal(activeResidentCount(state), BASE_RESIDENT_COUNT + 3);
  assert.equal(state.populationTarget, BASE_RESIDENT_COUNT + 2);
  for (let index = 0; index < 240; index += 1) updateWorld(state, 100);
  assert.equal(state.actors[selected].presence, "absent");
  assert.equal(activeResidentCount(state), BASE_RESIDENT_COUNT + 2);

  const leaveAll = setPopulationTarget(state, 0);
  assert.equal(leaveAll.exiting.length, BASE_RESIDENT_COUNT + 2);
  assert.equal(activeResidentCount(state), BASE_RESIDENT_COUNT + 2);
  for (let index = 0; index < 600; index += 1) updateWorld(state, 100);
  assert.equal(activeResidentCount(state), 0);

  const fillTown = setPopulationTarget(state, MAX_RESIDENT_COUNT);
  assert.equal(fillTown.entering.length, MAX_RESIDENT_COUNT);
  for (let index = 0; index < 20; index += 1) updateWorld(state, 100);
  assert.equal(activeResidentCount(state), MAX_RESIDENT_COUNT);

  const fullGuildOrder = playerSpeak(state, "Everyone go to the guild hall.", "call", "reducer")?.order;
  assert.ok(fullGuildOrder);
  assert.equal(fullGuildOrder.assigned.length, MAX_RESIDENT_COUNT);
  assert.deepEqual(fullGuildOrder.rejected, []);
  advanceOrderToTerminal(state, fullGuildOrder.id);
  assertOrderCompletedAtGoals(state, fullGuildOrder.id);
  assert.ok(RESIDENT_IDS.every((id) => state.actors[id].scene === "guild_hall"));
});

test("the World is one Rust subagent task tree with one embodied session per resident", () => {
  assert.match(worker, /from "nanocodex\/host"/);
  assert.match(worker, /Agent, Subagents, Transport/);
  assert.match(worker, /toolMode: "direct"/);
  assert.doesNotMatch(worker, /harness|web__run|image_gen/);
  assert.doesNotMatch(worker, /justBash|exec_command|messages\.jsonl/);
  assert.match(worker, /name: "act"/);
  assert.doesNotMatch(worker, /\n\s{6}(?:move|maintain|interact|emote):\s*\{/);
  assert.doesNotMatch(worker, /\n\s*say:\s*\{/);
  assert.doesNotMatch(worker, /read_chat|send_chat/);
  assert.doesNotMatch(worker, /residentAgents|residentBoots|createResidentAgent/);
  assert.match(worker, /coordinatorBoot \?\?= Agent\.create/);
  assert.match(worker, /Subagents\.create\(\{ maxConcurrency: 48 \}\)/);
  assert.match(worker, /six stable squads of eight/);
  assert.match(worker, /world-leader:<resident-id>/);
  assert.match(worker, /world-resident:<resident-id>/);
  assert.match(worker, /send_agent_message purpose=delegate/);
  assert.match(worker, /decodeWorldPrimitiveAction/);
  assert.doesNotMatch(worker, /\n\s*observe:\s*\{/);
  assert.doesNotMatch(worker, /WAIT_PARAMETERS|kind: "wait"/);
  assert.match(worker, /result = await turn\.result\(\)/);
  assert.match(worker, /agent\.turn\.prompt\(\{ input: coordinatorPrompt\(active\) \}\)/);
  assert.doesNotMatch(worker, /agent\.turn\.prompt\(\{\s*id:/);
  assert.match(worker, /residentBySubagent = new Map<string, ResidentId>/);
  assert.match(worker, /subagentByResident = new Map<ResidentId, string>/);
  assert.match(worker, /context\.subagent/);
  assert.match(worker, /already bound to another task-tree agent/);
  assert.match(worker, /otherSquadLeaders/);
  assert.match(worker, /RESULT_SCHEMA/);
  assert.match(worker, /type: "action"[\s\S]*?actionId[\s\S]*?action/);
  assert.match(worker, /function resolveWorldAction/);
  assert.match(worker, /const pendingWorldActions = new Map<string, PendingWorldAction>\(\)/);
  assert.doesNotMatch(worker, /MAX_(?:COMPLETED|ATTEMPTED)_TURNS|MAX_TOTAL_TOKENS|budgetFailureMessage/);
  assert.match(worker, /model: "gpt-5\.6-luna"/);
  assert.match(worker, /thinking: "none"/);
  assert.doesNotMatch(worker, /session\.spawn|LANE_COUNT/);
  assert.match(worker, /async function shutdownWorld[\s\S]*?retained\.session\.shutdown\(\)/);
  assert.doesNotMatch(worker, /usage_limit|tripUsageLimit|blocked = true/);
});

test("the World surface stays statically available, stoppable, and semantically observable", () => {
  assert.doesNotMatch(routeLoaders, /import\(/);
  assert.match(routeLoaders, /surface === "world"\) \{\s*return \{\};\s*\}/);
  assert.doesNotMatch(routeLoaders, /loadWorldAssets/);
  assert.match(application, /import \{ MonsterWorld \} from "\.\/MonsterWorld"/);
  assert.match(component, /new Worker\(new URL\("\.\/monsterWorldAgent\.worker\.ts"/);
  assert.match(component, /document\.visibilityState === "hidden"[\s\S]*?stopAgents\(\)/);
  assert.match(component, /type: "shutdown"/);
  assert.match(component, /worker\.terminate\(\)/);
  assert.match(component, /wake"\} \$\{onMapMindIds\.length\} minds/);
  assert.match(component, /Orchestrate by voice/);
  assert.match(component, /Q cycles loudness/);
  assert.doesNotMatch(component, /MAX_CONCURRENT_RESIDENT_TURNS|slice\(0,\s*6\)/);
  assert.match(component, /pendingRequests\.current\.set\(request\.requestId, request\)/);
  assert.match(component, /const callId = worldObservationCallId\(observation\)/);
  assert.match(component, /for \(const residentId of request\.residentIds\)[\s\S]*?completeResidentInstruction/);
  assert.doesNotMatch(component, /turn slots/);
  assert.doesNotMatch(component, /MAX_MODEL_TURNS|MAX_AGENT_TOKENS|modelBudgetExhausted/);
  assert.match(component, /type: "call"/);
  assert.match(component, /worldToolResultAtDecisionBoundary/);
  assert.match(component, /type: "action_result"/);
  assert.doesNotMatch(component, /batch/i);
  assert.match(component, /Semantic event stream/);
  assert.match(component, /Scene dialogue/);
  assert.match(component, /if \(mindsToWake\.length > 0\) startAgents\(\)/);
  assert.match(component, /residentIds: request\.residentIds/);
  assert.match(component, /A newer World call replaced this action/);
  assert.match(component, /ask to leave/);
  assert.match(component, /type="range"/);
  assert.doesNotMatch(component, /export function preloadMonsterWorld/);
  assert.match(
    component,
    /loadWorldAssets\(\)\.then\([\s\S]*?setAssetError\([\s\S]*?World assets could not be loaded[\s\S]*?if \(assetError\) throw assetError/,
  );
  assert.match(component, /onPointerDown=\{handleCanvasPointerDown\}/);
  assert.match(component, /Every entry marked <b>nanocodex<\/b> came from a live resident tool loop/);
  assert.match(application, /surface === "world"[\s\S]*?target === document\.activeElement[\s\S]*?target\?\.matches\("\.monster-world-stage canvas"\)/);
  assert.match(worldCss, /prefers-reduced-motion: reduce/);
  assert.match(worldCss, /monster-world-population input\[type="range"\]/);
  assert.doesNotMatch(worldCss, /grayscale\(/);
  assert.doesNotMatch(component, /spinner|skeleton|Suspense|dangerouslySetInnerHTML/i);
});

test("the World canvas renders only for active animation or explicit invalidation", () => {
  assert.match(component, /const WORLD_RENDER_INTERVAL_MS = 50/);
  assert.match(
    component,
    /let dirty = true;[\s\S]*?const requestRender = \(\) => \{\s*dirty = true;\s*scheduleFrame\(\);\s*\}/,
  );
  assert.match(
    component,
    /updateWorld\(activeWorld, delta\);[\s\S]*?if \(now >= nextCanvasDraw\) dirty = true;[\s\S]*?if \(activeWorld && dirty\) \{[\s\S]*?drawMonsterWorld\([\s\S]*?dirty = false;\s*nextCanvasDraw = now \+ WORLD_RENDER_INTERVAL_MS/,
  );
  assert.match(component, /if \(activeWorld && !paused\) scheduleFrame\(\);/);
  assert.match(
    component,
    /document\.visibilityState !== "visible"[\s\S]*?cancelFrame\(\);[\s\S]*?requestRender\(\)/,
  );
  assert.match(component, /const resizeObserver = new ResizeObserver\(requestRender\)/);
  assert.match(component, /assetsRef\.current = assets;\s*requestWorldRender\(\)/);
  assert.match(
    component,
    /heldDirections\.current\.add\(direction\);\s*requestWorldRender\(\)/,
  );
  assert.match(component, /const nudgePlayer[\s\S]*?movePlayer[\s\S]*?requestWorldRender\(\)/);
  assert.match(
    component,
    /disposed = true;[\s\S]*?resizeObserver\.disconnect\(\);\s*cancelFrame\(\)/,
  );
  assert.doesNotMatch(
    component,
    /frame = requestAnimationFrame\(render\);\s*\};\s*frame = requestAnimationFrame\(render\)/,
  );
});

test("worker messages reject malformed cross-isolate payloads", () => {
  assert.equal(isWorldAgentMessage({ protocol: WORLD_PROTOCOL, type: "status", status: "ready" }), true);
  assert.equal(isWorldAgentMessage({ protocol: WORLD_PROTOCOL, type: "status", status: "unknown" }), false);
  assert.equal(isWorldAgentMessage({
    protocol: WORLD_PROTOCOL,
    type: "settled",
    requestId: "turn",
    agentId: "cinder",
    outcome: "completed",
  }), true);
  assert.equal(isWorldAgentMessage({
    protocol: WORLD_PROTOCOL,
    type: "plan",
    plan: {},
    usage: {},
  }), false);
});

test("the imported art carries source, license, and modification attribution", () => {
  assert.match(attribution, /MyPixelWorld Special Packs #01/);
  assert.match(attribution, /scarloxy\.itch\.io\/mpwsp01/);
  assert.match(attribution, /Creative Commons Attribution 4\.0/);
  assert.match(attribution, /displayed in their original palette/);
  for (const path of [
    "../public/world/my-pixel-world/tileset/tileset.png",
    "../public/world/my-pixel-world/sprites/sprite7_idle.png",
    "../public/world/my-pixel-world/sprites/sprite14_idle.png",
    "../public/world/my-pixel-world/character-overworld/ow1.png",
    "../public/world/my-pixel-world/menu-sprites/menusprite16.png",
  ]) {
    assert.ok(readFileSync(new URL(path, import.meta.url)).byteLength > 100, path);
  }
});

function orderById(state: ReturnType<typeof createWorldState>, orderId: number | undefined) {
  if (orderId === undefined) throw new Error("expected an executable World order");
  const order = state.orders.find(({ id }) => id === orderId);
  if (!order) throw new Error(`missing World order ${orderId}`);
  return order;
}

function advanceOrderToTerminal(
  state: ReturnType<typeof createWorldState>,
  orderId: number | undefined,
): void {
  for (let index = 0; index < 2_000; index += 1) {
    if (orderById(state, orderId).completionEmitted) return;
    updateWorld(state, 100);
  }
  throw new Error(`World order ${String(orderId)} did not reach a terminal state`);
}

function assertOrderCompletedAtGoals(
  state: ReturnType<typeof createWorldState>,
  orderId: number,
): void {
  const order = orderById(state, orderId);
  assert.equal(order.completionEmitted, true);
  assert.ok(order.assignments.length > 0);
  for (const assignment of order.assignments) {
    assert.equal(assignment.status, "completed", assignment.actorId);
    assert.ok(assignment.goal, assignment.actorId);
    assert.deepEqual(
      actorWorldPosition(state.actors[assignment.actorId]),
      assignment.goal,
      assignment.actorId,
    );
  }
  assert.equal(
    state.activities.filter(({ text }) =>
      text === `Scout's order ${orderId} complete: ${order.assignments.length}/${order.assignments.length} residents arrived.`
    ).length,
    1,
  );
}

function source(path: string): string {
  return readFileSync(new URL(path, import.meta.url), "utf8");
}
