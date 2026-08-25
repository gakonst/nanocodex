export const WORLD_PROTOCOL = "nanocodex.monster-world.v6" as const;

export const WORLD_SCENE_IDS = ["town", "guild_hall", "trail_shop"] as const;
export const WORLD_ITEM_KINDS = ["sunberry", "supply_pack"] as const;

export const NAMED_RESIDENT_IDS = [
  "cinder",
  "moss",
  "rill",
  "luma",
  "iris",
  "rook",
  "june",
  "pax",
  "ember",
  "fern",
  "brook",
  "twig",
  "pebble",
  "nova",
  "pip",
  "dune",
  "aria",
  "beck",
  "cyra",
  "dev",
  "esme",
  "finn",
  "grey",
  "hope",
] as const;

export const GUEST_AGENT_IDS = [
  "guest01",
  "guest02",
  "guest03",
  "guest04",
  "guest05",
  "guest06",
  "guest07",
  "guest08",
  "guest09",
  "guest10",
  "guest11",
  "guest12",
  "guest13",
  "guest14",
  "guest15",
  "guest16",
  "guest17",
  "guest18",
  "guest19",
  "guest20",
  "guest21",
  "guest22",
  "guest23",
  "guest24",
] as const;

export const RESIDENT_IDS = [...NAMED_RESIDENT_IDS, ...GUEST_AGENT_IDS] as const;
// Every resident owns one persistent child session in the World task tree.
export const ACTOR_IDS = ["player", ...RESIDENT_IDS] as const;
export const WORLD_TARGETS = [
  "guild",
  "mission_board",
  "plaza",
  "orchard",
  "pond",
  "shop",
  "meadow",
  "bridge",
  "dungeon_gate",
  ...ACTOR_IDS,
] as const;
export const WORLD_EMOTES = ["!", "?", "heart", "music", "spark", "sweat"] as const;
export const WORLD_INTERACTIONS = [
  "inspect",
  "gather",
  "offer",
  "splash",
  "rest",
  "greet",
  "post",
  "train",
] as const;
export const VOICE_LEVELS = ["whisper", "talk", "call", "shout"] as const;
export const VOICE_RADIUS = Object.freeze({
  whisper: 3.5,
  talk: 7,
  call: 12,
  shout: 20,
} satisfies Record<VoiceLevel, number>);

export type GuestAgentId = (typeof GUEST_AGENT_IDS)[number];
export type ResidentId = (typeof RESIDENT_IDS)[number];
export type ActorId = (typeof ACTOR_IDS)[number];
export type WorldSceneId = (typeof WORLD_SCENE_IDS)[number];
export type WorldItemKind = (typeof WORLD_ITEM_KINDS)[number];
export type WorldTarget = (typeof WORLD_TARGETS)[number];
export type WorldEmote = (typeof WORLD_EMOTES)[number];
export type WorldInteraction = (typeof WORLD_INTERACTIONS)[number];
export type VoiceLevel = (typeof VOICE_LEVELS)[number];
export type Direction = "up" | "down" | "left" | "right";
export type PlanOrigin = "nanocodex" | "routine";

export type WorldPosition = Readonly<{
  scene: WorldSceneId;
  x: number;
  y: number;
}>;

export type WorldSupplyState = Readonly<{
  orchardBerries: number;
  shopStock: number;
  guildSupplies: number;
  trainingMarks: number;
}>;

export type WorldPrimitiveAction =
  | Readonly<{ kind: "move"; target: WorldTarget }>
  | Readonly<{
      kind: "move_relative";
      anchor: ActorId;
      dx_pixels: number;
      dy_pixels: number;
    }>
  | Readonly<{
      kind: "maintain_relative";
      anchor: ActorId;
      dx_pixels: number;
      dy_pixels: number;
      tolerance_pixels: number;
    }>
  | Readonly<{ kind: "say"; text: string; to?: ActorId }>
  | Readonly<{ kind: "emote"; icon: WorldEmote }>
  | Readonly<{
      kind: "interact";
      target: WorldTarget;
      action: WorldInteraction;
    }>;

export type WorldAction = WorldPrimitiveAction | Readonly<{
  kind: "random_choice";
  chance_percent: number;
  true_label: string;
  false_label: string;
  if_true: readonly WorldPrimitiveAction[];
  if_false: readonly WorldPrimitiveAction[];
}>;

export type WorldPlan = Readonly<{
  protocol: typeof WORLD_PROTOCOL;
  requestId: string;
  agentId: ResidentId;
  stateVersion: number;
  heardCallId?: number;
  summary: string;
  steps: readonly WorldAction[];
  origin: PlanOrigin;
}>;

export type HeardGuildCall = Readonly<{
  id: number;
  text: string;
  voice: VoiceLevel;
  distance?: number;
  radius: number;
  guildWide: boolean;
  coListeners: readonly ResidentId[];
  requestedTarget?: WorldTarget;
}>;

export type WorldPlayerOrder = Readonly<{
  id: number;
  text: string;
  coListeners: readonly ResidentId[];
  requestedTarget?: WorldTarget;
}>;

export type WorldBoardMessage = Readonly<{
  id: number;
  fromId: ActorId;
  fromName: string;
  toId?: ActorId;
  toName?: string;
  text: string;
  minuteOfDay: number;
  origin: PlanOrigin | "player";
  scope: "public" | "spatial";
}>;

export type WorldObservation = Readonly<{
  stateVersion: number;
  minuteOfDay: number;
  weather: "clear" | "drizzle";
  self: Readonly<{
    id: ResidentId;
    name: string;
    role: string;
    kind: "monster" | "human";
    scene: WorldSceneId;
    x: number;
    y: number;
    direction: Direction;
    movingTo?: WorldPosition;
    location: string;
    energy: number;
    curiosity: number;
    social: number;
    carrying?: WorldItemKind;
  }>;
  nearby: readonly Readonly<{
    id: ActorId;
    name: string;
    kind: "player" | "monster" | "human";
    scene: WorldSceneId;
    x: number;
    y: number;
    relativeX: number;
    relativeY: number;
    distance: number;
    direction: Direction;
    movingTo?: WorldPosition;
    activity: string;
    intent?: string;
  }>[];
  roster: readonly Readonly<{
    id: ActorId;
    name: string;
    kind: "player" | "monster" | "human";
    scene: WorldSceneId;
    x: number;
    y: number;
    direction: Direction;
    movingTo?: WorldPosition;
    location: string;
    activity: string;
  }>[];
  playerOrder?: WorldPlayerOrder;
  guildCall?: HeardGuildCall;
  guildBoard: readonly WorldBoardMessage[];
  recentEvents: readonly string[];
  availableTargets: readonly WorldTarget[];
  supplies: WorldSupplyState;
}>;

export type WorldUsage = Readonly<{
  modelTurns: number;
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
  estimatedUsd?: string;
}>;

export type WorldResidentMemory = Readonly<{
  summary: string;
  goals: readonly string[];
  relationships: readonly string[];
  recentDecisions: readonly string[];
  lastBoardMessageId: number;
}>;

export const EMPTY_WORLD_RESIDENT_MEMORY: WorldResidentMemory = Object.freeze({
  summary: "",
  goals: Object.freeze([]),
  relationships: Object.freeze([]),
  recentDecisions: Object.freeze([]),
  lastBoardMessageId: 0,
});

export function worldObservationCallId(observation: WorldObservation): number | undefined {
  return observation.playerOrder?.id
    ?? observation.guildCall?.id;
}

export type WorldActionOutcome = Readonly<{
  status: "completed" | "in_progress" | "blocked" | "rejected" | "superseded";
  action: WorldPrimitiveAction;
  detail: string;
}>;

export type WorldToolResult = Readonly<{
  worldRevision: number;
  outcome: WorldActionOutcome;
  self: WorldObservation["self"];
  nearby: WorldObservation["nearby"];
  roster: WorldObservation["roster"];
  playerOrder?: WorldPlayerOrder;
  guildCall?: HeardGuildCall;
  relevantEvents: readonly string[];
}>;

export type WorldResidentDecision = Readonly<{
  plan: WorldPlan;
  memory: WorldResidentMemory;
}>;

export type WorldFailureClass =
  | "transient"
  | "invalid"
  | "cancelled";

export type WorldAgentCommand =
  | Readonly<{
      protocol: typeof WORLD_PROTOCOL;
      type: "connect";
    }>
  | Readonly<{
      protocol: typeof WORLD_PROTOCOL;
      type: "call";
      requestId: string;
      agentId: ResidentId;
      residentIds: readonly ResidentId[];
      observation: WorldObservation;
    }>
  | Readonly<{
      protocol: typeof WORLD_PROTOCOL;
      type: "action_result";
      actionId: string;
      requestId: string;
      agentId: ResidentId;
      result: WorldToolResult;
    }>
  | Readonly<{
      protocol: typeof WORLD_PROTOCOL;
      type: "shutdown";
    }>;

export type WorldAgentMessage =
  | Readonly<{
      protocol: typeof WORLD_PROTOCOL;
      type: "status";
      status: "connecting" | "ready" | "stopped" | "error";
      message?: string;
    }>
  | Readonly<{
      protocol: typeof WORLD_PROTOCOL;
      type: "action";
      actionId: string;
      requestId: string;
      agentId: ResidentId;
      heardCallId?: number;
      action: WorldPrimitiveAction;
    }>
  | Readonly<{
      protocol: typeof WORLD_PROTOCOL;
      type: "settled";
      requestId: string;
      agentId: ResidentId;
      outcome: "completed" | "cancelled" | "failed";
      failure?: WorldFailureClass;
      message?: string;
      usage?: WorldUsage;
    }>;

export type ExpectedWorldDecision = Readonly<{
  requestId: string;
  agentId: ResidentId;
  stateVersion: number;
  memory?: WorldResidentMemory;
  heardCallId?: number;
  requestedTarget?: WorldTarget;
}>;

type JsonObject = Record<string, unknown>;

export function decodeStagedPlan(
  value: unknown,
  expected: ExpectedWorldDecision,
): WorldPlan {
  const input = object(value, "world action plan");
  const requestId = text(input.request_id, "request_id", 96);
  const agentId = text(input.agent_id, "agent_id", 24);
  const stateVersion = integer(input.state_version, "state_version", 0, Number.MAX_SAFE_INTEGER);
  if (requestId !== expected.requestId) throw new Error("request_id does not match the active turn");
  if (agentId !== expected.agentId) throw new Error("agent_id does not match this session");
  if (stateVersion !== expected.stateVersion) throw new Error("state_version is stale");
  const rawSteps = Array.isArray(input.steps) ? input.steps : undefined;
  if (!rawSteps || rawSteps.length < 1 || rawSteps.length > 6) {
    throw new Error("steps must contain between 1 and 6 actions");
  }
  const steps = rawSteps.map(decodeAction);
  if (expected.requestedTarget !== undefined && !steps.some((step) =>
    (step.kind === "move" || step.kind === "interact")
    && step.target === expected.requestedTarget
  )) {
    throw new Error(`this guild-call response must physically act at ${expected.requestedTarget}`);
  }
  return Object.freeze({
    protocol: WORLD_PROTOCOL,
    requestId,
    agentId: expected.agentId,
    stateVersion,
    ...(expected.heardCallId === undefined ? {} : { heardCallId: expected.heardCallId }),
    summary: text(input.summary, "summary", 80),
    steps: Object.freeze(steps),
    origin: "nanocodex",
  });
}

export function decodeWorldResidentMemory(value: unknown): WorldResidentMemory {
  const input = object(value, "resident memory");
  return Object.freeze({
    summary: optionalText(input.summary, "memory.summary", 320),
    goals: boundedTextList(input.goals, "memory.goals", 4, 120),
    relationships: boundedTextList(input.relationships, "memory.relationships", 8, 140),
    recentDecisions: boundedTextList(input.recent_decisions, "memory.recent_decisions", 6, 160),
    lastBoardMessageId: integer(
      input.last_board_message_id ?? 0,
      "memory.last_board_message_id",
      0,
      Number.MAX_SAFE_INTEGER,
    ),
  });
}

export function decodeResidentDecision(
  value: unknown,
  expected: ExpectedWorldDecision,
): WorldResidentDecision {
  const input = object(value, "resident world plan");
  const plan = decodeStagedPlan({
    request_id: expected.requestId,
    agent_id: expected.agentId,
    state_version: expected.stateVersion,
    summary: input.summary,
    steps: input.steps,
  }, expected);
  return Object.freeze({
    plan,
    memory: memoryAfterDecision(
      expected.memory ?? EMPTY_WORLD_RESIDENT_MEMORY,
      plan.summary,
      optionalText(input.memory_note, "memory_note", 160),
    ),
  });
}

function memoryAfterDecision(
  previous: WorldResidentMemory,
  planSummary: string,
  note: string,
): WorldResidentMemory {
  const remembered = note || planSummary;
  return Object.freeze({
    summary: note || previous.summary,
    goals: Object.freeze([...previous.goals]),
    relationships: Object.freeze([...previous.relationships]),
    recentDecisions: Object.freeze([
      remembered,
      ...previous.recentDecisions.filter((entry) => entry !== remembered),
    ].slice(0, 6)),
    lastBoardMessageId: previous.lastBoardMessageId,
  });
}

export function isWorldPlan(value: unknown): value is WorldPlan {
  if (!value || typeof value !== "object") return false;
  const plan = value as Partial<WorldPlan>;
  if (
    plan.protocol !== WORLD_PROTOCOL
    || typeof plan.requestId !== "string"
    || !isResidentId(plan.agentId)
    || !Number.isSafeInteger(plan.stateVersion)
    || (plan.heardCallId !== undefined && !Number.isSafeInteger(plan.heardCallId))
    || typeof plan.summary !== "string"
    || (plan.origin !== "nanocodex" && plan.origin !== "routine")
    || !Array.isArray(plan.steps)
    || plan.steps.length < 1
    || plan.steps.length > 6
    || !isDenseArray(plan.steps)
  ) return false;
  try {
    for (const step of plan.steps) decodeAction(step);
    return true;
  } catch {
    return false;
  }
}

export function isWorldAgentMessage(value: unknown): value is WorldAgentMessage {
  if (!value || typeof value !== "object") return false;
  const message = value as Partial<WorldAgentMessage>;
  if (message.protocol !== WORLD_PROTOCOL) return false;
  if (message.type === "status") {
    return message.status === "connecting"
      || message.status === "ready"
      || message.status === "stopped"
      || message.status === "error";
  }
  if (message.type === "action") {
    return isWorldIdentifier(message.actionId)
      && isWorldIdentifier(message.requestId)
      && isResidentId(message.agentId)
      && (message.heardCallId === undefined || Number.isSafeInteger(message.heardCallId))
      && isWorldPrimitiveAction(message.action);
  }
  if (message.type === "settled") {
    return typeof message.requestId === "string"
      && isResidentId(message.agentId)
      && (
        message.outcome === "completed"
        || message.outcome === "cancelled"
        || message.outcome === "failed"
      )
      && (message.failure === undefined || isWorldFailureClass(message.failure))
      && (message.usage === undefined || isWorldUsage(message.usage));
  }
  return false;
}

export function isWorldAgentCommand(value: unknown): value is WorldAgentCommand {
  if (!value || typeof value !== "object") return false;
  const command = value as Partial<WorldAgentCommand>;
  if (command.protocol !== WORLD_PROTOCOL) return false;
  if (command.type === "connect" || command.type === "shutdown") return true;
  if (command.type === "call") {
    return typeof command.requestId === "string"
      && command.requestId.length > 0
      && isResidentId(command.agentId)
      && isUniqueResidentList(command.residentIds)
      && command.residentIds.includes(command.agentId)
      && isWorldObservation(command.observation, command.agentId);
  }
  if (command.type === "action_result") {
    return isWorldIdentifier(command.actionId)
      && isWorldIdentifier(command.requestId)
      && isResidentId(command.agentId)
      && isWorldToolResult(command.result, command.agentId);
  }
  return false;
}

export function decodeWorldPrimitiveAction(value: unknown): WorldPrimitiveAction {
  const action = object(value, "action");
  return decodePrimitiveAction(action, text(action.kind, "action.kind", 24));
}

function isWorldPrimitiveAction(value: unknown): value is WorldPrimitiveAction {
  try {
    decodeWorldPrimitiveAction(value);
    return true;
  } catch {
    return false;
  }
}

function isWorldToolResult(value: unknown, agentId: ResidentId): value is WorldToolResult {
  if (!isJsonObject(value)) return false;
  const result = value as Partial<WorldToolResult>;
  if (
    !Number.isSafeInteger(result.worldRevision)
    || (result.worldRevision as number) < 0
    || !isJsonObject(result.outcome)
    || !isWorldObservationSelf(result.self, agentId)
    || !isDenseArrayOf(result.nearby, isWorldNearbyActor)
    || !isDenseArrayOf(result.roster, isWorldRosterActor)
    || (result.playerOrder !== undefined && !isWorldPlayerOrder(result.playerOrder))
    || (result.guildCall !== undefined && !isHeardGuildCall(result.guildCall))
    || !isDenseArrayOf(result.relevantEvents, isString)
  ) return false;
  const outcome = result.outcome as Partial<WorldActionOutcome>;
  return (
    outcome.status === "completed"
    || outcome.status === "in_progress"
    || outcome.status === "blocked"
    || outcome.status === "rejected"
    || outcome.status === "superseded"
  )
    && typeof outcome.detail === "string"
    && isWorldPrimitiveAction(outcome.action);
}

export function isResidentId(value: unknown): value is ResidentId {
  return typeof value === "string" && (RESIDENT_IDS as readonly string[]).includes(value);
}

export function sanitizeDialogue(value: string): string {
  return value.replace(/[\u0000-\u001f\u007f]/g, " ").replace(/\s+/g, " ").trim().slice(0, 140);
}

function isWorldUsage(value: unknown): value is WorldUsage {
  if (!value || typeof value !== "object") return false;
  const usage = value as Partial<WorldUsage>;
  return Number.isSafeInteger(usage.modelTurns)
    && (usage.modelTurns as number) >= 1
    && Number.isSafeInteger(usage.inputTokens)
    && (usage.inputTokens as number) >= 0
    && Number.isSafeInteger(usage.outputTokens)
    && (usage.outputTokens as number) >= 0
    && Number.isSafeInteger(usage.totalTokens)
    && (usage.totalTokens as number) >= 0
    && (usage.estimatedUsd === undefined || typeof usage.estimatedUsd === "string");
}

function isWorldResidentMemory(value: unknown): value is WorldResidentMemory {
  if (!value || typeof value !== "object") return false;
  const memory = value as Partial<WorldResidentMemory>;
  return typeof memory.summary === "string"
    && memory.summary.length <= 320
    && isBoundedTextList(memory.goals, 4, 120)
    && isBoundedTextList(memory.relationships, 8, 140)
    && isBoundedTextList(memory.recentDecisions, 6, 160)
    && Number.isSafeInteger(memory.lastBoardMessageId)
    && (memory.lastBoardMessageId as number) >= 0;
}

function isWorldObservation(value: unknown, agentId: ResidentId): value is WorldObservation {
  if (!isJsonObject(value)) return false;
  const observation = value as Partial<WorldObservation>;
  return Number.isSafeInteger(observation.stateVersion)
    && (observation.stateVersion as number) >= 0
    && isMinuteOfDay(observation.minuteOfDay)
    && (observation.weather === "clear" || observation.weather === "drizzle")
    && isWorldObservationSelf(observation.self, agentId)
    && isDenseArrayOf(observation.nearby, isWorldNearbyActor)
    && isDenseArrayOf(observation.roster, isWorldRosterActor)
    && (observation.playerOrder === undefined || isWorldPlayerOrder(observation.playerOrder))
    && (observation.guildCall === undefined || isHeardGuildCall(observation.guildCall))
    && isDenseArrayOf(observation.guildBoard, isWorldBoardMessage)
    && isDenseArrayOf(observation.recentEvents, isString)
    && isDenseArrayOf(observation.availableTargets, isWorldTarget)
    && isWorldSupplyState(observation.supplies);
}

function isWorldObservationSelf(
  value: unknown,
  agentId: ResidentId,
): value is WorldObservation["self"] {
  if (!isJsonObject(value)) return false;
  const self = value as Partial<WorldObservation["self"]>;
  return self.id === agentId
    && typeof self.name === "string"
    && typeof self.role === "string"
    && (self.kind === "monster" || self.kind === "human")
    && isWorldSceneId(self.scene)
    && isFiniteNumber(self.x)
    && isFiniteNumber(self.y)
    && isDirection(self.direction)
    && (self.movingTo === undefined || isWorldPosition(self.movingTo))
    && typeof self.location === "string"
    && isFiniteNumber(self.energy)
    && isFiniteNumber(self.curiosity)
    && isFiniteNumber(self.social)
    && (self.carrying === undefined || isWorldItemKind(self.carrying));
}

function isWorldNearbyActor(value: unknown): value is WorldObservation["nearby"][number] {
  if (!isJsonObject(value)) return false;
  const actor = value as Partial<WorldObservation["nearby"][number]>;
  return isActorId(actor.id)
    && typeof actor.name === "string"
    && isWorldActorKind(actor.kind)
    && isWorldSceneId(actor.scene)
    && isFiniteNumber(actor.x)
    && isFiniteNumber(actor.y)
    && isFiniteNumber(actor.relativeX)
    && isFiniteNumber(actor.relativeY)
    && isFiniteNonNegativeNumber(actor.distance)
    && isDirection(actor.direction)
    && (actor.movingTo === undefined || isWorldPosition(actor.movingTo))
    && typeof actor.activity === "string"
    && (actor.intent === undefined || typeof actor.intent === "string");
}

function isWorldRosterActor(value: unknown): value is WorldObservation["roster"][number] {
  if (!isJsonObject(value)) return false;
  const actor = value as Partial<WorldObservation["roster"][number]>;
  return isActorId(actor.id)
    && typeof actor.name === "string"
    && isWorldActorKind(actor.kind)
    && isWorldSceneId(actor.scene)
    && isFiniteNumber(actor.x)
    && isFiniteNumber(actor.y)
    && isDirection(actor.direction)
    && (actor.movingTo === undefined || isWorldPosition(actor.movingTo))
    && typeof actor.location === "string"
    && typeof actor.activity === "string";
}

function isHeardGuildCall(value: unknown): value is HeardGuildCall {
  if (!isJsonObject(value)) return false;
  const call = value as Partial<HeardGuildCall>;
  return Number.isSafeInteger(call.id)
    && (call.id as number) >= 0
    && typeof call.text === "string"
    && isVoiceLevel(call.voice)
    && (call.distance === undefined || isFiniteNonNegativeNumber(call.distance))
    && isFiniteNonNegativeNumber(call.radius)
    && typeof call.guildWide === "boolean"
    && isUniqueResidentList(call.coListeners)
    && (call.requestedTarget === undefined || isWorldTarget(call.requestedTarget));
}

function isWorldPlayerOrder(value: unknown): value is WorldPlayerOrder {
  if (!isJsonObject(value)) return false;
  const order = value as Partial<WorldPlayerOrder>;
  return Number.isSafeInteger(order.id)
    && (order.id as number) >= 0
    && typeof order.text === "string"
    && order.text.length > 0
    && order.text.length <= 140
    && isUniqueResidentList(order.coListeners)
    && (order.requestedTarget === undefined || isWorldTarget(order.requestedTarget));
}

function isWorldBoardMessage(value: unknown): value is WorldBoardMessage {
  if (!isJsonObject(value)) return false;
  const message = value as Partial<WorldBoardMessage>;
  return Number.isSafeInteger(message.id)
    && (message.id as number) >= 0
    && isActorId(message.fromId)
    && typeof message.fromName === "string"
    && (message.toId === undefined || isActorId(message.toId))
    && (message.toName === undefined || typeof message.toName === "string")
    && typeof message.text === "string"
    && isMinuteOfDay(message.minuteOfDay)
    && (message.origin === "nanocodex" || message.origin === "routine" || message.origin === "player")
    && (message.scope === "public" || message.scope === "spatial");
}

function isWorldSceneId(value: unknown): value is WorldSceneId {
  return typeof value === "string" && (WORLD_SCENE_IDS as readonly string[]).includes(value);
}

function isDirection(value: unknown): value is Direction {
  return value === "up" || value === "down" || value === "left" || value === "right";
}

function isWorldPosition(value: unknown): value is WorldPosition {
  if (!isJsonObject(value)) return false;
  const position = value as Partial<WorldPosition>;
  return isWorldSceneId(position.scene)
    && Number.isInteger(position.x)
    && Number.isInteger(position.y);
}

function isUniqueResidentList(value: unknown): value is readonly ResidentId[] {
  return Array.isArray(value)
    && value.length > 0
    && isDenseArray(value)
    && value.every(isResidentId)
    && new Set(value).size === value.length;
}

function isWorldItemKind(value: unknown): value is WorldItemKind {
  return typeof value === "string" && (WORLD_ITEM_KINDS as readonly string[]).includes(value);
}

function isWorldTarget(value: unknown): value is WorldTarget {
  return typeof value === "string" && (WORLD_TARGETS as readonly string[]).includes(value);
}

function isActorId(value: unknown): value is ActorId {
  return typeof value === "string" && (ACTOR_IDS as readonly string[]).includes(value);
}

function isWorldActorKind(value: unknown): value is "player" | "monster" | "human" {
  return value === "player" || value === "monster" || value === "human";
}

function isVoiceLevel(value: unknown): value is VoiceLevel {
  return typeof value === "string" && (VOICE_LEVELS as readonly string[]).includes(value);
}

function isWorldSupplyState(value: unknown): value is WorldSupplyState {
  if (!isJsonObject(value)) return false;
  const supplies = value as Partial<WorldSupplyState>;
  return Number.isSafeInteger(supplies.orchardBerries)
    && (supplies.orchardBerries as number) >= 0
    && Number.isSafeInteger(supplies.shopStock)
    && (supplies.shopStock as number) >= 0
    && Number.isSafeInteger(supplies.guildSupplies)
    && (supplies.guildSupplies as number) >= 0
    && Number.isSafeInteger(supplies.trainingMarks)
    && (supplies.trainingMarks as number) >= 0;
}

function isWorldFailureClass(value: unknown): value is WorldFailureClass {
  return value === "transient"
    || value === "invalid"
    || value === "cancelled";
}

function isWorldIdentifier(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0 && value.length <= 96;
}

function isOptionalUniqueList<T>(
  value: unknown,
  maxItems: number,
  predicate: (entry: unknown) => entry is T,
): value is readonly T[] | undefined {
  if (value === undefined) return true;
  if (
    !Array.isArray(value)
    || value.length < 1
    || value.length > maxItems
    || !isDenseArray(value)
  ) return false;
  const seen = new Set<T>();
  for (const entry of value) {
    if (!predicate(entry) || seen.has(entry)) return false;
    seen.add(entry);
  }
  return true;
}

function isDenseArray(value: readonly unknown[]): boolean {
  for (let index = 0; index < value.length; index += 1) {
    if (!Object.hasOwn(value, index)) return false;
  }
  return true;
}

function isDenseArrayOf<T>(
  value: unknown,
  predicate: (entry: unknown) => entry is T,
): value is readonly T[] {
  if (!Array.isArray(value) || !isDenseArray(value)) return false;
  for (const entry of value) {
    if (!predicate(entry)) return false;
  }
  return true;
}

function isJsonObject(value: unknown): value is JsonObject {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function isString(value: unknown): value is string {
  return typeof value === "string";
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isFiniteNonNegativeNumber(value: unknown): value is number {
  return isFiniteNumber(value) && value >= 0;
}

function isMinuteOfDay(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0 && (value as number) < 24 * 60;
}

function decodeAction(value: unknown): WorldAction {
  const action = object(value, "action");
  const kind = text(action.kind, "action.kind", 24);
  if (kind === "random_choice") {
    const ifTrue = decodeChoiceBranch(action.if_true, "action.if_true");
    const ifFalse = decodeChoiceBranch(action.if_false, "action.if_false");
    return Object.freeze({
      kind,
      chance_percent: integer(action.chance_percent, "action.chance_percent", 1, 99),
      true_label: text(action.true_label, "action.true_label", 24),
      false_label: text(action.false_label, "action.false_label", 24),
      if_true: Object.freeze(ifTrue),
      if_false: Object.freeze(ifFalse),
    });
  }
  return decodePrimitiveAction(action, kind);
}

function decodeChoiceBranch(value: unknown, label: string): readonly WorldPrimitiveAction[] {
  if (!Array.isArray(value) || value.length < 1 || value.length > 3 || !isDenseArray(value)) {
    throw new Error(`${label} must contain 1-3 physical actions`);
  }
  return value.map((entry) => {
    const action = object(entry, label);
    return decodePrimitiveAction(action, text(action.kind, `${label}.kind`, 24));
  });
}

function decodePrimitiveAction(action: JsonObject, kind: string): WorldPrimitiveAction {
  if (kind === "move") {
    return Object.freeze({ kind, target: member(action.target, WORLD_TARGETS, "action.target") });
  }
  if (kind === "move_relative") {
    const dxPixels = integer(action.dx_pixels, "action.dx_pixels", -192, 192);
    const dyPixels = integer(action.dy_pixels, "action.dy_pixels", -192, 192);
    if (dxPixels === 0 && dyPixels === 0) {
      throw new Error("a relative move must change at least one axis");
    }
    return Object.freeze({
      kind,
      anchor: member(action.anchor, ACTOR_IDS, "action.anchor"),
      dx_pixels: dxPixels,
      dy_pixels: dyPixels,
    });
  }
  if (kind === "maintain_relative") {
    const dxPixels = integer(action.dx_pixels, "action.dx_pixels", -192, 192);
    const dyPixels = integer(action.dy_pixels, "action.dy_pixels", -192, 192);
    if (dxPixels === 0 && dyPixels === 0) {
      throw new Error("a maintained relative position must change at least one axis");
    }
    return Object.freeze({
      kind,
      anchor: member(action.anchor, ACTOR_IDS, "action.anchor"),
      dx_pixels: dxPixels,
      dy_pixels: dyPixels,
      tolerance_pixels: integer(action.tolerance_pixels, "action.tolerance_pixels", 8, 32),
    });
  }
  if (kind === "say") {
    const dialogue = sanitizeDialogue(text(action.text, "action.text", 140));
    if (!dialogue) throw new Error("action.text must contain visible dialogue");
    const to = action.to === undefined ? undefined : member(action.to, ACTOR_IDS, "action.to");
    return Object.freeze({ kind, text: dialogue, ...(to === undefined ? {} : { to }) });
  }
  if (kind === "emote") {
    return Object.freeze({ kind, icon: member(action.icon, WORLD_EMOTES, "action.icon") });
  }
  if (kind === "interact") {
    return Object.freeze({
      kind,
      target: member(action.target, WORLD_TARGETS, "action.target"),
      action: member(action.action, WORLD_INTERACTIONS, "action.action"),
    });
  }
  throw new Error(`unsupported action kind: ${kind}`);
}

function object(value: unknown, label: string): JsonObject {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value as JsonObject;
}

function text(value: unknown, label: string, max: number): string {
  if (typeof value !== "string") throw new Error(`${label} must be a string`);
  const trimmed = value.trim();
  if (!trimmed || trimmed.length > max) throw new Error(`${label} must contain 1-${max} characters`);
  return trimmed;
}

function optionalText(value: unknown, label: string, max: number): string {
  if (value === undefined || value === null) return "";
  if (typeof value !== "string") throw new Error(`${label} must be a string`);
  const normalized = value.replace(/[\u0000-\u001f\u007f]/g, " ").replace(/\s+/g, " ").trim();
  if (normalized.length > max) throw new Error(`${label} must contain at most ${max} characters`);
  return normalized;
}

function boundedTextList(
  value: unknown,
  label: string,
  maxItems: number,
  maxLength: number,
): readonly string[] {
  if (value === undefined || value === null) return Object.freeze([]);
  if (!Array.isArray(value) || value.length > maxItems) {
    throw new Error(`${label} must contain at most ${maxItems} strings`);
  }
  return Object.freeze(value.map((entry, index) => {
    const normalized = optionalText(entry, `${label}[${index}]`, maxLength);
    if (!normalized) throw new Error(`${label}[${index}] must not be empty`);
    return normalized;
  }));
}

function isBoundedTextList(
  value: unknown,
  maxItems: number,
  maxLength: number,
): value is readonly string[] {
  return Array.isArray(value)
    && value.length <= maxItems
    && value.every((entry) => typeof entry === "string" && entry.length > 0 && entry.length <= maxLength);
}

function integer(value: unknown, label: string, min: number, max: number): number {
  if (!Number.isSafeInteger(value) || (value as number) < min || (value as number) > max) {
    throw new Error(`${label} must be an integer from ${min} to ${max}`);
  }
  return value as number;
}

function member<const values extends readonly string[]>(
  value: unknown,
  values: values,
  label: string,
): values[number] {
  if (typeof value !== "string" || !(values as readonly string[]).includes(value)) {
    throw new Error(`${label} is not an allowed value`);
  }
  return value as values[number];
}
