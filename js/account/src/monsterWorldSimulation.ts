import {
  ACTOR_IDS,
  GUEST_AGENT_IDS,
  RESIDENT_IDS,
  VOICE_RADIUS,
  WORLD_ITEM_KINDS,
  WORLD_SCENE_IDS,
  WORLD_TARGETS,
  decodeWorldResidentMemory,
  isResidentId,
  isWorldPlan,
  sanitizeDialogue,
  type ActorId,
  type Direction,
  type GuestAgentId,
  type HeardGuildCall,
  type PlanOrigin,
  type ResidentId,
  type VoiceLevel,
  type WorldAction,
  type WorldBoardMessage,
  type WorldInteraction,
  type WorldItemKind,
  type WorldObservation,
  type WorldPlan,
  type WorldPlayerOrder,
  type WorldPosition,
  type WorldPrimitiveAction,
  type WorldResidentMemory,
  type WorldSceneId,
  type WorldSupplyState,
  type WorldTarget,
  type WorldToolResult,
} from "./monsterWorldProtocol.ts";
import {
  WORLD_ENTRY_PORTALS,
  WORLD_PIXEL_HEIGHT as MAP_PIXEL_HEIGHT,
  WORLD_PIXEL_WIDTH as MAP_PIXEL_WIDTH,
  WORLD_POIS as SCENE_WORLD_POIS,
  WORLD_SCENES,
  WORLD_TILE_SIZE,
  cameraForPosition,
  findWorldRoute,
  isBlocked as isMapBlocked,
  isWorldPositionBlocked,
  portalDestinationAt,
  sceneLabel,
  type WorldCamera,
  type WorldEntryPortal,
} from "./monsterWorldMap.ts";

export const WORLD_COLUMNS = WORLD_SCENES.town.columns;
export const WORLD_ROWS = WORLD_SCENES.town.rows;
export const WORLD_PIXEL_WIDTH = MAP_PIXEL_WIDTH;
export const WORLD_PIXEL_HEIGHT = MAP_PIXEL_HEIGHT;
export const WORLD_SAVE_KEY = "nanocodex-monster-world-v3";
export const WORLD_POIS = SCENE_WORLD_POIS;
export const GUILD_RELAY_RADIUS = 3.25;
export const BASE_RESIDENT_COUNT = 36;
export const MAX_RESIDENT_COUNT = RESIDENT_IDS.length;
export const WORLD_ROOM_RETENTION = 128;
const RETAINED_TERMINAL_ORDERS = 8;
const OUTDOOR_TILE_ENERGY_COST = 1;
const GUILD_REST_ENERGY = 32;
const TRAINING_ENERGY_COST = 12;
const ORCHARD_CAPACITY = 8;
const ORCHARD_RESTOCK_INTERVAL_MS = 30_000;

export type ActivityOrigin = PlanOrigin | "player" | "system";

type WorldTaskOrigin = PlanOrigin | "player";

export type WorldActivity = Readonly<{
  id: number;
  actorId?: ActorId;
  minuteOfDay: number;
  origin: ActivityOrigin;
  text: string;
  audience?: readonly ResidentId[];
}>;

export type GuildMessage = Readonly<{
  id: number;
  fromId: ActorId;
  toId?: ActorId;
  minuteOfDay: number;
  origin: PlanOrigin | "player";
  text: string;
  scope: "public" | "spatial";
  audience?: readonly ResidentId[];
}>;

export type Point = Readonly<{ x: number; y: number }>;

type WorldMovement = {
  from: WorldPosition;
  to: WorldPosition;
  progress: number;
  durationMs: number;
};

type WorldDeparture = {
  inside: WorldPosition;
  outside: WorldPosition;
  phase: "to-edge" | "crossing";
  path?: WorldPosition[];
};

type WorldTask = {
  action: WorldAction;
  origin: WorldTaskOrigin;
  requestId: string;
  orderId?: number;
  goal?: WorldPosition;
  path?: WorldPosition[];
  rerouteAtMs?: number;
  blockedSinceMs?: number;
};

type WorldRelativeConstraint = Readonly<{
  action: Extract<WorldPrimitiveAction, { kind: "maintain_relative" }>;
  requestId: string;
}>;

export type WorldOrderStatus = "assigned" | "moving" | "completed" | "preempted" | "rejected";

export type WorldOrderAssignment = {
  actorId: ResidentId;
  target: WorldTarget;
  interaction?: WorldInteraction;
  goal?: WorldPosition;
  status: WorldOrderStatus;
  startedAtMs?: number;
  completedAtMs?: number;
  reason?: "not-active" | "unreachable" | "interaction-failed" | "superseded" | "left-world";
};

export type WorldOrder = {
  id: number;
  text: string;
  issuedAtMs: number;
  assignments: WorldOrderAssignment[];
  completionEmitted: boolean;
};

export type WorldOrderReceipt = Readonly<{
  id: number;
  assigned: readonly Readonly<{
    actorId: ResidentId;
    target: WorldTarget;
    interaction?: WorldInteraction;
    goal: WorldPosition;
  }>[];
  rejected: readonly Readonly<{
    actorId: ResidentId;
    target: WorldTarget;
    reason: "not-active" | "unreachable";
  }>[];
}>;

export type WorldActor = {
  id: ActorId;
  name: string;
  role: string;
  kind: "player" | "monster" | "human";
  presence: "absent" | "entering" | "active" | "exiting";
  sprite: number;
  scene: WorldSceneId;
  x: number;
  y: number;
  carrying?: WorldItemKind;
  direction: Direction;
  movement?: WorldMovement;
  departure?: WorldDeparture;
  tasks: WorldTask[];
  relativeConstraint?: WorldRelativeConstraint;
  activity: string;
  intent?: string;
  lastOrigin: ActivityOrigin;
  bubble?: Readonly<{ text: string; untilMs: number }>;
  emote?: Readonly<{ icon: string; untilMs: number }>;
  effect?: Readonly<{ kind: WorldInteraction; untilMs: number }>;
  listenerPulse?: Readonly<{ callId: number; untilMs: number }>;
  energy: number;
  curiosity: number;
  social: number;
  routineIndex: number;
  routineDueMs: number;
  activeOrderId?: number;
};

export type WorldMission = {
  stage: number;
  title: string;
  detail: string;
};

export type WorldState = {
  elapsedMs: number;
  minuteOfDay: number;
  weather: "clear" | "drizzle";
  weatherDueMs: number;
  actors: Record<ActorId, WorldActor>;
  decisionVersions: Record<ResidentId, number>;
  heardCalls: Partial<Record<ResidentId, HeardGuildCall>>;
  acknowledgedCallIds: Partial<Record<ResidentId, number>>;
  playerOrders: Partial<Record<ResidentId, WorldPlayerOrder>>;
  acknowledgedPlayerOrderIds: Partial<Record<ResidentId, number>>;
  residentMemories: Record<ResidentId, WorldResidentMemory>;
  speechWave?: Readonly<{
    issuedAtMs: number;
    voice: VoiceLevel;
    radius: number;
    guildWide: boolean;
  }>;
  activities: WorldActivity[];
  guildMessages: GuildMessage[];
  mission: WorldMission;
  supplies: MutableWorldSupplyState;
  orchardRestockDueMs: number;
  nextActivityId: number;
  nextGuildMessageId: number;
  rng: number;
  seenRequestIds: string[];
  orders: WorldOrder[];
  agentsOnline: boolean;
  agentDecisions: number;
  populationTarget: number;
};

type MutableWorldSupplyState = {
  -readonly [Key in keyof WorldSupplyState]: WorldSupplyState[Key];
};

export type PlayerSpeech = Readonly<{
  callId: number;
  text: string;
  voice: VoiceLevel;
  radius: number;
  guildWide: boolean;
  heardBy: readonly ResidentId[];
  liveHeardBy: readonly ResidentId[];
  liveAddressed: readonly ResidentId[];
  order?: WorldOrderReceipt;
}>;

export type PlayerSpeechExecution = "resident_minds" | "reducer";

export type PopulationChange = Readonly<{
  target: number;
  entering: readonly ResidentId[];
  exiting: readonly ResidentId[];
}>;

export type PlanApplication =
  | Readonly<{ accepted: true }>
  | Readonly<{ accepted: false; reason: "duplicate" | "invalid" | "stale" }>;

export type WorldToolAction = Readonly<{
  actionId: string;
  requestId: string;
  agentId: ResidentId;
  heardCallId?: number;
  action: WorldPrimitiveAction;
  startedAtMs: number;
  activityCursor: number;
}>;

export type WorldToolActionApplication =
  | Readonly<{ accepted: true; pending: WorldToolAction }>
  | Readonly<{ accepted: false; reason: "duplicate" | "invalid" | "superseded" }>;

export type WorldRoomSendApplication =
  | Readonly<{ accepted: true; message: WorldBoardMessage }>
  | Readonly<{ accepted: false; reason: "duplicate" | "invalid" }>;

type ActorDefinition = Readonly<{
  name: string;
  role: string;
  kind: "player" | "monster" | "human";
  sprite: number;
  x: number;
  y: number;
}>;

const namedActorDefinitions = {
  player: { name: "Scout", role: "you", kind: "player", sprite: 1, x: 16, y: 13 },
  cinder: { name: "Cinder", role: "monster captain", kind: "monster", sprite: 7, x: 8, y: 10 },
  moss: { name: "Moss", role: "monster forager", kind: "monster", sprite: 14, x: 18, y: 11 },
  rill: { name: "Rill", role: "monster pathfinder", kind: "monster", sprite: 11, x: 22, y: 18 },
  luma: { name: "Luma", role: "monster guild keeper", kind: "monster", sprite: 16, x: 13, y: 8 },
  iris: { name: "Iris", role: "human ranger", kind: "human", sprite: 2, x: 11, y: 11 },
  rook: { name: "Rook", role: "human cartographer", kind: "human", sprite: 3, x: 19, y: 14 },
  june: { name: "June", role: "human courier", kind: "human", sprite: 4, x: 15, y: 17 },
  pax: { name: "Pax", role: "human tinkerer", kind: "human", sprite: 5, x: 21, y: 10 },
  ember: { name: "Ember", role: "monster cook", kind: "monster", sprite: 1, x: 4, y: 10 },
  fern: { name: "Fern", role: "monster herbalist", kind: "monster", sprite: 2, x: 7, y: 12 },
  brook: { name: "Brook", role: "monster swimmer", kind: "monster", sprite: 3, x: 20, y: 20 },
  twig: { name: "Twig", role: "monster apprentice", kind: "monster", sprite: 4, x: 10, y: 16 },
  pebble: { name: "Pebble", role: "monster porter", kind: "monster", sprite: 5, x: 18, y: 8 },
  nova: { name: "Nova", role: "monster merchant", kind: "monster", sprite: 6, x: 27, y: 10 },
  pip: { name: "Pip", role: "monster trainee", kind: "monster", sprite: 8, x: 12, y: 20 },
  dune: { name: "Dune", role: "monster lookout", kind: "monster", sprite: 9, x: 21, y: 13 },
  aria: { name: "Aria", role: "human bard", kind: "human", sprite: 6, x: 10, y: 8 },
  beck: { name: "Beck", role: "human rescuer", kind: "human", sprite: 7, x: 14, y: 10 },
  cyra: { name: "Cyra", role: "human alchemist", kind: "human", sprite: 8, x: 17, y: 7 },
  dev: { name: "Dev", role: "human trader", kind: "human", sprite: 9, x: 25, y: 12 },
  esme: { name: "Esme", role: "human gardener", kind: "human", sprite: 10, x: 5, y: 19 },
  finn: { name: "Finn", role: "human explorer", kind: "human", sprite: 2, x: 11, y: 18 },
  grey: { name: "Grey", role: "human delver", kind: "human", sprite: 3, x: 19, y: 19 },
  hope: { name: "Hope", role: "human healer", kind: "human", sprite: 4, x: 28, y: 14 },
} as const satisfies Record<Exclude<ActorId, GuestAgentId>, ActorDefinition>;

const guestNames = [
  "Ash", "Briar", "Cove", "Dot", "Echo", "Fable", "Gale", "Haru",
  "Indie", "Jasper", "Kite", "Lux", "Mika", "Nori", "Ollie", "Quill",
  "Rune", "Sage", "Tala", "Uma", "Vale", "Wren", "Xeno", "Yara",
] as const;

const guestRoles = [
  "visiting delver",
  "wandering rescuer",
  "rookie explorer",
  "traveling forager",
] as const;

const guestActorDefinitions = Object.fromEntries(GUEST_AGENT_IDS.map((id, index) => {
  const kind = index % 2 === 0 ? "monster" : "human";
  return [id, Object.freeze({
    name: guestNames[index] ?? `Guest ${index + 1}`,
    role: guestRoles[index % guestRoles.length] ?? "visiting explorer",
    kind,
    sprite: kind === "monster" ? index % 16 + 1 : index % 10 + 1,
    x: 16,
    y: 13,
  })];
})) as Record<GuestAgentId, ActorDefinition>;

const actorDefinitions: Record<ActorId, ActorDefinition> = {
  ...namedActorDefinitions,
  ...guestActorDefinitions,
};

type RoutineScript = Readonly<{
  summary: string;
  steps: readonly WorldAction[];
}>;

const specialRoutineScripts: Partial<Record<ResidentId, readonly RoutineScript[]>> = {
  cinder: [
    {
      summary: "checks the new rescue notice",
      steps: [
        { kind: "move", target: "mission_board" },
        { kind: "interact", target: "mission_board", action: "inspect" },
        { kind: "emote", icon: "!" },
        { kind: "say", text: "Fresh tracks. Rescue Team, form up!" },
      ],
    },
    {
      summary: "scouts the Mystery Gate",
      steps: [
        { kind: "move", target: "dungeon_gate" },
        { kind: "interact", target: "dungeon_gate", action: "train" },
        { kind: "say", text: "The wind beyond the gate smells like rain." },
      ],
    },
    {
      summary: "returns to the guild plaza",
      steps: [
        { kind: "move", target: "plaza" },
        { kind: "emote", icon: "music" },
      ],
    },
  ],
  moss: [
    {
      summary: "forages for expedition supplies",
      steps: [
        { kind: "move", target: "orchard" },
        { kind: "interact", target: "orchard", action: "gather" },
        { kind: "say", text: "A silver sunberry! This was not here yesterday." },
      ],
    },
    {
      summary: "brings a clue to Luma",
      steps: [
        { kind: "move", target: "luma" },
        { kind: "interact", target: "luma", action: "offer" },
        { kind: "emote", icon: "spark" },
      ],
    },
    {
      summary: "restocks at the market",
      steps: [
        { kind: "move", target: "shop" },
        { kind: "interact", target: "shop", action: "inspect" },
        { kind: "say", text: "Two apples, one rope, zero cursed boxes." },
      ],
    },
  ],
  rill: [
    {
      summary: "searches beneath Bell Bridge",
      steps: [
        { kind: "move", target: "bridge" },
        { kind: "interact", target: "bridge", action: "inspect" },
        { kind: "say", text: "I heard the missing bell ring under the water." },
      ],
    },
    {
      summary: "tests the pond current",
      steps: [
        { kind: "move", target: "pond" },
        { kind: "interact", target: "pond", action: "splash" },
        { kind: "emote", icon: "?" },
      ],
    },
    {
      summary: "reports at the mission board",
      steps: [
        { kind: "move", target: "mission_board" },
        { kind: "say", text: "Current runs east. Whatever took it went gateward." },
      ],
    },
  ],
  luma: [
    {
      summary: "posts a new guild expedition",
      steps: [
        { kind: "move", target: "mission_board" },
        { kind: "interact", target: "mission_board", action: "post" },
        { kind: "say", text: "Bronze rank or not, every rescue matters." },
      ],
    },
    {
      summary: "rings the plaza assembly chime",
      steps: [
        { kind: "move", target: "plaza" },
        { kind: "emote", icon: "music" },
        { kind: "say", text: "Morning assembly! Check your badges and bags." },
      ],
    },
    {
      summary: "keeps watch at the guild",
      steps: [
        { kind: "move", target: "guild" },
        { kind: "interact", target: "guild", action: "rest" },
      ],
    },
  ],
};

const routineRoutes: Partial<Record<ResidentId, readonly WorldTarget[]>> = {
  cinder: ["mission_board", "dungeon_gate", "plaza"],
  moss: ["orchard", "luma", "shop"],
  rill: ["bridge", "pond", "mission_board"],
  luma: ["mission_board", "plaza", "guild"],
  iris: ["dungeon_gate", "meadow", "mission_board"],
  rook: ["bridge", "plaza", "dungeon_gate"],
  june: ["guild", "shop", "orchard"],
  pax: ["shop", "mission_board", "meadow"],
  ember: ["shop", "guild", "plaza"],
  fern: ["orchard", "guild", "pond"],
  brook: ["pond", "bridge", "plaza"],
  twig: ["orchard", "meadow", "mission_board"],
  pebble: ["guild", "shop", "mission_board"],
  nova: ["shop", "plaza", "guild"],
  pip: ["meadow", "dungeon_gate", "plaza"],
  dune: ["dungeon_gate", "bridge", "guild"],
  aria: ["plaza", "guild", "pond"],
  beck: ["mission_board", "meadow", "dungeon_gate"],
  cyra: ["orchard", "shop", "mission_board"],
  dev: ["shop", "bridge", "plaza"],
  esme: ["orchard", "pond", "guild"],
  finn: ["dungeon_gate", "bridge", "meadow"],
  grey: ["bridge", "dungeon_gate", "shop"],
  hope: ["guild", "pond", "plaza"],
};

const routineLines: Partial<Record<ResidentId, readonly string[]>> = {
  cinder: ["Rescue Team, form up!"],
  moss: ["I found another silver fleck."],
  rill: ["The current is telling us something."],
  luma: ["Badges ready, everyone."],
  iris: ["I'll scout one turn ahead."],
  rook: ["That shortcut is finally on the map."],
  june: ["Guild post! Make a hole!"],
  pax: ["This compass only sparks a little."],
  ember: ["Hot stew after the expedition!"],
  fern: ["These leaves calm dungeon fog."],
  brook: ["Race you across the bridge!"],
  twig: ["I can carry the small bag."],
  pebble: ["Supplies secured."],
  nova: ["Fair trades for brave teams."],
  pip: ["One more training lap!"],
  dune: ["The gate moved again."],
  aria: ["A marching song for the rescue team!"],
  beck: ["Rope, badge, map. Ready."],
  cyra: ["Silver dust reacts to rainwater."],
  dev: ["Fresh apples at the trail shop."],
  esme: ["The orchard is restless today."],
  finn: ["Dungeon weather looks strange."],
  grey: ["I marked a safe tile beyond the gate."],
  hope: ["Come back before your energy hits zero."],
};

export function createWorldState(saved?: string | null): WorldState {
  const state: WorldState = {
    elapsedMs: 0,
    minuteOfDay: 8 * 60 + 12,
    weather: "clear",
    weatherDueMs: 58_000,
    actors: createActors(),
    decisionVersions: createDecisionVersions(),
    heardCalls: {},
    acknowledgedCallIds: {},
    playerOrders: {},
    acknowledgedPlayerOrderIds: {},
    residentMemories: createResidentMemories(),
    activities: [],
    guildMessages: [],
    mission: {
      stage: 0,
      title: "The bell beneath the water",
      detail: "The guild's silver rescue bell vanished before dawn. Search the orchard and Bell Bridge for clues.",
    },
    supplies: createSupplies(),
    orchardRestockDueMs: ORCHARD_RESTOCK_INTERVAL_MS,
    nextActivityId: 1,
    nextGuildMessageId: 1,
    rng: 0x53_50_52_47,
    seenRequestIds: [],
    orders: [],
    agentsOnline: false,
    agentDecisions: 0,
    populationTarget: BASE_RESIDENT_COUNT,
  };
  addGuildMessage(
    state,
    "luma",
    "Morning assembly: share clues here, address teammates by name, and keep the rescue moving.",
    "routine",
  );
  addActivity(state, "system", "Morning assembly began. The silver rescue bell is missing.");
  if (saved) restoreSavedState(state, saved);
  return state;
}

export function activeResidentCount(state: WorldState): number {
  return RESIDENT_IDS.filter((id) => state.actors[id].presence !== "absent").length;
}

export function liveAgentIdsInWorld(state: WorldState): readonly ResidentId[] {
  return RESIDENT_IDS.filter((id) => state.actors[id].presence === "active");
}

export function residentMemoryFor(state: WorldState, id: ResidentId): WorldResidentMemory {
  return state.residentMemories[id];
}

export function applyResidentMemory(
  state: WorldState,
  id: ResidentId,
  memory: WorldResidentMemory,
): void {
  state.residentMemories[id] = copyResidentMemory(memory);
}

export function setPopulationTarget(state: WorldState, requested: number): PopulationChange {
  const fallback = Number.isFinite(state.populationTarget)
    ? state.populationTarget
    : activeResidentCount(state);
  const target = Math.max(
    0,
    Math.min(MAX_RESIDENT_COUNT, Math.round(Number.isFinite(requested) ? requested : fallback)),
  );
  state.populationTarget = target;
  const entering: ResidentId[] = [];
  const exiting: ResidentId[] = [];
  let projected = projectedResidentCount(state);

  if (target > projected) {
    for (const id of RESIDENT_IDS) {
      if (projected >= target) break;
      const actor = state.actors[id];
      if (actor.presence !== "exiting") continue;
      const departure = actor.departure;
      if (departure?.phase === "crossing") {
        const point = actorRenderPoint(actor);
        actor.presence = "entering";
        actor.scene = departure.outside.scene;
        actor.x = departure.outside.x;
        actor.y = departure.outside.y;
        actor.movement = {
          from: { scene: actor.scene, ...point },
          to: { ...departure.inside },
          progress: 0,
          durationMs: 620,
        };
        actor.direction = directionBetween(point, departure.inside);
      } else {
        actor.presence = "active";
      }
      actor.departure = undefined;
      actor.tasks = [];
      actor.activity = "decided to stay in town";
      actor.intent = "stay for one more expedition";
      actor.bubble = { text: "Actually, one more expedition.", untilMs: state.elapsedMs + 3_200 };
      actor.routineDueMs = state.elapsedMs + 700;
      state.decisionVersions[id] += 1;
      entering.push(id);
      projected += 1;
    }
    for (const id of RESIDENT_IDS) {
      if (projected >= target) break;
      if (state.actors[id].presence !== "absent") continue;
      spawnResident(state, id);
      entering.push(id);
      projected += 1;
    }
  } else if (target < projected) {
    const departureOrder: readonly ResidentId[] = [...RESIDENT_IDS].reverse();
    for (const id of departureOrder) {
      if (projected <= target) break;
      if (!beginResidentExit(state, id)) continue;
      exiting.push(id);
      projected -= 1;
    }
  }

  return Object.freeze({
    target,
    entering: Object.freeze(entering),
    exiting: Object.freeze(exiting),
  });
}

export function requestResidentExit(state: WorldState, id: ResidentId): boolean {
  if (!beginResidentExit(state, id)) return false;
  state.populationTarget = Math.max(0, projectedResidentCount(state));
  return true;
}

export function residentAtWorldPoint(
  state: WorldState,
  scene: WorldSceneId,
  x: number,
  y: number,
): ResidentId | undefined;
export function residentAtWorldPoint(
  state: WorldState,
  viewportX: number,
  viewportY: number,
): ResidentId | undefined;
export function residentAtWorldPoint(
  state: WorldState,
  sceneOrViewportX: WorldSceneId | number,
  xOrViewportY: number,
  worldY?: number,
): ResidentId | undefined {
  const camera = worldCameraForState(state);
  const scene = typeof sceneOrViewportX === "string" ? sceneOrViewportX : camera.scene;
  const x = typeof sceneOrViewportX === "string" ? xOrViewportY : camera.x + sceneOrViewportX;
  const y = typeof sceneOrViewportX === "string" ? worldY : camera.y + xOrViewportY;
  if (y === undefined) return undefined;
  return [...actorsInPaintOrder(state)]
    .reverse()
    .filter((actor): actor is WorldActor & { id: ResidentId } =>
      actor.id !== "player"
      && actor.scene === scene
      && (actor.presence === "active" || actor.presence === "entering")
    )
    .find((actor) => {
      const point = actorRenderPoint(actor);
      return Math.hypot(point.x - x, point.y - 1 - y) <= 1.8;
    })?.id;
}

export function actorsInPaintOrder(state: WorldState): readonly WorldActor[] {
  const order = new Map(ACTOR_IDS.map((id, index) => [id, index]));
  return ACTOR_IDS
    .map((id) => state.actors[id])
    .filter((actor) => actor.presence !== "absent")
    .sort((left, right) =>
      actorRenderPoint(left).y - actorRenderPoint(right).y
      || (order.get(left.id) ?? 0) - (order.get(right.id) ?? 0)
    );
}

function projectedResidentCount(state: WorldState): number {
  return RESIDENT_IDS.filter((id) => {
    const presence = state.actors[id].presence;
    return presence === "active" || presence === "entering";
  }).length;
}

function chooseEntryPortal(state: WorldState, id: ResidentId): WorldEntryPortal {
  const occupied = new Set(RESIDENT_IDS.flatMap((otherId) => {
    const other = state.actors[otherId];
    if (otherId === id || other.presence !== "entering") return [];
    return [positionKey(actorWorldPosition(other))];
  }));
  const available = WORLD_ENTRY_PORTALS.filter(({ outside }) => !occupied.has(positionKey(outside)));
  const candidates = available.length > 0 ? available : WORLD_ENTRY_PORTALS;
  const portal = candidates[Math.floor(random(state) * candidates.length)] ?? WORLD_ENTRY_PORTALS[0];
  if (!portal) throw new Error("Monster World has no population entry portals");
  return portal;
}

function spawnResident(state: WorldState, id: ResidentId): void {
  const actor = state.actors[id];
  const portal = chooseEntryPortal(state, id);
  actor.presence = "entering";
  actor.scene = portal.outside.scene;
  actor.x = portal.outside.x;
  actor.y = portal.outside.y;
  actor.direction = directionBetween(portal.outside, portal.inside);
  actor.movement = {
    from: { ...portal.outside },
    to: { ...portal.inside },
    progress: 0,
    durationMs: 920 + random(state) * 420,
  };
  actor.departure = undefined;
  actor.tasks = [];
  actor.bubble = { text: "Springleaf Guild should be just ahead!", untilMs: state.elapsedMs + 4_000 };
  actor.emote = { icon: "music", untilMs: state.elapsedMs + 1_600 };
  actor.activity = `entering from the ${portal.label} trail`;
  actor.intent = "enter Springleaf Guild and join normal town life";
  actor.lastOrigin = "routine";
  state.decisionVersions[id] += 1;
  addActivity(state, "system", `${actor.name} appeared just beyond the ${portal.label} edge.`);
}

function beginResidentExit(state: WorldState, id: ResidentId): boolean {
  const actor = state.actors[id];
  if (actor.presence === "absent" || actor.presence === "exiting") return false;
  const wasEntering = actor.presence === "entering";
  const point = actorRenderPoint(actor);
  const start = actorWorldPosition(actor);
  const portal = wasEntering
    ? WORLD_ENTRY_PORTALS.find(({ outside }) => samePosition(outside, actor.movement?.from ?? start))
      ?? WORLD_ENTRY_PORTALS[0]
    : start.scene === "town"
      ? [...WORLD_ENTRY_PORTALS].sort((left, right) =>
          pointDistance(start, left.inside) - pointDistance(start, right.inside)
          || positionKey(left.inside).localeCompare(positionKey(right.inside))
        )[0]
      : WORLD_ENTRY_PORTALS[stableHash(actor.id) % WORLD_ENTRY_PORTALS.length];
  if (!portal) return false;
  preemptActorOrder(state, actor, "left-world");
  actor.presence = "exiting";
  actor.tasks = [];
  actor.activity = "telling themself it's time to head out";
  actor.intent = "walk to the nearest edge and leave the map";
  actor.lastOrigin = "routine";
  actor.bubble = { text: "Hey, I've gotta get out of this map.", untilMs: state.elapsedMs + 5_000 };
  actor.emote = { icon: "!", untilMs: state.elapsedMs + 1_500 };
  actor.listenerPulse = undefined;
  delete state.heardCalls[id];
  delete state.acknowledgedCallIds[id];
  delete state.playerOrders[id];
  delete state.acknowledgedPlayerOrderIds[id];
  actor.departure = {
    inside: { ...portal.inside },
    outside: { ...portal.outside },
    phase: wasEntering ? "crossing" : "to-edge",
  };
  if (wasEntering) {
    actor.movement = {
      from: { scene: actor.scene, ...point },
      to: { ...portal.outside },
      progress: 0,
      durationMs: 620,
    };
    actor.direction = directionBetween(point, portal.outside);
  } else {
    actor.movement = undefined;
  }
  state.decisionVersions[id] += 1;
  addActivity(state, "routine", `${actor.name} decided to leave town under their own power.`, id);
  return true;
}

export function updateWorld(state: WorldState, deltaMs: number): void {
  const boundedDelta = Math.max(0, Math.min(deltaMs, 100));
  state.elapsedMs += boundedDelta;
  state.minuteOfDay = (state.minuteOfDay + boundedDelta / 1_500) % (24 * 60);
  if (state.elapsedMs >= state.orchardRestockDueMs) {
    while (state.elapsedMs >= state.orchardRestockDueMs) {
      state.orchardRestockDueMs += ORCHARD_RESTOCK_INTERVAL_MS;
      if (state.supplies.orchardBerries >= ORCHARD_CAPACITY) continue;
      state.supplies.orchardBerries += 1;
      addActivity(state, "system", "A ripe sunberry appeared in the orchard.");
      fenceAllResidentDecisions(state);
    }
  }
  if (state.elapsedMs >= state.weatherDueMs) {
    state.weather = state.weather === "clear" ? "drizzle" : "clear";
    state.weatherDueMs = state.elapsedMs + 55_000 + random(state) * 45_000;
    addActivity(
      state,
      "system",
      state.weather === "drizzle"
        ? "A silver drizzle crossed the Mystery Gate."
        : "The drizzle passed over Springleaf Guild.",
    );
    for (const id of RESIDENT_IDS) state.decisionVersions[id] += 1;
  }
  for (const id of ACTOR_IDS) updateActor(state, state.actors[id], boundedDelta);
}

export function movePlayer(state: WorldState, direction: Direction): boolean {
  const player = state.actors.player;
  player.direction = direction;
  if (player.movement) return false;
  const offset = directionOffset(direction);
  const destination = {
    scene: player.scene,
    x: player.x + offset.x,
    y: player.y + offset.y,
  } satisfies WorldPosition;
  if (isWorldPositionBlocked(destination) || !worldStepAvailable(state, player, destination)) return false;
  player.movement = {
    from: actorWorldPosition(player),
    to: destination,
    progress: 0,
    durationMs: tileDuration(player, 115),
  };
  player.activity = `crossing ${sceneLabel(player.scene)}`;
  player.lastOrigin = "player";
  return true;
}

export function playerSpeak(
  state: WorldState,
  input: string,
  voice: VoiceLevel = "call",
  execution: PlayerSpeechExecution = "resident_minds",
): PlayerSpeech | undefined {
  const text = sanitizeDialogue(input);
  if (!text) return undefined;
  const player = state.actors.player;
  player.bubble = { text, untilMs: state.elapsedMs + bubbleDuration(text) };
  player.activity = `said “${text}”`;
  player.lastOrigin = "player";
  const radius = VOICE_RADIUS[voice];
  const guildWide = isGuildRelayActive(state);
  const heardBy = RESIDENT_IDS.filter((id) => {
    const actor = state.actors[id];
    if (actor.presence === "absent" || actor.presence === "exiting") return false;
    return guildWide || actorDistance(player, actor) <= radius;
  });
  const liveHeardBy = heardBy;
  // The voice radius drives the visible in-world reaction. The message board is
  // the shared orchestration primitive, so every present resident can read and
  // independently act on an order posted there.
  const boardReaders = RESIDENT_IDS.filter((id) => {
    const presence = state.actors[id].presence;
    return presence === "active" || presence === "entering";
  });
  const directives = execution === "reducer"
    ? resolvePlayerDirectives(state, text, boardReaders)
    : new Map<ResidentId, PlayerDirective>();
  const callId = state.nextActivityId;
  const order = directives.size > 0
    ? queuePlayerOrder(state, callId, text, directives)
    : undefined;
  const assignedIds = new Set(order?.assigned.map(({ actorId }) => actorId) ?? []);
  // Every active resident receives the same raw board order. Luna decides from
  // the text and that resident's identity whether and how it applies; the
  // deterministic directive parser remains only an immediate physical fast
  // path for the small set of known destinations.
  const liveAddressed = [...boardReaders];
  for (const id of boardReaders) {
    const actor = state.actors[id];
    actor.relativeConstraint = undefined;
    if (heardBy.includes(id)) {
      actor.direction = directionBetween(actorRenderPoint(actor), actorRenderPoint(player));
      actor.emote = { icon: "!", untilMs: state.elapsedMs + 1_100 + (actor.sprite % 5) * 120 };
      actor.listenerPulse = { callId, untilMs: state.elapsedMs + 3_800 };
    }
    if (!assignedIds.has(id)) {
      state.decisionVersions[id] += 1;
      if (actor.presence === "active") {
        preemptActorOrder(state, actor, "superseded");
        actor.tasks = [];
        actor.activity = "interpreting Scout's order";
        actor.intent = `interpret Scout's words: ${text}`;
        actor.lastOrigin = "player";
        actor.routineDueMs = state.elapsedMs + 30_000;
      }
    }
  }
  for (const id of liveAddressed) {
    const requestedTarget = directives.get(id)?.target;
    state.playerOrders[id] = Object.freeze({
      id: callId,
      text,
      coListeners: Object.freeze([...liveAddressed]),
      ...(requestedTarget === undefined ? {} : { requestedTarget }),
    });
    if (heardBy.includes(id)) {
      const listener = state.actors[id];
      const distance = listener.scene === player.scene
        ? Math.round(actorDistance(player, listener) * 10) / 10
        : undefined;
      state.heardCalls[id] = Object.freeze({
        id: callId,
        text,
        voice,
        ...(distance === undefined ? {} : { distance }),
        radius,
        guildWide,
        coListeners: Object.freeze([...liveAddressed]),
        ...(requestedTarget === undefined ? {} : { requestedTarget }),
      });
    } else {
      delete state.heardCalls[id];
    }
  }
  state.speechWave = Object.freeze({ issuedAtMs: state.elapsedMs, voice, radius, guildWide });
  addActivity(
    state,
    "player",
    `Scout ${voice === "shout" ? "shouted" : voice === "whisper" ? "whispered" : "called"}: “${text}”`,
    "player",
    liveHeardBy,
  );
  addGuildMessage(
    state,
    "player",
    text,
    "player",
    undefined,
    "public",
  );
  addActivity(
    state,
    "system",
    guildWide
      ? `The guild relay carried Scout's voice to all ${heardBy.length} on-map residents across every scene.`
      : `${heardBy.length} resident${heardBy.length === 1 ? "" : "s"} heard Scout within ${radius} tiles.`,
  );
  return Object.freeze({
    callId,
    text,
    voice,
    radius,
    guildWide,
    heardBy: Object.freeze(heardBy),
    liveHeardBy: Object.freeze(liveHeardBy),
    liveAddressed: Object.freeze(liveAddressed),
    ...(order === undefined ? {} : { order }),
  });
}

type TargetOccurrence = Readonly<{
  target: WorldTarget;
  start: number;
  end: number;
}>;

type PlayerDirective = Readonly<{
  target: WorldTarget;
  interaction?: WorldInteraction;
}>;

const commandTargetAliases: readonly Readonly<{
  target: WorldTarget;
  aliases: readonly string[];
}>[] = [
  { target: "mission_board", aliases: ["mission board", "message board", "board"] },
  { target: "dungeon_gate", aliases: ["mystery gate", "dungeon gate", "gate"] },
  { target: "player", aliases: ["my character", "scout", "player", "here", "me"] },
  { target: "guild", aliases: ["guild hall", "rescue guild", "guild"] },
  { target: "plaza", aliases: ["guild plaza", "plaza", "center", "centre"] },
  { target: "orchard", aliases: ["sunberry orchard", "orchard"] },
  { target: "pond", aliases: ["whisper pond", "pond", "water"] },
  { target: "shop", aliases: ["trail shop", "shop", "store"] },
  { target: "meadow", aliases: ["training meadow", "meadow", "training field"] },
  { target: "bridge", aliases: ["bell bridge", "bridge"] },
];

function resolvePlayerDirectives(
  state: WorldState,
  text: string,
  heardBy: readonly ResidentId[],
): ReadonlyMap<ResidentId, PlayerDirective> {
  const normalized = text.toLowerCase();
  const occurrences = commandTargetOccurrences(state, normalized);
  const directives = new Map<ResidentId, PlayerDirective>();
  let clauseStart = 0;
  occurrences.forEach((occurrence, index) => {
    const clause = normalized.slice(clauseStart, occurrence.start);
    const directive = playerDirectiveForClause(clause, occurrence.target);
    clauseStart = occurrence.end;
    if (!directive) return;
    const named = heardBy.filter((id) => actorMentioned(state, id, clause));
    const rest = /\b(the rest|everyone else|everybody else|all others|the others)\b/.test(clause);
    const everyone = /\b(everyone|everybody|all residents|all agents|all of you)\b/.test(clause);
    if (named.length > 0) {
      for (const id of named) directives.set(id, directive);
    } else if (rest) {
      for (const id of heardBy) {
        if (!directives.has(id)) directives.set(id, directive);
      }
    } else if (everyone || index === 0) {
      for (const id of heardBy) directives.set(id, directive);
    }
  });
  return directives;
}

function queuePlayerOrder(
  state: WorldState,
  orderId: number,
  text: string,
  directives: ReadonlyMap<ResidentId, PlayerDirective>,
): WorldOrderReceipt {
  const assignments: WorldOrderAssignment[] = [];
  for (const [actorId, directive] of directives) {
    const { target, interaction } = directive;
    const group = [...directives].filter(([, candidate]) =>
      candidate.target === target && candidate.interaction === interaction
    );
    const groupIndex = group.findIndex(([candidateId]) => candidateId === actorId);
    const actor = state.actors[actorId];
    state.decisionVersions[actorId] += 1;
    if (actor.presence !== "active") {
      assignments.push({
        actorId,
        target,
        ...(interaction === undefined ? {} : { interaction }),
        status: "rejected",
        completedAtMs: state.elapsedMs,
        reason: "not-active",
      });
      continue;
    }

    preemptActorOrder(state, actor, "superseded");
    actor.tasks = [];
    actor.movement = undefined;
    actor.departure = undefined;
    const targetGoal = targetPosition(state, actor, target);
    const desiredGoal = interaction === undefined && group.length > 1
      ? coordinatedOrderGoal(
          targetGoal,
          groupIndex,
          group.length,
          group.map(([memberId]) => actorWorldPosition(state.actors[memberId])),
        )
      : targetGoal;
    const route = safeOrderRoute(
      state,
      actor,
      desiredGoal,
      interaction === undefined ? Math.max(2, Math.ceil(group.length / 4)) : 0,
    );
    if (!route) {
      assignments.push({
        actorId,
        target,
        ...(interaction === undefined ? {} : { interaction }),
        status: "rejected",
        completedAtMs: state.elapsedMs,
        reason: "unreachable",
      });
      actor.activity = `could not find a route to ${targetLabel(state, target)}`;
      actor.intent = undefined;
      actor.lastOrigin = "player";
      continue;
    }
    const { goal, path } = route;

    const assignment: WorldOrderAssignment = {
      actorId,
      target,
      ...(interaction === undefined ? {} : { interaction }),
      goal,
      status: "assigned",
    };
    assignments.push(assignment);
    actor.activeOrderId = orderId;
    actor.tasks = [{
      action: interaction === undefined
        ? Object.freeze({ kind: "move", target })
        : Object.freeze({ kind: "interact", target, action: interaction }),
      origin: "player",
      requestId: `scout-order-${orderId}-${actorId}`,
      orderId,
      goal,
      path: path.map((point) => ({ ...point })),
    }];
    const assignmentVerb = interaction === undefined ? "go to" : `${interaction} at`;
    actor.activity = `assigned to ${assignmentVerb} ${targetLabel(state, target)}`;
    actor.intent = `obey Scout: ${assignmentVerb} ${targetLabel(state, target)}`;
    actor.lastOrigin = "player";
    actor.bubble = { text: "On my way, Scout!", untilMs: state.elapsedMs + 2_800 };
    actor.routineDueMs = state.elapsedMs + 9_000;
  }

  const order: WorldOrder = {
    id: orderId,
    text,
    issuedAtMs: state.elapsedMs,
    assignments,
    completionEmitted: false,
  };
  state.orders.unshift(order);
  maybeEmitOrderCompletion(state, order);
  pruneWorldOrders(state);

  return Object.freeze({
    id: orderId,
    assigned: Object.freeze(assignments.flatMap((assignment) =>
      assignment.status === "assigned" && assignment.goal
        ? [Object.freeze({
            actorId: assignment.actorId,
            target: assignment.target,
            ...(assignment.interaction === undefined ? {} : { interaction: assignment.interaction }),
            goal: assignment.goal,
          })]
        : []
    )),
    rejected: Object.freeze(assignments.flatMap((assignment) =>
      assignment.status === "rejected"
        ? [Object.freeze({
            actorId: assignment.actorId,
            target: assignment.target,
            reason: assignment.reason === "unreachable" ? "unreachable" as const : "not-active" as const,
          })]
        : []
    )),
  });
}

function coordinatedOrderGoal(
  anchor: WorldPosition,
  index: number,
  count: number,
  origins: readonly WorldPosition[],
): WorldPosition {
  if (count <= 4) {
    const sameScene = origins.filter(({ scene }) => scene === anchor.scene);
    const averageX = sameScene.reduce((total, point) => total + point.x, 0) / Math.max(sameScene.length, 1);
    const averageY = sameScene.reduce((total, point) => total + point.y, 0) / Math.max(sameScene.length, 1);
    const rank = Math.max(index, 0) - (count - 1) / 2;
    if (Math.abs(averageX - anchor.x) >= Math.abs(averageY - anchor.y)) {
      return Object.freeze({
        scene: anchor.scene,
        x: anchor.x + (averageX < anchor.x ? -2 : 2),
        y: anchor.y + Math.round(rank * 2),
      });
    }
    return Object.freeze({
      scene: anchor.scene,
      x: anchor.x + Math.round(rank * 2),
      y: anchor.y + (averageY < anchor.y ? -2 : 2),
    });
  }
  const radius = Math.max(2, Math.ceil(count / 5));
  const angle = (Math.PI * 2 * Math.max(index, 0)) / Math.max(count, 1);
  return Object.freeze({
    scene: anchor.scene,
    x: anchor.x + Math.round(radius * Math.cos(angle)),
    y: anchor.y + Math.round(radius * Math.sin(angle)),
  });
}

function safeOrderRoute(
  state: WorldState,
  actor: WorldActor,
  desired: WorldPosition,
  maximumRadius: number,
): Readonly<{ goal: WorldPosition; path: readonly WorldPosition[] }> | undefined {
  const start = actorWorldPosition(actor);
  const candidates: WorldPosition[] = [];
  for (let radius = 0; radius <= maximumRadius; radius += 1) {
    for (let yOffset = -radius; yOffset <= radius; yOffset += 1) {
      for (let xOffset = -radius; xOffset <= radius; xOffset += 1) {
        if (Math.max(Math.abs(xOffset), Math.abs(yOffset)) !== radius) continue;
        const goal = Object.freeze({
          scene: desired.scene,
          x: desired.x + xOffset,
          y: desired.y + yOffset,
        });
        if (
          isWorldPositionBlocked(goal)
          || portalDestinationAt(goal) !== undefined
          || positionClaimedByOther(state, actor.id, goal)
        ) continue;
        candidates.push(goal);
      }
    }
  }
  candidates.sort((left, right) =>
    positionDistance(desired, left) - positionDistance(desired, right)
    || positionDistance(start, left) - positionDistance(start, right)
  );
  for (const goal of candidates) {
    const path = findWorldRoute(start, goal);
    if (path.length > 0 || samePosition(start, goal)) {
      return Object.freeze({ goal, path });
    }
  }
  return undefined;
}

function commandTargetOccurrences(state: WorldState, text: string): TargetOccurrence[] {
  const landmarkCandidates = commandTargetAliases.flatMap(({ target, aliases }) =>
    aliases.flatMap((alias) => {
      const matches: TargetOccurrence[] = [];
      const pattern = new RegExp(`\\b${escapeRegExp(alias)}\\b`, "g");
      for (const match of text.matchAll(pattern)) {
        const start = match.index ?? 0;
        matches.push({ target, start, end: start + alias.length });
      }
      return matches;
    })
  );
  const actorCandidates = ACTOR_IDS.flatMap((id): TargetOccurrence[] => {
    if (id === "player") return [];
    const actor = state.actors[id];
    if (actor.presence === "absent" || actor.presence === "exiting") return [];
    const aliases = new Set([id, actor.name.toLowerCase()]);
    return [...aliases].flatMap((alias): TargetOccurrence[] => {
      const matches: TargetOccurrence[] = [];
      const pattern = new RegExp(`\\b${escapeRegExp(alias)}\\b`, "g");
      for (const match of text.matchAll(pattern)) {
        const start = match.index ?? 0;
        const prefix = text.slice(Math.max(0, start - 32), start);
        if (!/\b(?:to|towards?|follow|meet|join|greet|inspect|check|find|visit|help)\s+(?:with\s+)?$/.test(prefix)) {
          continue;
        }
        matches.push({ target: id, start, end: start + alias.length });
      }
      return matches;
    });
  });
  const candidates = [...landmarkCandidates, ...actorCandidates]
    .sort((left, right) => left.start - right.start || right.end - right.start - (left.end - left.start));
  const selected: TargetOccurrence[] = [];
  for (const candidate of candidates) {
    if (selected.some(({ start, end }) => candidate.start < end && candidate.end > start)) continue;
    selected.push(candidate);
  }
  if (selected.length === 0 && /\b(come|follow me|stay close|regroup)\b/.test(text)) {
    selected.push({ target: "player", start: text.length, end: text.length });
  }
  return selected.sort((left, right) => left.start - right.start);
}

function playerDirectiveForClause(
  clause: string,
  target: WorldTarget,
): PlayerDirective | undefined {
  const interaction = interactionIntent(clause, target);
  if (interaction !== undefined) return Object.freeze({ target, interaction });
  if (/\b(go|move|walk|head|come|follow|meet|join|visit|help|stay|regroup|assemble|gather)\b/.test(clause)) {
    return Object.freeze({ target });
  }
  return undefined;
}

function interactionIntent(
  clause: string,
  target: WorldTarget,
): WorldInteraction | undefined {
  if (/\b(inspect|check|search|examine|investigate|look at)\b/.test(clause)) return "inspect";
  if (/\b(splash|test the (?:water|current))\b/.test(clause)) return "splash";
  if (/\b(rest|recover|sleep)\b/.test(clause)) return "rest";
  if (/\b(train|practice|spar)\b/.test(clause)) return "train";
  if (/\b(greet|welcome|say hello)\b/.test(clause)) return "greet";
  if (/\b(post|pin|publish)\b/.test(clause)) return "post";
  if (/\b(offer|deliver|drop off|hand in|bring)\b/.test(clause)) return "offer";
  if (
    (target === "orchard" || target === "shop")
    && /\b(gather|collect|harvest|pick up|take)\b/.test(clause)
  ) return "gather";
  return undefined;
}

function actorMentioned(state: WorldState, id: ResidentId, text: string): boolean {
  const actor = state.actors[id];
  return [actor.id, actor.name].some((value) =>
    new RegExp(`\\b${escapeRegExp(value.toLowerCase())}\\b`).test(text)
  );
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function preemptActorOrder(
  state: WorldState,
  actor: WorldActor,
  reason: "superseded" | "left-world",
): void {
  const orderId = actor.activeOrderId;
  if (orderId === undefined) return;
  const order = state.orders.find(({ id }) => id === orderId);
  const assignment = order?.assignments.find(({ actorId }) => actorId === actor.id);
  if (assignment && (assignment.status === "assigned" || assignment.status === "moving")) {
    assignment.status = "preempted";
    assignment.reason = reason;
    assignment.completedAtMs = state.elapsedMs;
  }
  actor.activeOrderId = undefined;
  if (order) {
    maybeEmitOrderCompletion(state, order);
    pruneWorldOrders(state);
  }
}

function maybeEmitOrderCompletion(state: WorldState, order: WorldOrder): void {
  if (order.completionEmitted || order.assignments.some(({ status }) =>
    status === "assigned" || status === "moving"
  )) return;
  order.completionEmitted = true;
  for (const assignment of order.assignments) {
    const actor = state.actors[assignment.actorId];
    if (actor.activeOrderId !== order.id) continue;
    actor.activeOrderId = undefined;
    actor.routineDueMs = state.elapsedMs + 9_000;
  }
  const completed = order.assignments.filter(({ status }) => status === "completed").length;
  const rejected = order.assignments.filter(({ status }) => status === "rejected").length;
  const preempted = order.assignments.filter(({ status }) => status === "preempted").length;
  const text = order.assignments.length > 0 && completed === order.assignments.length
    ? `Scout's order ${order.id} complete: ${completed}/${order.assignments.length} residents arrived.`
    : `Scout's order ${order.id} settled: ${completed}/${order.assignments.length} completed; ${rejected} rejected; ${preempted} preempted.`;
  addActivity(state, "player", text, "player");
}

function pruneWorldOrders(state: WorldState): void {
  const active = state.orders.filter((order) => order.assignments.some(({ status }) =>
    status === "assigned" || status === "moving"
  ));
  const terminal = state.orders
    .filter((order) => !active.includes(order))
    .slice(0, RETAINED_TERMINAL_ORDERS);
  state.orders = [...active, ...terminal].sort((left, right) => right.id - left.id);
}

function samePosition(left: WorldPosition, right: WorldPosition): boolean {
  return left.scene === right.scene && left.x === right.x && left.y === right.y;
}

function observedDestination(actor: WorldActor): WorldPosition | undefined {
  const destination = actor.tasks[0]?.goal ?? actor.movement?.to;
  return destination === undefined ? undefined : Object.freeze({ ...destination });
}

function positionClaimedByOther(
  state: WorldState,
  actorId: ActorId,
  candidate: WorldPosition,
): boolean {
  return ACTOR_IDS.some((otherId) => {
    if (otherId === actorId) return false;
    const other = state.actors[otherId];
    if (other.presence !== "active" && otherId !== "player") return false;
    return samePosition(actorWorldPosition(other), candidate)
      || (other.movement !== undefined && samePosition(other.movement.to, candidate))
      || (other.tasks[0]?.goal !== undefined && samePosition(other.tasks[0].goal, candidate));
  });
}

function worldStepAvailable(
  state: WorldState,
  actor: WorldActor,
  destination: WorldPosition,
): boolean {
  const origin = actorWorldPosition(actor);
  return ACTOR_IDS.every((otherId) => {
    if (otherId === actor.id) return true;
    const other = state.actors[otherId];
    if (other.presence !== "active" && otherId !== "player") return true;
    if (samePosition(actorWorldPosition(other), destination)) return false;
    if (other.movement === undefined) return true;
    if (samePosition(other.movement.to, destination)) return false;
    return !(samePosition(other.movement.from, destination) && samePosition(other.movement.to, origin));
  });
}

export function isGuildRelayActive(state: WorldState): boolean {
  return positionDistance(actorWorldPosition(state.actors.player), WORLD_POIS.plaza)
    <= GUILD_RELAY_RADIUS;
}

export function hasUnansweredGuildCall(state: WorldState, agentId: ResidentId): boolean {
  const heard = state.heardCalls[agentId]?.id;
  return heard !== undefined && heard !== state.acknowledgedCallIds[agentId];
}

export function hasUnansweredPlayerOrder(state: WorldState, agentId: ResidentId): boolean {
  const order = state.playerOrders[agentId]?.id;
  return order !== undefined && order !== state.acknowledgedPlayerOrderIds[agentId];
}

export function completeResidentInstruction(
  state: WorldState,
  agentId: ResidentId,
  callId: number | undefined,
): boolean {
  if (callId === undefined) return false;
  if (state.heardCalls[agentId]?.id === callId) {
    state.acknowledgedCallIds[agentId] = callId;
  }
  if (state.playerOrders[agentId]?.id !== callId) return false;
  state.acknowledgedPlayerOrderIds[agentId] = callId;
  return true;
}

function pendingDecisionCallId(state: WorldState, agentId: ResidentId): number | undefined {
  if (hasUnansweredPlayerOrder(state, agentId)) return state.playerOrders[agentId]?.id;
  if (hasUnansweredGuildCall(state, agentId)) return state.heardCalls[agentId]?.id;
  return undefined;
}

export function playerInteract(state: WorldState): void {
  const player = state.actors.player;
  const physicalInteraction = nearestReducerInteraction(player);
  if (physicalInteraction) {
    resolveInteraction(
      state,
      player,
      physicalInteraction.target,
      physicalInteraction.interaction,
      "player",
    );
    return;
  }
  const nearby = RESIDENT_IDS
    .filter((id) => state.actors[id].presence === "active")
    .map((id) => ({ actor: state.actors[id], distance: actorDistance(player, state.actors[id]) }))
    .sort((left, right) => left.distance - right.distance)[0];
  if (nearby && nearby.distance <= 2.6) {
    const text = `${nearby.actor.name}, what's our next move?`;
    player.bubble = { text, untilMs: state.elapsedMs + bubbleDuration(text) };
    player.activity = `checked in with ${nearby.actor.name}`;
    addActivity(state, "player", `Scout checked in with ${nearby.actor.name}.`, "player");
    if (isResidentId(nearby.actor.id)) state.decisionVersions[nearby.actor.id] += 1;
    nearby.actor.emote = { icon: "!", untilMs: state.elapsedMs + 1_500 };
    return;
  }
  const poi = nearestPoi(player);
  if (poi.distance <= 2.8) {
    const text = poi.target === "mission_board"
      ? "Missing: one silver rescue bell. Last heard before dawn."
      : `You inspect ${poi.label}.`;
    player.bubble = { text, untilMs: state.elapsedMs + bubbleDuration(text) };
    addActivity(state, "player", text, "player");
    for (const id of RESIDENT_IDS) state.decisionVersions[id] += 1;
    return;
  }
  player.emote = { icon: "?", untilMs: state.elapsedMs + 1_200 };
}

export function applyWorldPlan(state: WorldState, plan: WorldPlan): PlanApplication {
  if (!isWorldPlan(plan) || plan.origin !== "nanocodex" || !isResidentId(plan.agentId)) {
    return { accepted: false, reason: "invalid" };
  }
  const actor = state.actors[plan.agentId];
  if (actor.presence !== "active" || !planTargetsPresent(state, plan.steps)) {
    return { accepted: false, reason: "invalid" };
  }
  if (actor.activeOrderId !== undefined) return { accepted: false, reason: "stale" };
  if (state.seenRequestIds.includes(plan.requestId)) {
    return { accepted: false, reason: "duplicate" };
  }
  if (state.decisionVersions[plan.agentId] !== plan.stateVersion) {
    return { accepted: false, reason: "stale" };
  }
  const pendingCallId = pendingDecisionCallId(state, plan.agentId);
  if (pendingCallId !== undefined && pendingCallId !== plan.heardCallId) {
    return { accepted: false, reason: "stale" };
  }
  if (
    plan.heardCallId !== undefined
    && state.heardCalls[plan.agentId]?.id !== plan.heardCallId
    && state.playerOrders[plan.agentId]?.id !== plan.heardCallId
  ) {
    return { accepted: false, reason: "stale" };
  }
  if (actor.tasks.some(({ origin }) => origin === "nanocodex")) {
    return { accepted: false, reason: "stale" };
  }
  actor.tasks = [];
  actor.tasks.push(...tasksFor(plan.steps, plan.origin, plan.requestId));
  actor.activity = plan.summary;
  actor.intent = plan.summary;
  actor.lastOrigin = "nanocodex";
  actor.routineDueMs = state.elapsedMs + 9_000;
  state.decisionVersions[plan.agentId] += 1;
  if (plan.heardCallId !== undefined && state.heardCalls[plan.agentId]?.id === plan.heardCallId) {
    state.acknowledgedCallIds[plan.agentId] = plan.heardCallId;
  }
  if (plan.heardCallId !== undefined && state.playerOrders[plan.agentId]?.id === plan.heardCallId) {
    state.acknowledgedPlayerOrderIds[plan.agentId] = plan.heardCallId;
  }
  state.seenRequestIds.push(plan.requestId);
  if (state.seenRequestIds.length > 128) state.seenRequestIds.shift();
  state.agentDecisions += 1;
  addActivity(
    state,
    "nanocodex",
    `${actor.name} decided to ${lowercaseSentence(plan.summary)}: ${plan.steps.map(actionLabel).join(" → ")}.`,
    actor.id,
  );
  return { accepted: true };
}

export function applyWorldToolAction(
  state: WorldState,
  request: Omit<WorldToolAction, "startedAtMs" | "activityCursor">,
): WorldToolActionApplication {
  const actor = state.actors[request.agentId];
  if (
    actor.presence !== "active"
    || !actionTargetsPresent(state, request.action)
    || (
      (request.action.kind === "move_relative" || request.action.kind === "maintain_relative")
      && request.action.anchor === request.agentId
    )
    || (
      request.action.kind === "maintain_relative"
      && maintainedRelativeCycle(state, request.agentId, request.action.anchor)
    )
  ) return { accepted: false, reason: "invalid" };
  if (state.seenRequestIds.includes(request.actionId)) {
    return { accepted: false, reason: "duplicate" };
  }
  const pendingCallId = pendingDecisionCallId(state, request.agentId);
  if (pendingCallId !== undefined && pendingCallId !== request.heardCallId) {
    return { accepted: false, reason: "superseded" };
  }

  // The reducer is the single writer. A later embodied call from this resident
  // replaces only its own earlier control horizon.
  if (request.action.kind === "maintain_relative") {
    actor.relativeConstraint = Object.freeze({
      action: request.action,
      requestId: request.actionId,
    });
    actor.tasks = [];
    ensureRelativeControllerTask(state, actor);
  } else {
    if (
      request.action.kind === "move"
      || request.action.kind === "move_relative"
      || request.action.kind === "interact"
    ) actor.relativeConstraint = undefined;
    actor.tasks = tasksFor([request.action], "nanocodex", request.actionId);
  }
  actor.intent = actionLabel(request.action);
  actor.activity = actionLabel(request.action);
  actor.lastOrigin = "nanocodex";
  actor.routineDueMs = state.elapsedMs + 9_000;
  state.decisionVersions[request.agentId] += 1;
  state.seenRequestIds.push(request.actionId);
  if (state.seenRequestIds.length > 128) state.seenRequestIds.shift();
  state.agentDecisions += 1;
  return {
    accepted: true,
    pending: Object.freeze({
      ...request,
      startedAtMs: state.elapsedMs,
      activityCursor: state.nextActivityId - 1,
    }),
  };
}

export function applyWorldRoomSend(
  state: WorldState,
  request: Readonly<{
    sendId: string;
    requestId: string;
    agentId: ResidentId;
    heardCallId?: number;
    text: string;
  }>,
): WorldRoomSendApplication {
  const actor = state.actors[request.agentId];
  const text = sanitizeDialogue(request.text);
  if (actor.presence !== "active" || !text || text.length > 140) {
    return { accepted: false, reason: "invalid" };
  }
  if (state.seenRequestIds.includes(request.sendId)) {
    return { accepted: false, reason: "duplicate" };
  }
  state.seenRequestIds.push(request.sendId);
  if (state.seenRequestIds.length > 128) state.seenRequestIds.shift();
  addGuildMessage(state, actor.id, text, "nanocodex");
  const message = state.guildMessages[0];
  if (!message) return { accepted: false, reason: "invalid" };
  return Object.freeze({
    accepted: true,
    message: boardMessageForObservation(state, message),
  });
}

export function worldToolResultAtDecisionBoundary(
  state: WorldState,
  pending: WorldToolAction,
  controlHorizonMs = 1_200,
): WorldToolResult | undefined {
  const actor = state.actors[pending.agentId];
  const newerCallId = pendingDecisionCallId(state, pending.agentId);
  if (newerCallId !== undefined && newerCallId !== pending.heardCallId) {
    return worldToolResult(state, pending, "superseded", "A newer instruction arrived.");
  }
  const active = actor.tasks.some(({ requestId }) => requestId === pending.actionId);
  const normalizedActivity = actor.activity.toLowerCase();
  if (
    active
    && (
      normalizedActivity.includes("waiting for a clear")
      || normalizedActivity.includes("could not")
      || normalizedActivity.includes("gave up")
    )
  ) {
    return worldToolResult(state, pending, "blocked", actor.activity);
  }
  if (
    pending.action.kind === "maintain_relative"
    && actor.relativeConstraint?.requestId === pending.actionId
    && !active
  ) {
    return worldToolResult(
      state,
      pending,
      "in_progress",
      `${actor.activity}; the relative controller remains active`,
    );
  }
  if (!active) {
    const status = normalizedActivity.includes("could not") || normalizedActivity.includes("gave up")
      ? "blocked"
      : "completed";
    return worldToolResult(state, pending, status, actor.activity);
  }
  if (state.elapsedMs - pending.startedAtMs >= controlHorizonMs) {
    return worldToolResult(state, pending, "in_progress", actor.activity);
  }
  return undefined;
}

export function rejectedWorldToolResult(
  state: WorldState,
  agentId: ResidentId,
  action: WorldPrimitiveAction,
  status: "rejected" | "superseded",
  detail: string,
): WorldToolResult {
  return worldToolResult(state, {
    actionId: "rejected",
    requestId: "rejected",
    agentId,
    action,
    startedAtMs: state.elapsedMs,
    activityCursor: state.nextActivityId - 1,
  }, status, detail);
}

function worldToolResult(
  state: WorldState,
  pending: WorldToolAction,
  status: WorldToolResult["outcome"]["status"],
  detail: string,
): WorldToolResult {
  const observation = observationFor(state, pending.agentId);
  const relevantEvents = state.activities
    .filter(({ id, actorId, audience }) => (
      id > pending.activityCursor
      && (audience === undefined || audience.includes(pending.agentId))
      && (actorId === undefined || actorId === pending.agentId || actorId === "player")
    ))
    .slice(-6)
    .map(({ text }) => text);
  return Object.freeze({
    worldRevision: observation.stateVersion,
    outcome: Object.freeze({ status, action: pending.action, detail }),
    self: observation.self,
    nearby: observation.nearby,
    roster: observation.roster,
    ...(observation.playerOrder === undefined ? {} : { playerOrder: observation.playerOrder }),
    ...(observation.guildCall === undefined ? {} : { guildCall: observation.guildCall }),
    relevantEvents: Object.freeze(relevantEvents),
  });
}

function planTargetsPresent(state: WorldState, steps: readonly WorldAction[]): boolean {
  return steps.every((step) => actionTargetsPresent(state, step));
}

function actionTargetsPresent(state: WorldState, action: WorldAction): boolean {
  if (action.kind === "random_choice") {
    return action.if_true.every((step) => actionTargetsPresent(state, step))
      && action.if_false.every((step) => actionTargetsPresent(state, step));
  }
  if (action.kind === "say") {
    return action.to === undefined
      || action.to === "player"
      || state.actors[action.to].presence === "active";
  }
  if (action.kind === "move_relative") {
    return action.anchor === "player" || state.actors[action.anchor].presence === "active";
  }
  if (action.kind === "maintain_relative") {
    return action.anchor === "player" || state.actors[action.anchor].presence === "active";
  }
  if (action.kind !== "move" && action.kind !== "interact") return true;
  return !isActorId(action.target)
    || action.target === "player"
    || state.actors[action.target].presence === "active";
}

export function observationFor(
  state: WorldState,
  agentId: ResidentId,
): WorldObservation {
  const actor = state.actors[agentId];
  const selfPoint = actorRenderPoint(actor);
  const nearby = ACTOR_IDS
    .filter((id) => id !== agentId)
    .filter((id) => id === "player" || state.actors[id].presence === "active")
    .map((id) => {
      const other = state.actors[id];
      const point = actorRenderPoint(other);
      const movingTo = observedDestination(other);
      return {
        id,
        name: other.name,
        kind: other.kind,
        scene: other.scene,
        x: Math.round(point.x * 10) / 10,
        y: Math.round(point.y * 10) / 10,
        relativeX: Math.round((point.x - selfPoint.x) * 10) / 10,
        relativeY: Math.round((point.y - selfPoint.y) * 10) / 10,
        distance: Math.round(actorDistance(actor, other) * 10) / 10,
        direction: other.direction,
        ...(movingTo === undefined ? {} : { movingTo }),
        activity: other.activity,
        ...(other.intent === undefined ? {} : { intent: other.intent }),
      };
    })
    .filter(({ distance }) => distance <= VOICE_RADIUS.shout)
    .sort((left, right) => left.distance - right.distance)
    .map((entry) => Object.freeze(entry));
  const guildCall = hasUnansweredGuildCall(state, agentId)
    ? state.heardCalls[agentId]
    : undefined;
  const playerOrder = hasUnansweredPlayerOrder(state, agentId)
    ? state.playerOrders[agentId]
    : undefined;
  const availableTargets = WORLD_TARGETS.filter((target) =>
    !isActorId(target)
    || target === "player"
    || state.actors[target].presence === "active"
  );
  const roster = actorsInPaintOrder(state).map((other) => {
    const point = actorRenderPoint(other);
    const movingTo = observedDestination(other);
    return Object.freeze({
      id: other.id,
      name: other.name,
      kind: other.kind,
      scene: other.scene,
      x: Math.round(point.x * 10) / 10,
      y: Math.round(point.y * 10) / 10,
      direction: other.direction,
      ...(movingTo === undefined ? {} : { movingTo }),
      location: locationFor(other),
      activity: other.activity,
    });
  });
  return Object.freeze({
    stateVersion: state.decisionVersions[agentId],
    minuteOfDay: Math.floor(state.minuteOfDay),
    weather: state.weather,
    self: Object.freeze({
      id: agentId,
      name: actor.name,
      role: actor.role,
      kind: actor.kind === "human" ? "human" : "monster",
      scene: actor.scene,
      x: Math.round(selfPoint.x * 10) / 10,
      y: Math.round(selfPoint.y * 10) / 10,
      direction: actor.direction,
      ...(observedDestination(actor) === undefined ? {} : { movingTo: observedDestination(actor) }),
      location: locationFor(actor),
      energy: Math.round(actor.energy),
      curiosity: Math.round(actor.curiosity),
      social: Math.round(actor.social),
      ...(actor.carrying === undefined ? {} : { carrying: actor.carrying }),
    }),
    nearby: Object.freeze(nearby),
    roster: Object.freeze(roster),
    ...(playerOrder === undefined ? {} : { playerOrder }),
    ...(guildCall === undefined ? {} : { guildCall }),
    guildBoard: Object.freeze(
      state.guildMessages
        .filter(({ audience }) => audience === undefined || audience.includes(agentId))
        .slice(0, WORLD_ROOM_RETENTION)
        .map((message) => boardMessageForObservation(state, message)),
    ),
    recentEvents: Object.freeze(
      state.activities
        .filter(({ audience }) => audience === undefined || audience.includes(agentId))
        .slice(0, 6)
        .map(({ text }) => text),
    ),
    availableTargets: Object.freeze(availableTargets),
    supplies: Object.freeze({ ...state.supplies }),
  });
}

export function setWorldAgentsOnline(state: WorldState, online: boolean): void {
  if (state.agentsOnline === online) return;
  state.agentsOnline = online;
  const activeResidents = liveAgentIdsInWorld(state);
  for (const id of activeResidents) {
    const actor = state.actors[id];
    state.decisionVersions[id] += 1;
    if (!online) continue;
    actor.tasks = actor.tasks.filter(({ origin }) => origin !== "routine");
    if (actor.activeOrderId !== undefined || actor.tasks.length > 0) continue;
    actor.activity = actor.movement
      ? "finishing the current step before Luna's next decision"
      : "waiting for Luna's next decision";
    actor.intent = "let Luna choose the next autonomous action";
    actor.lastOrigin = "system";
  }
  addActivity(
    state,
    "system",
    online
      ? `Luna control is online for all ${activeResidents.length} active residents currently on the map; future autonomous choices come from Luna.`
      : `Luna control is offline for all ${activeResidents.length} active residents currently on the map; deterministic fallback routines resume when they are idle.`,
  );
}

export function serializeWorldState(state: WorldState): string {
  return JSON.stringify({
    version: 3,
    elapsedMs: state.elapsedMs,
    minuteOfDay: state.minuteOfDay,
    weather: state.weather,
    weatherDueMs: state.weatherDueMs,
    missionStage: state.mission.stage,
    rng: state.rng,
    populationTarget: state.populationTarget,
    supplies: state.supplies,
    orchardRestockDueMs: state.orchardRestockDueMs,
    actors: Object.fromEntries(ACTOR_IDS.map((id) => {
      const actor = state.actors[id];
      const position = persistedActorPosition(actor);
      return [id, {
        position,
        direction: actor.direction,
        ...(actor.carrying === undefined ? {} : { carrying: actor.carrying }),
        energy: actor.energy,
        curiosity: actor.curiosity,
        social: actor.social,
        routineIndex: actor.routineIndex,
        present: actor.presence === "active" || actor.presence === "entering",
      }];
    })),
    activities: state.activities.slice(0, 16),
    guildMessages: state.guildMessages.slice(0, WORLD_ROOM_RETENTION),
    agentDecisions: state.agentDecisions,
    residentMemories: state.residentMemories,
  });
}

function persistedActorPosition(actor: WorldActor): WorldPosition {
  if (actor.presence === "entering") {
    return actor.movement?.to ?? actor.departure?.inside ?? actorWorldPosition(actor);
  }
  if (actor.presence === "exiting") {
    const candidate = actor.movement?.to ?? actor.departure?.inside ?? actorWorldPosition(actor);
    return isWorldPositionBlocked(candidate) ? actorWorldPosition(actor) : candidate;
  }
  return actor.movement?.to ?? actorWorldPosition(actor);
}

export function formatWorldTime(minuteOfDay: number): string {
  const minute = Math.floor(minuteOfDay) % (24 * 60);
  const hours = Math.floor(minute / 60);
  return `${String(hours).padStart(2, "0")}:${String(minute % 60).padStart(2, "0")}`;
}

export function actorRenderPoint(actor: WorldActor): Point {
  const movement = actor.movement;
  if (!movement) return { x: actor.x, y: actor.y };
  const progress = easeInOut(Math.min(1, movement.progress));
  return {
    x: movement.from.x + (movement.to.x - movement.from.x) * progress,
    y: movement.from.y + (movement.to.y - movement.from.y) * progress,
  };
}

export function actorWorldPosition(actor: WorldActor): WorldPosition {
  return Object.freeze({ scene: actor.scene, x: actor.x, y: actor.y });
}

export function worldCameraForState(state: WorldState): WorldCamera {
  return cameraForPosition(actorWorldPosition(state.actors.player));
}

export function isBlocked(x: number, y: number, scene: WorldSceneId = "town"): boolean {
  return isMapBlocked(x, y, scene);
}

function createActors(): Record<ActorId, WorldActor> {
  return Object.fromEntries(
    ACTOR_IDS.map((id, index) => [id, createActor(id, index)]),
  ) as Record<ActorId, WorldActor>;
}

function createDecisionVersions(): Record<ResidentId, number> {
  return Object.fromEntries(
    RESIDENT_IDS.map((id) => [id, 1]),
  ) as Record<ResidentId, number>;
}

function createResidentMemories(): Record<ResidentId, WorldResidentMemory> {
  return Object.fromEntries(
    RESIDENT_IDS.map((id) => [id, copyResidentMemory({})]),
  ) as Record<ResidentId, WorldResidentMemory>;
}

function copyResidentMemory(value: unknown): WorldResidentMemory {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return decodeWorldResidentMemory({});
  }
  const memory = value as Partial<WorldResidentMemory>;
  try {
    return decodeWorldResidentMemory({
      summary: memory.summary,
      goals: memory.goals,
      relationships: memory.relationships,
      recent_decisions: memory.recentDecisions,
      last_board_message_id: memory.lastBoardMessageId,
    });
  } catch {
    return decodeWorldResidentMemory({});
  }
}

function createActor(id: ActorId, index: number): WorldActor {
  const definition = actorDefinitions[id];
  return {
    id,
    ...definition,
    scene: "town",
    presence: id !== "player" && index > BASE_RESIDENT_COUNT ? "absent" : "active",
    direction: "down",
    tasks: [],
    activity: id === "player" ? "exploring the guild grounds" : "starting the morning round",
    lastOrigin: id === "player" ? "player" : "routine",
    energy: 68 + (index * 7) % 29,
    curiosity: 54 + (index * 11) % 43,
    social: 57 + (index * 13) % 39,
    routineIndex: id !== "player" && specialRoutineScripts[id as ResidentId] ? 0 : index,
    routineDueMs: 700 + index * 560,
  };
}

function createSupplies(): MutableWorldSupplyState {
  return {
    orchardBerries: ORCHARD_CAPACITY,
    shopStock: 1,
    guildSupplies: 0,
    trainingMarks: 0,
  };
}

function updateActor(state: WorldState, actor: WorldActor, deltaMs: number): void {
  if (actor.presence === "absent") return;
  if (actor.bubble && actor.bubble.untilMs <= state.elapsedMs) actor.bubble = undefined;
  if (actor.emote && actor.emote.untilMs <= state.elapsedMs) actor.emote = undefined;
  if (actor.effect && actor.effect.untilMs <= state.elapsedMs) actor.effect = undefined;
  if (actor.listenerPulse && actor.listenerPulse.untilMs <= state.elapsedMs) {
    actor.listenerPulse = undefined;
  }
  if (actor.movement) {
    const movement = actor.movement;
    movement.progress += deltaMs / movement.durationMs;
    if (movement.progress < 1) return;
    actor.scene = movement.to.scene;
    actor.x = movement.to.x;
    actor.y = movement.to.y;
    actor.movement = undefined;
    drainCompletedOutdoorTile(state, actor, movement);
    if (actor.presence === "entering") {
      actor.presence = "active";
      actor.activity = "arrived in Springleaf District";
      actor.bubble = { text: "Made it! What's on the board?", untilMs: state.elapsedMs + 3_500 };
      actor.routineDueMs = state.elapsedMs + 900 + random(state) * 1_800;
      addActivity(state, "routine", `${actor.name} entered the guild grounds.`, actor.id);
    } else if (actor.presence === "exiting" && actor.departure?.phase === "crossing") {
      actor.presence = "absent";
      actor.departure = undefined;
      actor.activity = "away from the guild";
      actor.intent = undefined;
      actor.bubble = undefined;
      actor.emote = undefined;
      addActivity(state, "routine", `${actor.name} crossed the boundary and left town.`, actor.id);
      return;
    } else {
      traversePortalAtCurrentPosition(state, actor);
    }
  }
  if (actor.presence === "exiting") {
    updateDeparture(state, actor);
    return;
  }
  if (actor.presence === "entering") return;
  if (traversePortalAtCurrentPosition(state, actor)) return;
  if (actor.id === "player") return;
  if (actor.tasks.length === 0) ensureRelativeControllerTask(state, actor);
  const task = actor.tasks[0];
  if (!task) {
    if (actor.activeOrderId !== undefined) return;
    if (pendingDecisionCallId(state, actor.id as ResidentId) !== undefined) return;
    if (!state.agentsOnline && state.elapsedMs >= actor.routineDueMs) {
      scheduleRoutine(state, actor as WorldActor & { id: ResidentId });
    }
    return;
  }
  if (playerOrderGoalStillNeeded(state, actor, task)) {
    actor.activity = "holding a clear approach for Scout's group order";
    return;
  }
  if (task.action.kind === "move") {
    walkTowardTask(state, actor, task, task.action.target);
    return;
  }
  if (task.action.kind === "move_relative") {
    walkTowardRelativeTask(state, actor, task, task.action);
    return;
  }
  if (task.action.kind === "maintain_relative") {
    actor.tasks.shift();
    actor.relativeConstraint = Object.freeze({
      action: task.action,
      requestId: task.requestId,
    });
    ensureRelativeControllerTask(state, actor);
    return;
  }
  if (task.action.kind === "random_choice") {
    actor.tasks.shift();
    const choseTrue = random(state) * 100 < task.action.chance_percent;
    const label = choseTrue ? task.action.true_label : task.action.false_label;
    const branch = choseTrue ? task.action.if_true : task.action.if_false;
    actor.tasks.unshift(...tasksFor(branch, task.origin, task.requestId));
    actor.bubble = { text: label, untilMs: state.elapsedMs + 2_200 };
    actor.emote = { icon: "?", untilMs: state.elapsedMs + 900 };
    actor.activity = `random choice: ${label}`;
    addActivity(state, task.origin, `${actor.name}'s random choice was ${label}.`, actor.id);
    return;
  }
  if (task.action.kind === "interact") {
    task.goal ??= targetPosition(state, actor, task.action.target);
    if (positionDistance(actorWorldPosition(actor), task.goal) > 1.6) {
      walkTowardTask(state, actor, task, task.action.target, false);
      return;
    }
    actor.tasks.shift();
    const succeeded = resolveInteraction(
      state,
      actor,
      task.action.target,
      task.action.action,
      task.origin,
    );
    if (task.orderId !== undefined) {
      finishPlayerOrderTask(
        state,
        actor,
        task,
        task.action.target,
        succeeded ? "completed" : "interaction-failed",
      );
    }
    return;
  }
  if (task.action.kind === "say") {
    actor.tasks.shift();
    actor.bubble = {
      text: task.action.text,
      untilMs: state.elapsedMs + bubbleDuration(task.action.text),
    };
    actor.activity = `said “${task.action.text}”`;
    actor.social = clamp(actor.social + 2);
    addActivity(
      state,
      task.origin,
      `${actor.name}: “${task.action.text}”`,
      actor.id,
      [actor.id],
    );
    return;
  }
  if (task.action.kind === "emote") {
    actor.tasks.shift();
    actor.emote = { icon: task.action.icon, untilMs: state.elapsedMs + 1_650 };
    actor.activity = emoteActivity(task.action.icon);
    return;
  }
}

function playerOrderGoalStillNeeded(
  state: WorldState,
  actor: WorldActor,
  task: WorldTask,
): boolean {
  if (task.orderId === undefined || task.goal === undefined) return false;
  const order = state.orders.find(({ id }) => id === task.orderId);
  const ownIndex = order?.assignments.findIndex(({ actorId }) => actorId === actor.id) ?? -1;
  const goal = task.goal;
  return ACTOR_IDS.some((otherId) => {
    if (otherId === actor.id) return false;
    const other = state.actors[otherId];
    const otherTask = other.tasks[0];
    if (
      otherTask?.orderId !== task.orderId
      || otherTask.path?.some((step) => samePosition(step, goal)) !== true
    ) return false;
    const reciprocal = otherTask.goal !== undefined
      && (
        samePosition(actorWorldPosition(actor), otherTask.goal)
        || task.path?.some((step) => samePosition(step, otherTask.goal as WorldPosition)) === true
      );
    if (!reciprocal) return true;
    const otherIndex = order?.assignments.findIndex(({ actorId }) => actorId === otherId) ?? -1;
    return otherIndex >= 0 && (ownIndex < 0 || otherIndex < ownIndex);
  });
}

function updateDeparture(state: WorldState, actor: WorldActor): void {
  const departure = actor.departure;
  if (!departure || actor.movement) return;
  if (departure.phase === "crossing") {
    actor.movement = {
      from: actorWorldPosition(actor),
      to: departure.outside,
      progress: 0,
      durationMs: 720,
    };
    return;
  }
  departure.path ??= findWorldRoute(actorWorldPosition(actor), departure.inside)
    .map((position) => ({ ...position }));
  const next = departure.path[0];
  if (next) {
    const from = actorWorldPosition(actor);
    if (!worldStepAvailable(state, actor, next)) {
      const rerouted = routeAroundActors(state, actor, departure.inside);
      if (rerouted) departure.path = rerouted;
      actor.activity = "waiting for a clear route out of town";
      return;
    }
    if (next.scene !== actor.scene) {
      if (!traverseDeclaredPortal(state, actor, next)) departure.path = [];
      return;
    }
    departure.path.shift();
    actor.direction = directionBetween(from, next);
    actor.movement = { from, to: next, progress: 0, durationMs: tileDuration(actor, 175) };
    return;
  }
  if (!samePosition(actorWorldPosition(actor), departure.inside)) {
    actor.activity = "could not find a safe route out of town";
    return;
  }
  departure.phase = "crossing";
  actor.direction = directionBetween(departure.inside, departure.outside);
  actor.movement = {
    from: actorWorldPosition(actor),
    to: departure.outside,
    progress: 0,
    durationMs: 720,
  };
}

function traversePortalAtCurrentPosition(state: WorldState, actor: WorldActor): boolean {
  const destination = portalDestinationAt(actorWorldPosition(actor));
  if (destination === undefined) return false;
  if (actor.id !== "player") {
    const expected = actor.tasks[0]?.path?.[0] ?? actor.departure?.path?.[0];
    if (expected === undefined || !samePosition(expected, destination)) return false;
  }
  return traverseDeclaredPortal(state, actor, destination);
}

function traverseDeclaredPortal(
  state: WorldState,
  actor: WorldActor,
  destination: WorldPosition,
): boolean {
  const declared = portalDestinationAt(actorWorldPosition(actor));
  if (
    declared === undefined
    || !samePosition(declared, destination)
    || !worldStepAvailable(state, actor, destination)
  ) return false;
  actor.scene = destination.scene;
  actor.x = destination.x;
  actor.y = destination.y;
  trimTraversedStep(actor.tasks[0]?.path, destination);
  trimTraversedStep(actor.departure?.path, destination);
  actor.activity = `entered ${sceneLabel(destination.scene)}`;
  addActivity(state, actor.lastOrigin, `${actor.name} entered ${sceneLabel(destination.scene)}.`, actor.id);
  fenceActorDecision(state, actor);
  return true;
}

function trimTraversedStep(path: WorldPosition[] | undefined, destination: WorldPosition): void {
  if (path?.[0] && samePosition(path[0], destination)) path.shift();
}

function drainCompletedOutdoorTile(
  state: WorldState,
  actor: WorldActor,
  movement: WorldMovement,
): void {
  if (
    movement.from.scene !== "town"
    || movement.to.scene !== "town"
    || Math.abs(movement.from.x - movement.to.x) + Math.abs(movement.from.y - movement.to.y) !== 1
  ) return;
  actor.energy = clamp(actor.energy - OUTDOOR_TILE_ENERGY_COST);
  fenceActorDecision(state, actor);
}

function tileDuration(actor: WorldActor, baseMs: number): number {
  if (actor.energy <= 15) return Math.round(baseMs * 1.75);
  if (actor.energy <= 35) return Math.round(baseMs * 1.35);
  return baseMs;
}

function fenceActorDecision(state: WorldState, actor: WorldActor): void {
  if (actor.id !== "player" && isResidentId(actor.id)) {
    state.decisionVersions[actor.id] += 1;
  }
}

function fenceAllResidentDecisions(state: WorldState): void {
  for (const id of RESIDENT_IDS) state.decisionVersions[id] += 1;
}

function scheduleRoutine(state: WorldState, actor: WorldActor & { id: ResidentId }): void {
  const special = specialRoutineScripts[actor.id];
  const specialSelection = special?.[actor.routineIndex % special.length];
  const selected = specialSelection && planTargetsPresent(state, specialSelection.steps)
    ? specialSelection
    : routineFor(actor, actor.routineIndex);
  actor.routineIndex += 1;
  actor.routineDueMs = state.elapsedMs + 7_000 + random(state) * 5_000;
  actor.tasks.push(...tasksFor(
    selected.steps,
    "routine",
    `routine-${actor.id}-${actor.routineIndex}`,
  ));
  actor.activity = selected.summary;
  actor.intent = selected.summary;
  actor.lastOrigin = "routine";
}

function routineFor(actor: WorldActor & { id: ResidentId }, index: number): RoutineScript {
  const guestRoutes = [
    ["plaza", "mission_board", "meadow"],
    ["shop", "orchard", "guild"],
    ["bridge", "pond", "plaza"],
    ["dungeon_gate", "meadow", "shop"],
  ] as const satisfies readonly (readonly WorldTarget[])[];
  const route = routineRoutes[actor.id]
    ?? guestRoutes[stableHash(actor.id) % guestRoutes.length]
    ?? guestRoutes[0];
  const target = route[index % route.length] ?? "plaza";
  const lines = routineLines[actor.id] ?? [
    `${actor.name} reporting in!`,
    "This guild is busier every minute.",
  ];
  const line = lines[index % lines.length] ?? "Guild round complete.";
  const interaction: WorldInteraction = target === "orchard"
    ? "gather"
    : target === "pond" || target === "bridge"
      ? "inspect"
      : target === "meadow" || target === "dungeon_gate"
        ? "train"
        : target === "guild"
          ? "greet"
          : "inspect";
  const flourish: WorldAction = index % 2 === 0
    ? { kind: "say", text: line }
    : { kind: "emote", icon: index % 3 === 0 ? "music" : "spark" };
  return Object.freeze({
    summary: `makes a ${target.replaceAll("_", " ")} round`,
    steps: Object.freeze([
      { kind: "move", target },
      { kind: "interact", target, action: interaction },
      flourish,
    ] satisfies WorldAction[]),
  });
}

function tasksFor(
  steps: readonly WorldAction[],
  origin: WorldTaskOrigin,
  requestId: string,
): WorldTask[] {
  return steps.map((action) => ({ action, origin, requestId }));
}

function ensureRelativeControllerTask(state: WorldState, actor: WorldActor): void {
  const constraint = actor.relativeConstraint;
  if (!constraint || actor.id === "player" || actor.tasks.length > 0 || actor.movement) return;
  const anchor = state.actors[constraint.action.anchor];
  if (
    anchor.id === actor.id
    || (anchor.id !== "player" && anchor.presence !== "active")
  ) {
    actor.relativeConstraint = undefined;
    actor.intent = undefined;
    actor.activity = anchor.id === actor.id
      ? "could not maintain a position relative to themself"
      : `stopped following ${anchor.name}; they are not on the map`;
    return;
  }
  const desired = relativeDesiredPosition(actorWorldPosition(anchor), constraint.action);
  const errorPixels = positionDistance(actorWorldPosition(actor), desired) * WORLD_TILE_SIZE;
  const label = relativeMoveLabel(state, constraint.action);
  actor.intent = `maintain ${label} within ${constraint.action.tolerance_pixels}px`;
  if (
    actor.scene === desired.scene
    && errorPixels <= constraint.action.tolerance_pixels
  ) {
    actor.activity = `holding ${label}`;
    return;
  }
  actor.tasks.push({
    action: Object.freeze({
      kind: "move_relative",
      anchor: constraint.action.anchor,
      dx_pixels: constraint.action.dx_pixels,
      dy_pixels: constraint.action.dy_pixels,
    }),
    origin: "nanocodex",
    requestId: constraint.requestId,
  });
  actor.activity = `correcting to maintain ${label}`;
}

function maintainedRelativeCycle(
  state: WorldState,
  followerId: ResidentId,
  anchorId: ActorId,
): boolean {
  const visited = new Set<ActorId>([followerId]);
  let current: ActorId | undefined = anchorId;
  while (current !== undefined) {
    if (visited.has(current)) return true;
    visited.add(current);
    current = state.actors[current].relativeConstraint?.action.anchor;
  }
  return false;
}

function walkTowardRelativeTask(
  state: WorldState,
  actor: WorldActor,
  task: WorldTask,
  action: Extract<WorldPrimitiveAction, { kind: "move_relative" }>,
): void {
  const anchor = state.actors[action.anchor];
  const label = relativeMoveLabel(state, action);
  if (anchor.id !== "player" && anchor.presence !== "active") {
    actor.tasks.shift();
    actor.activity = `could not move ${label}; ${anchor.name} is not on the map`;
    actor.intent = undefined;
    return;
  }
  if (!task.goal) {
    task.goal = safeRelativeGoal(state, actor, actorWorldPosition(anchor), action);
    if (!task.goal) {
      actor.tasks.shift();
      actor.activity = `could not find a safe tile ${label}`;
      actor.intent = undefined;
      addActivity(state, task.origin, `${actor.name} could not find a safe relative destination.`, actor.id);
      return;
    }
  }
  task.path ??= findWorldRoute(
    actorWorldPosition(actor),
    task.goal,
    dynamicBlocker(state, actor),
  )
    .map((position) => ({ ...position }));
  const next = task.path[0];
  if (!next) {
    const reached = samePosition(actorWorldPosition(actor), task.goal);
    actor.tasks.shift();
    actor.activity = reached ? `arrived ${label}` : `could not reach ${label}`;
    if (reached) {
      addActivity(state, task.origin, `${actor.name} moved ${label}.`, actor.id);
    } else {
      actor.intent = undefined;
    }
    return;
  }
  const from = actorWorldPosition(actor);
  if (!worldStepAvailable(state, actor, next)) {
    task.blockedSinceMs ??= state.elapsedMs;
    if (state.elapsedMs >= (task.rerouteAtMs ?? 0)) {
      const rerouted = routeAroundActors(state, actor, task.goal);
      if (rerouted) task.path = rerouted;
      task.rerouteAtMs = state.elapsedMs + 2_000;
    }
    if (state.elapsedMs - task.blockedSinceMs >= 10_000) {
      actor.tasks.shift();
      actor.activity = `could not find a clear path ${label}`;
      actor.intent = undefined;
      addActivity(state, task.origin, `${actor.name} gave up a blocked relative route.`, actor.id);
      return;
    }
    actor.activity = `waiting for a clear path ${label}`;
    return;
  }
  task.rerouteAtMs = undefined;
  task.blockedSinceMs = undefined;
  if (next.scene !== actor.scene) {
    if (!traverseDeclaredPortal(state, actor, next)) task.path = [];
    return;
  }
  task.path.shift();
  actor.direction = directionBetween(from, next);
  actor.movement = { from, to: next, progress: 0, durationMs: tileDuration(actor, 205) };
  actor.activity = `moving ${label}`;
}

function safeRelativeGoal(
  state: WorldState,
  actor: WorldActor,
  anchor: WorldPosition,
  action: Readonly<{ dx_pixels: number; dy_pixels: number }>,
): WorldPosition | undefined {
  const desired = relativeDesiredPosition(anchor, action);
  for (let radius = 0; radius <= 6; radius += 1) {
    for (let yOffset = -radius; yOffset <= radius; yOffset += 1) {
      for (let xOffset = -radius; xOffset <= radius; xOffset += 1) {
        if (Math.abs(xOffset) + Math.abs(yOffset) !== radius) continue;
        const candidate = Object.freeze({
          scene: desired.scene,
          x: desired.x + xOffset,
          y: desired.y + yOffset,
        });
        if (
          isWorldPositionBlocked(candidate)
          || portalDestinationAt(candidate) !== undefined
          || positionClaimedByOther(state, actor.id, candidate)
        ) continue;
        const start = actorWorldPosition(actor);
        const path = findWorldRoute(start, candidate, dynamicBlocker(state, actor));
        if (path.length > 0 || samePosition(start, candidate)) return candidate;
      }
    }
  }
  return undefined;
}

function relativeDesiredPosition(
  anchor: WorldPosition,
  action: Readonly<{ dx_pixels: number; dy_pixels: number }>,
): WorldPosition {
  return Object.freeze({
    scene: anchor.scene,
    x: anchor.x + pixelOffsetToTiles(action.dx_pixels),
    y: anchor.y + pixelOffsetToTiles(action.dy_pixels),
  });
}

function pixelOffsetToTiles(pixels: number): number {
  if (pixels === 0) return 0;
  return Math.sign(pixels) * Math.max(1, Math.round(Math.abs(pixels) / WORLD_TILE_SIZE));
}

function relativeMoveLabel(
  state: WorldState,
  action: Readonly<{ anchor: ActorId; dx_pixels: number; dy_pixels: number }>,
): string {
  const directions = [
    action.dx_pixels > 0 ? `${action.dx_pixels}px right of` : undefined,
    action.dx_pixels < 0 ? `${Math.abs(action.dx_pixels)}px left of` : undefined,
    action.dy_pixels > 0 ? `${action.dy_pixels}px below` : undefined,
    action.dy_pixels < 0 ? `${Math.abs(action.dy_pixels)}px above` : undefined,
  ].filter((value): value is string => value !== undefined);
  return `${directions.join(" and ")} ${state.actors[action.anchor].name}`;
}

function walkTowardTask(
  state: WorldState,
  actor: WorldActor,
  task: WorldTask,
  target: WorldTarget,
  finishAtDestination = true,
): void {
  task.goal ??= targetPosition(state, actor, target);
  if (!task.path) {
    task.path = findWorldRoute(actorWorldPosition(actor), task.goal)
      .map((position) => ({ ...position }));
  }
  const next = task.path[0];
  if (!next) {
    if (finishAtDestination && task.orderId !== undefined && task.goal) {
      actor.tasks.shift();
      finishPlayerOrderTask(
        state,
        actor,
        task,
        target,
        samePosition(actorWorldPosition(actor), task.goal) ? "completed" : "unreachable",
      );
      return;
    }
    const reached = positionDistance(actorWorldPosition(actor), task.goal) <= (finishAtDestination ? 0 : 1.6);
    if (!reached && task.orderId !== undefined) {
      actor.tasks.shift();
      finishPlayerOrderTask(state, actor, task, target, "unreachable");
      return;
    }
    if (finishAtDestination || !reached) actor.tasks.shift();
    actor.activity = reached
      ? `arrived at ${targetLabel(state, target)}`
      : `could not find a route to ${targetLabel(state, target)}`;
    return;
  }
  if (task.orderId !== undefined) markPlayerOrderMoving(state, actor, task.orderId);
  const from = actorWorldPosition(actor);
  if (!worldStepAvailable(state, actor, next)) {
    task.blockedSinceMs ??= state.elapsedMs;
    if (state.elapsedMs >= (task.rerouteAtMs ?? 0)) {
      const rerouted = routeAroundActors(state, actor, task.goal);
      if (rerouted) task.path = rerouted;
      task.rerouteAtMs = state.elapsedMs + 2_000;
    }
    if (task.orderId === undefined && state.elapsedMs - task.blockedSinceMs >= 10_000) {
      actor.tasks.shift();
      actor.activity = `could not find a clear route to ${targetLabel(state, target)}`;
      actor.intent = undefined;
      return;
    }
    actor.activity = `waiting for a clear route to ${targetLabel(state, target)}`;
    return;
  }
  task.rerouteAtMs = undefined;
  task.blockedSinceMs = undefined;
  if (next.scene !== actor.scene) {
    if (!traverseDeclaredPortal(state, actor, next)) task.path = [];
    return;
  }
  task.path.shift();
  actor.direction = directionBetween(from, next);
  actor.movement = { from, to: next, progress: 0, durationMs: tileDuration(actor, 205) };
  actor.activity = `heading to ${targetLabel(state, target)}`;
}

function routeAroundActors(
  state: WorldState,
  actor: WorldActor,
  goal: WorldPosition,
): WorldPosition[] | undefined {
  const route = findWorldRoute(
    actorWorldPosition(actor),
    goal,
    dynamicBlocker(state, actor),
  ).map((position) => ({ ...position }));
  return route.length > 0 ? route : undefined;
}

function dynamicBlocker(state: WorldState, actor: WorldActor) {
  const claims = new Set<string>();
  for (const otherId of ACTOR_IDS) {
    if (otherId === actor.id) continue;
    const other = state.actors[otherId];
    if (other.presence !== "active" && otherId !== "player") continue;
    claims.add(worldPositionKey(actorWorldPosition(other)));
    if (other.movement !== undefined) claims.add(worldPositionKey(other.movement.to));
  }
  return (position: WorldPosition): boolean => claims.has(worldPositionKey(position));
}

function worldPositionKey(position: WorldPosition): string {
  return `${position.scene}:${position.x},${position.y}`;
}

function markPlayerOrderMoving(state: WorldState, actor: WorldActor, orderId: number): void {
  const assignment = state.orders
    .find(({ id }) => id === orderId)
    ?.assignments.find(({ actorId }) => actorId === actor.id);
  if (!assignment || assignment.status !== "assigned") return;
  assignment.status = "moving";
  assignment.startedAtMs = state.elapsedMs;
}

function finishPlayerOrderTask(
  state: WorldState,
  actor: WorldActor,
  task: WorldTask,
  target: WorldTarget,
  result: "completed" | "unreachable" | "interaction-failed",
): void {
  const order = state.orders.find(({ id }) => id === task.orderId);
  const assignment = order?.assignments.find(({ actorId }) => actorId === actor.id);
  actor.lastOrigin = "player";
  if (!order || !assignment || (assignment.status !== "assigned" && assignment.status !== "moving")) {
    actor.activeOrderId = undefined;
    actor.activity = result === "completed"
      ? `arrived at ${targetLabel(state, target)}`
      : `lost Scout's route to ${targetLabel(state, target)}`;
    return;
  }
  assignment.completedAtMs = state.elapsedMs;
  actor.routineDueMs = state.elapsedMs + 9_000;
  if (result === "completed") {
    assignment.status = "completed";
    const action = task.action.kind === "interact"
      ? `${task.action.action} at`
      : "arrive at";
    actor.activity = `completed Scout's order to ${action} ${targetLabel(state, target)}`;
    actor.intent = actor.activity;
    addActivity(state, "player", `${actor.name} completed Scout's order ${order.id} at ${targetLabel(state, target)}.`, actor.id);
  } else {
    assignment.status = "rejected";
    assignment.reason = result;
    if (result === "unreachable") {
      actor.activity = `could not complete Scout's route to ${targetLabel(state, target)}`;
    }
    actor.intent = undefined;
  }
  maybeEmitOrderCompletion(state, order);
  pruneWorldOrders(state);
}

function resolveInteraction(
  state: WorldState,
  actor: WorldActor,
  target: WorldTarget,
  interaction: WorldInteraction,
  origin: WorldTaskOrigin,
): boolean {
  const label = targetLabel(state, target);
  actor.effect = { kind: interaction, untilMs: state.elapsedMs + 950 };
  let actorChanged = false;
  let suppliesChanged = false;
  let succeeded = true;
  let missionEligible = interaction === "inspect"
    || interaction === "splash"
    || (target === "dungeon_gate" && interaction === "train");

  if (distanceToTarget(state, actor, target) > 1.6) {
    actor.activity = `could not reach ${label} to ${interaction}`;
    missionEligible = false;
    succeeded = false;
  } else if (target === "orchard" && interaction === "gather") {
    if (actor.carrying !== undefined) {
      actor.activity = `could not gather at ${label} while carrying ${itemLabel(actor.carrying)}`;
      succeeded = false;
    } else if (state.supplies.orchardBerries <= 0) {
      actor.activity = `found no ripe sunberries at ${label}`;
      succeeded = false;
    } else {
      actor.carrying = "sunberry";
      state.supplies.orchardBerries -= 1;
      actor.activity = `gathered a sunberry at ${label}`;
      actorChanged = true;
      suppliesChanged = true;
      missionEligible = true;
    }
  } else if (target === "shop" && interaction === "offer") {
    if (actor.carrying !== "sunberry") {
      actor.activity = actor.carrying === undefined
        ? `had no sunberry to offer at ${label}`
        : `could not offer ${itemLabel(actor.carrying)} at ${label}`;
      succeeded = false;
    } else {
      actor.carrying = undefined;
      state.supplies.shopStock += 1;
      actor.activity = `stocked a sunberry at ${label}`;
      actorChanged = true;
      suppliesChanged = true;
    }
  } else if (target === "shop" && interaction === "gather") {
    if (actor.carrying !== undefined) {
      actor.activity = `could not take a supply pack while carrying ${itemLabel(actor.carrying)}`;
      succeeded = false;
    } else if (state.supplies.shopStock <= 0) {
      actor.activity = `found no supply packs in stock at ${label}`;
      succeeded = false;
    } else {
      actor.carrying = "supply_pack";
      state.supplies.shopStock -= 1;
      actor.activity = `picked up a supply pack at ${label}`;
      actorChanged = true;
      suppliesChanged = true;
    }
  } else if (target === "guild" && interaction === "offer") {
    if (actor.carrying !== "supply_pack") {
      actor.activity = actor.carrying === undefined
        ? `had no supply pack to offer at ${label}`
        : `could not deliver ${itemLabel(actor.carrying)} at ${label}`;
      succeeded = false;
    } else {
      actor.carrying = undefined;
      state.supplies.guildSupplies += 1;
      actor.activity = `delivered a supply pack to ${label}`;
      actorChanged = true;
      suppliesChanged = true;
    }
  } else if (target === "guild" && interaction === "rest") {
    const before = actor.energy;
    actor.energy = clamp(actor.energy + GUILD_REST_ENERGY);
    actor.activity = actor.energy > before
      ? `rested at ${label} and recovered ${Math.round(actor.energy - before)} energy`
      : `rested at ${label} with energy already full`;
    actorChanged = actor.energy !== before;
  } else if (target === "meadow" && interaction === "train") {
    if (actor.energy <= 0) {
      actor.activity = `was too exhausted to train at ${label}`;
      succeeded = false;
    } else {
      const spent = Math.min(TRAINING_ENERGY_COST, actor.energy);
      actor.energy = clamp(actor.energy - spent);
      state.supplies.trainingMarks += 1;
      actor.activity = `trained at ${label}, spent ${Math.round(spent)} energy, and earned a mark`;
      actorChanged = true;
      suppliesChanged = true;
    }
  } else if (interaction === "gather") {
    actor.activity = `found nothing useful to gather at ${label}`;
    missionEligible = false;
    succeeded = false;
  } else if (interaction === "offer") {
    actor.activity = `had no valid delivery to offer at ${label}`;
    missionEligible = false;
    succeeded = false;
  } else if (interaction === "rest") {
    actor.activity = `could not rest away from the guild at ${label}`;
    missionEligible = false;
    succeeded = false;
  } else if (interaction === "train" && target !== "dungeon_gate") {
    actor.activity = `could not train safely at ${label}`;
    missionEligible = false;
    succeeded = false;
  } else {
    actor.activity = interactionActivity(interaction, label);
    actor.curiosity = clamp(actor.curiosity + (interaction === "inspect" ? 3 : 1));
    actorChanged = true;
  }

  addActivity(state, origin, `${actor.name} ${actor.activity}.`, actor.id);
  const missionChanged = missionEligible && advanceMission(state, target, interaction);
  if (suppliesChanged || missionChanged) fenceAllResidentDecisions(state);
  else if (actorChanged) fenceActorDecision(state, actor);
  return succeeded;
}

function advanceMission(state: WorldState, target: WorldTarget, interaction: WorldInteraction): boolean {
  if (state.mission.stage === 0 && target === "orchard" && interaction === "gather") {
    state.mission = {
      stage: 1,
      title: "A silver sunberry",
      detail: "Moss found a berry dusted with the same silver metal as the bell. The trail points toward Bell Bridge.",
    };
    addActivity(state, "system", "Clue found: silver dust beneath the orchard trees.");
    return true;
  } else if (
    state.mission.stage === 1
    && (target === "bridge" || target === "pond")
    && (interaction === "inspect" || interaction === "splash")
  ) {
    state.mission = {
      stage: 2,
      title: "Echoes under Bell Bridge",
      detail: "Rill heard the bell in the eastbound current. Wet tracks continue through the Mystery Gate.",
    };
    addActivity(state, "system", "Clue found: wet tracks lead from the bridge to the Mystery Gate.");
    return true;
  } else if (
    state.mission.stage === 2
    && target === "dungeon_gate"
    && (interaction === "inspect" || interaction === "train")
  ) {
    state.mission = {
      stage: 3,
      title: "Expedition ready",
      detail: "The route is confirmed. The guild team is gathering supplies before entering the shifting dungeon.",
    };
    addActivity(state, "system", "Route confirmed: the next expedition begins at the Mystery Gate.");
    return true;
  }
  return false;
}

function addActivity(
  state: WorldState,
  origin: ActivityOrigin,
  text: string,
  actorId?: ActorId,
  audience?: readonly ResidentId[],
): void {
  state.activities.unshift(Object.freeze({
    id: state.nextActivityId++,
    ...(actorId === undefined ? {} : { actorId }),
    minuteOfDay: Math.floor(state.minuteOfDay),
    origin,
    text,
    ...(audience === undefined ? {} : { audience: Object.freeze([...audience]) }),
  }));
  if (state.activities.length > 48) state.activities.length = 48;
}

function addGuildMessage(
  state: WorldState,
  fromId: ActorId,
  text: string,
  origin: PlanOrigin | "player",
  toId?: ActorId,
  scope: "public" | "spatial" = "public",
  audience?: readonly ResidentId[],
): void {
  state.guildMessages.unshift(Object.freeze({
    id: state.nextGuildMessageId++,
    fromId,
    ...(toId === undefined ? {} : { toId }),
    minuteOfDay: Math.floor(state.minuteOfDay),
    origin,
    text,
    scope,
    ...(audience === undefined ? {} : { audience: Object.freeze([...audience]) }),
  }));
  if (state.guildMessages.length > WORLD_ROOM_RETENTION) {
    state.guildMessages.length = WORLD_ROOM_RETENTION;
  }
}

function boardMessageForObservation(
  state: WorldState,
  message: GuildMessage,
): WorldBoardMessage {
  const from = state.actors[message.fromId];
  const to = message.toId === undefined ? undefined : state.actors[message.toId];
  return Object.freeze({
    id: message.id,
    fromId: message.fromId,
    fromName: from.name,
    ...(message.toId === undefined || !to
      ? {}
      : { toId: message.toId, toName: to.name }),
    text: message.text,
    minuteOfDay: message.minuteOfDay,
    origin: message.origin,
    scope: message.scope,
  });
}

function decodeSavedActivities(value: unknown): WorldActivity[] {
  if (!Array.isArray(value)) return [];
  const seen = new Set<number>();
  return value.slice(0, 48).flatMap((entry): WorldActivity[] => {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) return [];
    const activity = entry as Record<string, unknown>;
    if (
      !Number.isSafeInteger(activity.id)
      || (activity.id as number) < 1
      || seen.has(activity.id as number)
      || (activity.actorId !== undefined
        && (typeof activity.actorId !== "string" || !isActorId(activity.actorId)))
      || !finiteNumber(activity.minuteOfDay, 0, 24 * 60)
      || (activity.origin !== "nanocodex"
        && activity.origin !== "routine"
        && activity.origin !== "player"
        && activity.origin !== "system")
      || typeof activity.text !== "string"
    ) return [];
    const text = activity.text
      .replace(/[\u0000-\u001f\u007f]/g, " ")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 240);
    if (!text) return [];
    const audience = Array.isArray(activity.audience)
      ? activity.audience.filter((id): id is ResidentId => isResidentId(id))
      : undefined;
    seen.add(activity.id as number);
    return [Object.freeze({
      id: activity.id as number,
      ...(activity.actorId === undefined ? {} : { actorId: activity.actorId }),
      minuteOfDay: activity.minuteOfDay,
      origin: activity.origin,
      text,
      ...(audience === undefined ? {} : { audience: Object.freeze(audience) }),
    })];
  });
}

function restoreSavedState(state: WorldState, saved: string): void {
  try {
    const value = JSON.parse(saved) as Record<string, unknown>;
    if (value.version !== 3) return;
    if (finiteNumber(value.elapsedMs, 0, Number.MAX_SAFE_INTEGER)) {
      state.elapsedMs = value.elapsedMs;
      state.weatherDueMs = state.elapsedMs + 58_000;
      state.orchardRestockDueMs = state.elapsedMs + ORCHARD_RESTOCK_INTERVAL_MS;
    }
    if (finiteNumber(value.minuteOfDay, 0, 24 * 60)) state.minuteOfDay = value.minuteOfDay;
    if (value.weather === "clear" || value.weather === "drizzle") state.weather = value.weather;
    if (finiteNumber(value.weatherDueMs, state.elapsedMs, state.elapsedMs + 100_000)) {
      state.weatherDueMs = value.weatherDueMs;
    }
    if (Number.isInteger(value.missionStage)) {
      const stage = Math.max(0, Math.min(3, value.missionStage as number));
      state.mission = missionForStage(stage);
    }
    if (Number.isInteger(value.rng)) state.rng = value.rng as number;
    if (Number.isInteger(value.agentDecisions) && (value.agentDecisions as number) >= 0) {
      state.agentDecisions = value.agentDecisions as number;
    }
    if (Number.isInteger(value.populationTarget)) {
      state.populationTarget = Math.max(0, Math.min(MAX_RESIDENT_COUNT, value.populationTarget as number));
    }
    const supplies = decodeSupplies(value.supplies);
    if (supplies) state.supplies = supplies;
    if (
      finiteNumber(
        value.orchardRestockDueMs,
        state.elapsedMs,
        state.elapsedMs + ORCHARD_RESTOCK_INTERVAL_MS,
      )
    ) {
      state.orchardRestockDueMs = value.orchardRestockDueMs;
    }
    const activities = decodeSavedActivities(value.activities);
    if (activities.length > 0) {
      state.activities = activities;
      state.nextActivityId = Math.max(...activities.map(({ id }) => id)) + 1;
    }
    if (Array.isArray(value.guildMessages)) {
      const messages = value.guildMessages
        .slice(0, WORLD_ROOM_RETENTION)
        .flatMap((entry): GuildMessage[] => {
          if (!entry || typeof entry !== "object") return [];
          const message = entry as Record<string, unknown>;
          if (
            !Number.isSafeInteger(message.id)
            || (message.id as number) < 1
            || typeof message.fromId !== "string"
            || !isActorId(message.fromId)
            || (message.toId !== undefined
              && (typeof message.toId !== "string" || !isActorId(message.toId)))
            || !finiteNumber(message.minuteOfDay, 0, 24 * 60)
            || (message.origin !== "nanocodex"
              && message.origin !== "routine"
              && message.origin !== "player")
            || (message.scope !== undefined
              && message.scope !== "public"
              && message.scope !== "spatial")
            || typeof message.text !== "string"
          ) return [];
          const text = sanitizeDialogue(message.text);
          if (!text) return [];
          const audience = Array.isArray(message.audience)
            ? message.audience.filter((id): id is ResidentId => isResidentId(id))
            : undefined;
          return [Object.freeze({
            id: message.id as number,
            fromId: message.fromId,
            ...(message.toId === undefined ? {} : { toId: message.toId }),
            minuteOfDay: message.minuteOfDay,
            origin: message.origin,
            text,
            scope: message.scope === "spatial" ? "spatial" : "public",
            ...(audience === undefined ? {} : { audience: Object.freeze(audience) }),
          })];
        });
      if (messages.length > 0) {
        state.guildMessages = messages;
        state.nextGuildMessageId = Math.max(...messages.map(({ id }) => id)) + 1;
      }
    }
    if (value.actors && typeof value.actors === "object") {
      const actors = value.actors as Record<string, unknown>;
      for (const id of ACTOR_IDS) {
        const savedActor = actors[id];
        if (!savedActor || typeof savedActor !== "object") continue;
        const entry = savedActor as Record<string, unknown>;
        const actor = state.actors[id];
        if (id !== "player" && typeof entry.present === "boolean") {
          actor.presence = entry.present ? "active" : "absent";
        }
        const position = decodePosition(entry.position);
        if (position && !isWorldPositionBlocked(position)) {
          actor.scene = position.scene;
          actor.x = position.x;
          actor.y = position.y;
        }
        actor.carrying = isWorldItemKind(entry.carrying) ? entry.carrying : undefined;
        if (entry.direction === "up" || entry.direction === "down"
          || entry.direction === "left" || entry.direction === "right") {
          actor.direction = entry.direction;
        }
        if (finiteNumber(entry.energy, 0, 100)) actor.energy = entry.energy;
        if (finiteNumber(entry.curiosity, 0, 100)) actor.curiosity = entry.curiosity;
        if (finiteNumber(entry.social, 0, 100)) actor.social = entry.social;
        if (Number.isInteger(entry.routineIndex) && (entry.routineIndex as number) >= 0) {
          actor.routineIndex = entry.routineIndex as number;
        }
      }
    }
    if (value.residentMemories && typeof value.residentMemories === "object") {
      const memories = value.residentMemories as Record<string, unknown>;
      for (const id of RESIDENT_IDS) {
        if (!Object.hasOwn(memories, id)) continue;
        state.residentMemories[id] = copyResidentMemory(memories[id]);
      }
    }
  } catch {
    // Local saves are expendable; malformed state starts a fresh deterministic town.
  }
}

function missionForStage(stage: number): WorldMission {
  if (stage === 1) return {
    stage,
    title: "A silver sunberry",
    detail: "Moss found a berry dusted with the same silver metal as the bell. The trail points toward Bell Bridge.",
  };
  if (stage === 2) return {
    stage,
    title: "Echoes under Bell Bridge",
    detail: "Rill heard the bell in the eastbound current. Wet tracks continue through the Mystery Gate.",
  };
  if (stage === 3) return {
    stage,
    title: "Expedition ready",
    detail: "The route is confirmed. The guild team is gathering supplies before entering the shifting dungeon.",
  };
  return {
    stage: 0,
    title: "The bell beneath the water",
    detail: "The guild's silver rescue bell vanished before dawn. Search the orchard and Bell Bridge for clues.",
  };
}

function targetPosition(
  state: WorldState,
  actor: WorldActor,
  target: WorldTarget,
): WorldPosition {
  if (isActorId(target)) {
    const targetActor = state.actors[target];
    return arrivalPosition(actor, actorWorldPosition(targetActor), actorArrivalOffsets);
  }
  return arrivalPosition(actor, WORLD_POIS[target], arrivalOffsets);
}

const arrivalOffsets = [
  { x: 0, y: 0 },
  { x: -1, y: 0 },
  { x: 1, y: 0 },
  { x: 0, y: -1 },
  { x: 0, y: 1 },
  { x: -1, y: -1 },
  { x: 1, y: -1 },
  { x: -1, y: 1 },
  { x: 1, y: 1 },
] as const;

const actorArrivalOffsets = arrivalOffsets.slice(1);

function arrivalPosition(
  actor: WorldActor,
  center: WorldPosition,
  offsets: readonly Point[],
): WorldPosition {
  const start = stableHash(actor.id) % offsets.length;
  for (let index = 0; index < offsets.length; index += 1) {
    const offset = offsets[(start + index) % offsets.length];
    if (!offset) continue;
    const position = { scene: center.scene, x: center.x + offset.x, y: center.y + offset.y };
    if (!isWorldPositionBlocked(position)) return position;
  }
  return { ...center };
}

function distanceToTarget(state: WorldState, actor: WorldActor, target: WorldTarget): number {
  const position = isActorId(target)
    ? actorWorldPosition(state.actors[target])
    : WORLD_POIS[target];
  return positionDistance(actorWorldPosition(actor), position);
}

function targetLabel(state: WorldState, target: WorldTarget): string {
  return isActorId(target) ? state.actors[target].name : WORLD_POIS[target].label;
}

function locationFor(actor: WorldActor): string {
  const poi = nearestPoi(actor);
  return poi.distance <= 8 ? poi.label : sceneLabel(actor.scene);
}

function nearestPoi(actor: Pick<WorldActor, "scene" | "x" | "y">): {
  target: Exclude<WorldTarget, ActorId>;
  label: string;
  distance: number;
} {
  return (Object.entries(WORLD_POIS) as Array<[
    Exclude<WorldTarget, ActorId>,
    WorldPosition & { label: string },
  ]>).map(([target, point]) => ({
    target,
    label: point.label,
    distance: positionDistance(actor, point),
  })).sort((left, right) => left.distance - right.distance)[0]
    ?? { target: "plaza", label: sceneLabel(actor.scene), distance: Number.POSITIVE_INFINITY };
}

function nearestReducerInteraction(actor: WorldActor): Readonly<{
  target: "orchard" | "shop" | "guild" | "meadow";
  interaction: "gather" | "offer" | "rest" | "train";
}> | undefined {
  const candidates = (["orchard", "shop", "guild", "meadow"] as const)
    .map((target) => ({ target, distance: distanceToPoi(actor, target) }))
    .filter(({ distance }) => distance <= 1.8)
    .sort((left, right) => left.distance - right.distance);
  const nearest = candidates[0];
  if (!nearest) return undefined;
  if (nearest.target === "orchard") return { target: "orchard", interaction: "gather" };
  if (nearest.target === "shop") {
    return { target: "shop", interaction: actor.carrying === "sunberry" ? "offer" : "gather" };
  }
  if (nearest.target === "guild") {
    return { target: "guild", interaction: actor.carrying === "supply_pack" ? "offer" : "rest" };
  }
  return { target: "meadow", interaction: "train" };
}

function distanceToPoi(
  actor: WorldActor,
  target: "orchard" | "shop" | "guild" | "meadow",
): number {
  return positionDistance(actorWorldPosition(actor), WORLD_POIS[target]);
}

function directionOffset(direction: Direction): Point {
  if (direction === "up") return { x: 0, y: -1 };
  if (direction === "down") return { x: 0, y: 1 };
  if (direction === "left") return { x: -1, y: 0 };
  return { x: 1, y: 0 };
}

function directionBetween(from: Point, to: Point): Direction {
  if (to.x < from.x) return "left";
  if (to.x > from.x) return "right";
  if (to.y < from.y) return "up";
  return "down";
}

function isActorId(value: string): value is ActorId {
  return (ACTOR_IDS as readonly string[]).includes(value);
}

function pointDistance(left: Pick<Point, "x" | "y">, right: Pick<Point, "x" | "y">): number {
  return Math.hypot(left.x - right.x, left.y - right.y);
}

function actorDistance(left: WorldActor, right: WorldActor): number {
  if (left.scene !== right.scene) return Number.POSITIVE_INFINITY;
  return pointDistance(actorRenderPoint(left), actorRenderPoint(right));
}

function positionDistance(
  left: Pick<WorldPosition, "scene" | "x" | "y">,
  right: Pick<WorldPosition, "scene" | "x" | "y">,
): number {
  return left.scene === right.scene ? pointDistance(left, right) : Number.POSITIVE_INFINITY;
}

function positionKey(position: WorldPosition): string {
  return `${position.scene}:${position.x},${position.y}`;
}

function stableHash(value: string): number {
  let hash = 0;
  for (let index = 0; index < value.length; index += 1) {
    hash = (hash * 31 + value.charCodeAt(index)) >>> 0;
  }
  return hash;
}

function itemLabel(item: WorldItemKind): string {
  return item === "supply_pack" ? "a supply pack" : "a sunberry";
}

function isWorldSceneId(value: unknown): value is WorldSceneId {
  return typeof value === "string" && (WORLD_SCENE_IDS as readonly string[]).includes(value);
}

function isWorldItemKind(value: unknown): value is WorldItemKind {
  return typeof value === "string" && (WORLD_ITEM_KINDS as readonly string[]).includes(value);
}

function decodePosition(value: unknown): WorldPosition | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const position = value as Record<string, unknown>;
  if (
    !isWorldSceneId(position.scene)
    || !Number.isInteger(position.x)
    || !Number.isInteger(position.y)
  ) return undefined;
  return { scene: position.scene, x: position.x as number, y: position.y as number };
}

function decodeSupplies(value: unknown): MutableWorldSupplyState | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const supplies = value as Record<string, unknown>;
  if (
    !safeCount(supplies.orchardBerries, ORCHARD_CAPACITY)
    || !safeCount(supplies.shopStock)
    || !safeCount(supplies.guildSupplies)
    || !safeCount(supplies.trainingMarks)
  ) return undefined;
  return {
    orchardBerries: supplies.orchardBerries,
    shopStock: supplies.shopStock,
    guildSupplies: supplies.guildSupplies,
    trainingMarks: supplies.trainingMarks,
  };
}

function safeCount(value: unknown, maximum = Number.MAX_SAFE_INTEGER): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0 && (value as number) <= maximum;
}

function random(state: WorldState): number {
  let value = state.rng | 0;
  value ^= value << 13;
  value ^= value >>> 17;
  value ^= value << 5;
  state.rng = value >>> 0;
  return state.rng / 0x1_0000_0000;
}

function clamp(value: number): number {
  return Math.max(0, Math.min(100, value));
}

function bubbleDuration(text: string): number {
  return Math.max(2_500, Math.min(7_000, 1_800 + text.length * 64));
}

function easeInOut(value: number): number {
  return value * value * (3 - 2 * value);
}

function finiteNumber(value: unknown, min: number, max: number): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= min && value <= max;
}

function lowercaseSentence(value: string): string {
  return value ? `${value[0].toLowerCase()}${value.slice(1)}` : value;
}

function actionLabel(action: WorldAction): string {
  if (action.kind === "move") return `move to ${action.target.replaceAll("_", " ")}`;
  if (action.kind === "move_relative") {
    return `move ${action.dx_pixels}px x / ${action.dy_pixels}px y from ${action.anchor}`;
  }
  if (action.kind === "maintain_relative") {
    return `maintain ${action.dx_pixels}px x / ${action.dy_pixels}px y from ${action.anchor} within ${action.tolerance_pixels}px`;
  }
  if (action.kind === "random_choice") {
    return `choose between ${action.true_label} and ${action.false_label}`;
  }
  if (action.kind === "interact") {
    return `${action.action} at ${action.target.replaceAll("_", " ")}`;
  }
  if (action.kind === "say") {
    return `say “${action.text}”${action.to === undefined ? "" : ` to ${action.to}`}`;
  }
  if (action.kind === "emote") return `emote ${action.icon}`;
  throw new Error("unsupported World action");
}

function emoteActivity(icon: string): string {
  if (icon === "heart") return "looks delighted";
  if (icon === "music") return "hums the guild march";
  if (icon === "spark") return "notices a clue";
  if (icon === "sweat") return "looks uneasy";
  if (icon === "!") return "spots something important";
  return "thinks it over";
}

function interactionActivity(interaction: WorldInteraction, label: string): string {
  if (interaction === "inspect") return `inspects ${label}`;
  if (interaction === "gather") return `gathers supplies at ${label}`;
  if (interaction === "offer") return `offers a clue at ${label}`;
  if (interaction === "splash") return `tests the current at ${label}`;
  if (interaction === "rest") return `rests at ${label}`;
  if (interaction === "greet") return `greets the team at ${label}`;
  if (interaction === "post") return `posts an expedition at ${label}`;
  return `trains near ${label}`;
}
