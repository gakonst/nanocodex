import assert from "node:assert/strict";
import test from "node:test";

import {
  RESIDENT_IDS,
  type WorldResidentMemory,
} from "../src/monsterWorldProtocol.ts";
import {
  BASE_RESIDENT_COUNT,
  WORLD_ROOM_RETENTION,
  applyResidentMemory,
  applyWorldRoomSend,
  applyWorldToolAction,
  completeResidentInstruction,
  createWorldState,
  hasUnansweredGuildCall,
  hasUnansweredPlayerOrder,
  liveAgentIdsInWorld,
  observationFor,
  playerSpeak,
  requestResidentExit,
  residentMemoryFor,
  serializeWorldState,
  setPopulationTarget,
  setWorldAgentsOnline,
  updateWorld,
  worldToolResultAtDecisionBoundary,
} from "../src/monsterWorldSimulation.ts";

test("every on-map resident has autonomous state and bounded retained memory", () => {
  const state = createWorldState();
  const activeIds = liveAgentIdsInWorld(state);
  assert.equal(activeIds.length, BASE_RESIDENT_COUNT);
  assert.deepEqual(activeIds, RESIDENT_IDS.slice(0, BASE_RESIDENT_COUNT));
  assert.deepEqual(Object.keys(state.decisionVersions), RESIDENT_IDS);
  assert.deepEqual(Object.keys(state.residentMemories), RESIDENT_IDS);
  assert.notStrictEqual(residentMemoryFor(state, "cinder"), residentMemoryFor(state, "june"));
  assert.notStrictEqual(
    residentMemoryFor(state, "cinder").goals,
    residentMemoryFor(state, "june").goals,
  );

  const goals = ["Inspect Bell Bridge"];
  const memory: WorldResidentMemory = {
    summary: "June is carrying guild dispatches.",
    goals,
    relationships: ["Scout gives physical orders."],
    recentDecisions: ["Read the newest public board post."],
    lastBoardMessageId: 7,
  };
  applyResidentMemory(state, "june", memory);
  goals.push("Mutate the caller-owned array");
  assert.deepEqual(residentMemoryFor(state, "june").goals, ["Inspect Bell Bridge"]);
  assert.equal(Object.isFrozen(residentMemoryFor(state, "june")), true);
  assert.equal(Object.isFrozen(residentMemoryFor(state, "june").goals), true);

  const restored = createWorldState(serializeWorldState(state));
  assert.deepEqual(residentMemoryFor(restored, "june"), residentMemoryFor(state, "june"));

  const malformed = JSON.parse(serializeWorldState(state)) as Record<string, unknown>;
  (malformed.residentMemories as Record<string, unknown>).june = {
    summary: ["not text"],
    goals: "not a list",
    lastBoardMessageId: -1,
  };
  assert.deepEqual(residentMemoryFor(createWorldState(JSON.stringify(malformed)), "june"), {
    summary: "",
    goals: [],
    relationships: [],
    recentDecisions: [],
    lastBoardMessageId: 0,
  });
});

test("speech, observations, calls, and plans include non-legacy residents", () => {
  const state = createWorldState();
  const activeIds = liveAgentIdsInWorld(state);
  const versions = { ...state.decisionVersions };
  const speech = playerSpeak(state, "Everyone go to the bridge.", "whisper", "reducer");
  assert.ok(speech?.order);
  assert.deepEqual(speech.liveHeardBy, activeIds);
  assert.deepEqual(speech.liveAddressed, activeIds);
  for (const id of activeIds) {
    assert.equal(state.decisionVersions[id], versions[id] + 1);
    assert.equal(hasUnansweredGuildCall(state, id), true);
    assert.equal(observationFor(state, id).guildCall?.requestedTarget, "bridge");
  }

  const planState = createWorldState();
  const application = applyWorldToolAction(planState, {
    actionId: "june-action-1",
    requestId: "june-autonomous-1",
    agentId: "june",
    action: { kind: "move", target: "mission_board" },
  });
  assert.equal(application.accepted, true);
});

test("resident lifecycle and global weather changes fence every affected decision", () => {
  const lifecycle = createWorldState();
  const nextResident = RESIDENT_IDS[BASE_RESIDENT_COUNT];
  assert.ok(nextResident);
  const guestVersion = lifecycle.decisionVersions[nextResident];
  assert.deepEqual(setPopulationTarget(lifecycle, BASE_RESIDENT_COUNT + 1).entering, [nextResident]);
  assert.equal(lifecycle.decisionVersions[nextResident], guestVersion + 1);
  for (let index = 0; index < 20; index += 1) updateWorld(lifecycle, 100);
  const activeVersion = observationFor(lifecycle, nextResident).stateVersion;
  assert.equal(requestResidentExit(lifecycle, nextResident), true);
  assert.equal(lifecycle.decisionVersions[nextResident], activeVersion + 1);

  const weather = createWorldState();
  const versions = { ...weather.decisionVersions };
  weather.weatherDueMs = 0;
  updateWorld(weather, 100);
  for (const id of RESIDENT_IDS) {
    assert.equal(weather.decisionVersions[id], versions[id] + 1, id);
  }
});

test("online Luna control suppresses only new idle fallback routines", () => {
  const transitioning = createWorldState();
  for (let index = 0; index < 15; index += 1) updateWorld(transitioning, 100);
  assert.ok(transitioning.actors.cinder.movement);
  assert.ok(transitioning.actors.cinder.tasks.some(({ origin }) => origin === "routine"));
  setWorldAgentsOnline(transitioning, true);
  assert.ok(transitioning.actors.cinder.movement);
  assert.equal(transitioning.actors.cinder.tasks.length, 0);
  for (let index = 0; index < 100; index += 1) updateWorld(transitioning, 100);
  assert.equal(transitioning.actors.cinder.movement, undefined);
  assert.equal(transitioning.actors.cinder.tasks.length, 0);

  const state = createWorldState();
  const activeIds = liveAgentIdsInWorld(state);
  const routineIndexes = Object.fromEntries(
    activeIds.map((id) => [id, state.actors[id].routineIndex]),
  );
  setWorldAgentsOnline(state, true);
  for (let index = 0; index < 200; index += 1) updateWorld(state, 100);
  for (const id of activeIds) {
    assert.equal(state.actors[id].routineIndex, routineIndexes[id]);
    assert.equal(state.actors[id].tasks.length, 0);
  }

  const order = playerSpeak(state, "Everyone go to the pond.", "call", "reducer")?.order;
  assert.ok(order);
  updateWorld(state, 100);
  assert.ok(activeIds.every((id) => state.actors[id].tasks[0]?.orderId === order.id));
  assert.ok(activeIds.some((id) => state.actors[id].movement !== undefined));

  const fallback = createWorldState();
  setWorldAgentsOnline(fallback, true);
  for (let index = 0; index < 200; index += 1) updateWorld(fallback, 100);
  const fallbackIds = liveAgentIdsInWorld(fallback);
  const fallbackIndexes = Object.fromEntries(
    fallbackIds.map((id) => [id, fallback.actors[id].routineIndex]),
  );
  setWorldAgentsOnline(fallback, false);
  for (let index = 0; index < 150; index += 1) updateWorld(fallback, 100);
  assert.ok(fallbackIds.every((id) => fallback.actors[id].routineIndex !== fallbackIndexes[id]));
});

test("independent resident tools mutate through one reducer and return fresh observations", () => {
  const state = createWorldState();
  setWorldAgentsOnline(state, true);
  const cinder = applyWorldToolAction(state, {
    actionId: "cinder-say-1",
    requestId: "cinder-turn-1",
    agentId: "cinder",
    action: { kind: "say", text: "Taking my place." },
  });
  const june = applyWorldToolAction(state, {
    actionId: "june-emote-1",
    requestId: "june-turn-1",
    agentId: "june",
    action: { kind: "emote", icon: "?" },
  });
  assert.equal(cinder.accepted, true);
  assert.equal(june.accepted, true);
  if (!cinder.accepted || !june.accepted) return;
  assert.equal(state.actors.cinder.tasks[0]?.requestId, "cinder-say-1");
  assert.equal(state.actors.june.tasks[0]?.requestId, "june-emote-1");
  updateWorld(state, 100);
  const cinderResult = worldToolResultAtDecisionBoundary(state, cinder.pending);
  assert.equal(cinderResult?.outcome.status, "completed");
  assert.equal(cinderResult?.self.id, "cinder");
  assert.equal(cinderResult?.worldRevision, observationFor(state, "cinder").stateVersion);
  assert.equal(cinderResult?.roster.some(({ id }) => id === "june"), true);
  const juneResult = worldToolResultAtDecisionBoundary(state, june.pending);
  assert.equal(juneResult?.outcome.status, "completed");
  assert.equal(juneResult?.self.id, "june");
});

test("room posts are ordered reducer writes that do not interrupt embodied movement", () => {
  const state = createWorldState();
  setWorldAgentsOnline(state, true);
  const movement = applyWorldToolAction(state, {
    actionId: "cinder-move-before-chat",
    requestId: "cinder-turn-before-chat",
    agentId: "cinder",
    action: { kind: "move", target: "bridge" },
  });
  assert.equal(movement.accepted, true);
  const movementTask = state.actors.cinder.tasks[0];
  assert.equal(movementTask?.requestId, "cinder-move-before-chat");

  const decisionVersion = state.decisionVersions.cinder;
  const roomPost = applyWorldRoomSend(state, {
    sendId: "cinder-room-1",
    requestId: "cinder-turn-before-chat",
    agentId: "cinder",
    text: "Moss, I will check Bell Bridge.",
  });
  assert.equal(roomPost.accepted, true);
  assert.strictEqual(state.actors.cinder.tasks[0], movementTask);
  assert.equal(state.decisionVersions.cinder, decisionVersion);
  assert.equal(state.guildMessages[0]?.fromId, "cinder");
  assert.equal(state.guildMessages[0]?.text, "Moss, I will check Bell Bridge.");
  if (!roomPost.accepted) return;
  assert.equal(roomPost.message.text, "Moss, I will check Bell Bridge.");
  assert.equal(observationFor(state, "moss").guildBoard[0]?.fromName, "Cinder");

  assert.deepEqual(applyWorldRoomSend(state, {
    sendId: "cinder-room-1",
    requestId: "cinder-turn-before-chat",
    agentId: "cinder",
    text: "This duplicate must not append.",
  }), { accepted: false, reason: "duplicate" });

  const roomOrder = playerSpeak(state, "Post your status in room chat.", "call");
  const callId = observationFor(state, "cinder").playerOrder?.id;
  assert.ok(roomOrder && callId !== undefined);
  assert.equal(hasUnansweredPlayerOrder(state, "cinder"), true);
  assert.equal(applyWorldRoomSend(state, {
    sendId: "cinder-room-2",
    requestId: "cinder-room-order-turn",
    agentId: "cinder",
    heardCallId: callId,
    text: "Cinder ready.",
  }).accepted, true);
  assert.equal(hasUnansweredPlayerOrder(state, "cinder"), true);
  assert.equal(hasUnansweredGuildCall(state, "cinder"), true);
  assert.equal(completeResidentInstruction(state, "cinder", callId), true);
  assert.equal(hasUnansweredPlayerOrder(state, "cinder"), false);
  assert.equal(hasUnansweredGuildCall(state, "cinder"), false);
});

test("the shared room retains a full 48-resident coordination wave", () => {
  const state = createWorldState();
  setWorldAgentsOnline(state, true);
  const firstMessageText = "CALL-77 CONTRACT origin=Scout groups=coListener-index-div-8";

  for (let index = 0; index < 64; index += 1) {
    const posted = applyWorldRoomSend(state, {
      sendId: `coordination-wave-${index}`,
      requestId: `coordination-turn-${index}`,
      agentId: "cinder",
      text: index === 0 ? firstMessageText : `CALL-77 resident update ${index}`,
    });
    assert.equal(posted.accepted, true);
  }

  assert.equal(WORLD_ROOM_RETENTION >= 64, true);
  assert.equal(state.guildMessages.some(({ text }) => text === firstMessageText), true);
  assert.equal(
    observationFor(state, "june").guildBoard.some(({ text }) => text === firstMessageText),
    true,
  );
  assert.equal(
    createWorldState(serializeWorldState(state)).guildMessages.some(
      ({ text }) => text === firstMessageText,
    ),
    true,
  );
});

test("World actions and room posts keep an instruction active until its resident turn completes", () => {
  const state = createWorldState();
  setWorldAgentsOnline(state, true);
  playerSpeak(state, "Form six groups of eight, each group making a square.", "call");
  const callId = observationFor(state, "cinder").playerOrder?.id;
  assert.ok(callId !== undefined);

  const movement = applyWorldToolAction(state, {
    actionId: "cinder-provisional-position",
    requestId: "cinder-formation-turn",
    agentId: "cinder",
    heardCallId: callId,
    action: { kind: "move_relative", anchor: "player", dx_pixels: 24, dy_pixels: 0 },
  });
  assert.equal(movement.accepted, true);
  assert.equal(hasUnansweredPlayerOrder(state, "cinder"), true);
  assert.equal(hasUnansweredGuildCall(state, "cinder"), true);

  assert.equal(applyWorldRoomSend(state, {
    sendId: "cinder-provisional-board-post",
    requestId: "cinder-formation-turn",
    agentId: "cinder",
    heardCallId: callId,
    text: "I am taking a provisional northwest corner and will adjust.",
  }).accepted, true);
  assert.equal(hasUnansweredPlayerOrder(state, "cinder"), true);
  assert.equal(hasUnansweredGuildCall(state, "cinder"), true);

  playerSpeak(state, "Now make one wide ring.", "call");
  assert.equal(completeResidentInstruction(state, "cinder", callId), false);
  assert.equal(hasUnansweredPlayerOrder(state, "cinder"), true);
  const newerCallId = observationFor(state, "cinder").playerOrder?.id;
  assert.ok(newerCallId !== undefined && newerCallId !== callId);
  assert.equal(completeResidentInstruction(state, "cinder", newerCallId), true);
  assert.equal(hasUnansweredPlayerOrder(state, "cinder"), false);
});

test("a newer player instruction supersedes stale in-turn World control", () => {
  const state = createWorldState();
  setWorldAgentsOnline(state, true);
  const firstSpeech = playerSpeak(state, "Everyone form a circle around me.", "call");
  assert.ok(firstSpeech);
  const heardCallId = observationFor(state, "cinder").playerOrder?.id;
  assert.ok(heardCallId !== undefined);
  const action = applyWorldToolAction(state, {
    actionId: "cinder-circle-1",
    requestId: "cinder-turn-circle",
    agentId: "cinder",
    heardCallId,
    action: { kind: "move_relative", anchor: "player", dx_pixels: 64, dy_pixels: 0 },
  });
  assert.equal(action.accepted, true);
  if (!action.accepted) return;
  playerSpeak(state, "Everyone form a star around me.", "call");
  const result = worldToolResultAtDecisionBoundary(state, action.pending);
  assert.equal(result?.outcome.status, "superseded");
  assert.match(result?.outcome.detail ?? "", /newer instruction/i);
});

test("an agent-authored relative constraint keeps correcting below the Luna turn boundary", () => {
  const state = createWorldState();
  setWorldAgentsOnline(state, true);
  for (const id of RESIDENT_IDS) {
    if (id !== "cinder") state.actors[id].presence = "absent";
  }
  const player = state.actors.player;
  const cinder = state.actors.cinder;
  cinder.presence = "active";
  cinder.scene = player.scene;
  cinder.x = player.x - 8;
  cinder.y = player.y;
  cinder.tasks = [];
  cinder.movement = undefined;

  const application = applyWorldToolAction(state, {
    actionId: "cinder-maintain-player-left",
    requestId: "cinder-maintain-turn",
    agentId: "cinder",
    action: {
      kind: "maintain_relative",
      anchor: "player",
      dx_pixels: -24,
      dy_pixels: 0,
      tolerance_pixels: 8,
    },
  });
  assert.equal(application.accepted, true);
  if (!application.accepted) return;
  for (let index = 0; index < 300 && (cinder.tasks.length > 0 || cinder.movement); index += 1) {
    updateWorld(state, 100);
  }
  assert.ok(cinder.relativeConstraint);
  assert.ok(Math.abs(cinder.x - (player.x - 3)) <= 1);
  assert.equal(
    worldToolResultAtDecisionBoundary(state, application.pending)?.outcome.status,
    "in_progress",
  );

  const settledX = cinder.x;
  player.x += 2;
  for (let index = 0; index < 300; index += 1) updateWorld(state, 100);
  assert.ok(cinder.x > settledX, "the cheap controller should follow a moving anchor without Luna");
  assert.ok(Math.abs(cinder.x - (player.x - 3)) <= 1);

  playerSpeak(state, "Now gather beside the bridge instead.", "call");
  assert.equal(cinder.relativeConstraint, undefined, "a newer raw order replaces the old relation");

  assert.deepEqual(applyWorldToolAction(state, {
    actionId: "cinder-invalid-self-anchor",
    requestId: "cinder-invalid-self-turn",
    agentId: "cinder",
    heardCallId: observationFor(state, "cinder").playerOrder?.id,
    action: {
      kind: "maintain_relative",
      anchor: "cinder",
      dx_pixels: 24,
      dy_pixels: 0,
      tolerance_pixels: 8,
    },
  }), { accepted: false, reason: "invalid" });
});

test("maintained relative constraints reject follower cycles", () => {
  const state = createWorldState();
  setWorldAgentsOnline(state, true);
  const cinder = applyWorldToolAction(state, {
    actionId: "cinder-follows-june",
    requestId: "cinder-chain-turn",
    agentId: "cinder",
    action: {
      kind: "maintain_relative",
      anchor: "june",
      dx_pixels: -24,
      dy_pixels: 0,
      tolerance_pixels: 8,
    },
  });
  assert.equal(cinder.accepted, true);

  assert.deepEqual(applyWorldToolAction(state, {
    actionId: "june-follows-cinder",
    requestId: "june-cycle-turn",
    agentId: "june",
    action: {
      kind: "maintain_relative",
      anchor: "cinder",
      dx_pixels: 24,
      dy_pixels: 0,
      tolerance_pixels: 8,
    },
  }), { accepted: false, reason: "invalid" });
});
