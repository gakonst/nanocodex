export const AGENT_MODELS = [
  "gpt-5.6-sol",
  "gpt-5.6-terra",
  "gpt-5.6-luna",
  "gpt-6-astra",
] as const;

export const AGENT_THINKING = [
  "none",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
] as const;

export const AGENT_REASONING_MODES = ["standard", "pro"] as const;

export type ManagedAgentSettings = Readonly<{
  model: (typeof AGENT_MODELS)[number];
  thinking: (typeof AGENT_THINKING)[number];
  reasoning_mode: (typeof AGENT_REASONING_MODES)[number];
  fast_mode: boolean;
}>;

export type ManagedAgentSettingsPatch = Partial<ManagedAgentSettings>;

export type ManagedAgentCreateBody = Readonly<{
  durability?: unknown;
  settings: ManagedAgentSettings;
  settingsProvided: boolean;
}>;

export const DEFAULT_AGENT_SETTINGS: ManagedAgentSettings = Object.freeze({
  model: "gpt-5.6-sol",
  thinking: "high",
  reasoning_mode: "standard",
  fast_mode: false,
});

const QUERY_KEYS = new Set<keyof ManagedAgentSettings>([
  "model",
  "thinking",
  "reasoning_mode",
  "fast_mode",
]);

export function parseAgentSettingsQuery(
  search: URLSearchParams,
): ManagedAgentSettings {
  for (const key of search.keys()) {
    if (!QUERY_KEYS.has(key as keyof ManagedAgentSettings)
      || search.getAll(key).length !== 1) {
      throw new TypeError("invalid agent settings query");
    }
  }
  const model = search.get("model") ?? DEFAULT_AGENT_SETTINGS.model;
  const thinking = search.get("thinking") ?? DEFAULT_AGENT_SETTINGS.thinking;
  const reasoningMode = search.get("reasoning_mode")
    ?? DEFAULT_AGENT_SETTINGS.reasoning_mode;
  const encodedFastMode = search.get("fast_mode");
  if (!isAgentModel(model)
    || !isAgentThinking(thinking)
    || !isAgentReasoningMode(reasoningMode)
    || (encodedFastMode !== null
      && encodedFastMode !== "true"
      && encodedFastMode !== "false")) {
    throw new TypeError("invalid agent settings query");
  }
  return validateAgentSettings({
    model,
    thinking,
    reasoning_mode: reasoningMode,
    fast_mode: encodedFastMode === null
      ? DEFAULT_AGENT_SETTINGS.fast_mode
      : encodedFastMode === "true",
  });
}

export function agentSettingsQuery(settings: ManagedAgentSettings): URLSearchParams {
  return new URLSearchParams({
    model: settings.model,
    thinking: settings.thinking,
    reasoning_mode: settings.reasoning_mode,
    fast_mode: String(settings.fast_mode),
  });
}

export function parseAgentSettingsPatch(value: unknown): ManagedAgentSettingsPatch {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("agent settings must be a JSON object");
  }
  const input = value as Record<string, unknown>;
  const keys = Object.keys(input);
  if (keys.length === 0 || keys.some((key) => !QUERY_KEYS.has(key as keyof ManagedAgentSettings))) {
    throw new TypeError("agent settings contain unsupported fields");
  }
  if (Object.hasOwn(input, "model") && !isAgentModel(input.model)) {
    throw new TypeError("invalid agent model");
  }
  if (Object.hasOwn(input, "thinking") && !isAgentThinking(input.thinking)) {
    throw new TypeError("invalid agent thinking");
  }
  if (Object.hasOwn(input, "reasoning_mode") && !isAgentReasoningMode(input.reasoning_mode)) {
    throw new TypeError("invalid agent reasoning mode");
  }
  if (Object.hasOwn(input, "fast_mode") && typeof input.fast_mode !== "boolean") {
    throw new TypeError("invalid agent fast mode");
  }
  return {
    ...(Object.hasOwn(input, "model") ? { model: input.model as ManagedAgentSettings["model"] } : {}),
    ...(Object.hasOwn(input, "thinking")
      ? { thinking: input.thinking as ManagedAgentSettings["thinking"] }
      : {}),
    ...(Object.hasOwn(input, "reasoning_mode")
      ? { reasoning_mode: input.reasoning_mode as ManagedAgentSettings["reasoning_mode"] }
      : {}),
    ...(Object.hasOwn(input, "fast_mode") ? { fast_mode: input.fast_mode as boolean } : {}),
  };
}

export function parseCompleteAgentSettings(value: unknown): ManagedAgentSettings {
  const settings = parseAgentSettingsPatch(value);
  if (Object.keys(settings).length !== 4) {
    throw new TypeError("agent settings must contain all four fields");
  }
  return validateAgentSettings(settings as ManagedAgentSettings);
}

export function validateAgentSettings(
  settings: ManagedAgentSettings,
): ManagedAgentSettings {
  if (settings.model === "gpt-6-astra" && settings.thinking === "none") {
    throw new TypeError("GPT-6 Astra requires low, medium, high, xhigh, or max thinking");
  }
  if (settings.model === "gpt-6-astra" && settings.reasoning_mode === "pro") {
    throw new TypeError("GPT-6 Astra does not support pro reasoning mode");
  }
  return settings;
}

export function parseAgentCreateBody(encoded: string): ManagedAgentCreateBody {
  if (!encoded.trim()) {
    return { settings: DEFAULT_AGENT_SETTINGS, settingsProvided: false };
  }
  const value = JSON.parse(encoded) as unknown;
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("agent creation body must be a JSON object");
  }
  const body = value as Record<string, unknown>;
  const keys = Object.keys(body);
  if (keys.length === 0
    || keys.some((key) => key !== "durability" && key !== "settings")
    || (Object.hasOwn(body, "durability") && body.durability === undefined)
    || (Object.hasOwn(body, "settings") && body.settings === undefined)) {
    throw new TypeError("agent creation body contains unsupported or missing fields");
  }
  const settingsProvided = Object.hasOwn(body, "settings");
  return {
    ...(Object.hasOwn(body, "durability") ? { durability: body.durability } : {}),
    settings: settingsProvided
      ? parseCompleteAgentSettings(body.settings)
      : DEFAULT_AGENT_SETTINGS,
    settingsProvided,
  };
}

export function isAgentModel(value: unknown): value is ManagedAgentSettings["model"] {
  return typeof value === "string" && (AGENT_MODELS as readonly string[]).includes(value);
}

export function isAgentThinking(value: unknown): value is ManagedAgentSettings["thinking"] {
  return typeof value === "string" && (AGENT_THINKING as readonly string[]).includes(value);
}

export function isAgentReasoningMode(
  value: unknown,
): value is ManagedAgentSettings["reasoning_mode"] {
  return typeof value === "string"
    && (AGENT_REASONING_MODES as readonly string[]).includes(value);
}
