import { parseConfiguration, type AgentConfiguration } from "./agent-configuration";
import { AGENT_MODELS, AGENT_THINKING, AGENT_REASONING_MODES, DEFAULT_AGENT_SETTINGS, parseAgentSettingsQuery, agentSettingsQuery, parseAgentSettingsPatch, parseCompleteAgentSettings, validateAgentSettings, validateAgentAdmissionSettings, isAgentModel, isAgentThinking, isAgentReasoningMode, type ManagedAgentSettings, type ManagedAgentSettingsPatch } from "nanocodex/cloudflare/agent-settings";
export { AGENT_MODELS, AGENT_THINKING, AGENT_REASONING_MODES, DEFAULT_AGENT_SETTINGS, parseAgentSettingsQuery, agentSettingsQuery, parseAgentSettingsPatch, parseCompleteAgentSettings, validateAgentSettings, validateAgentAdmissionSettings, isAgentModel, isAgentThinking, isAgentReasoningMode, type ManagedAgentSettings, type ManagedAgentSettingsPatch };
export type ManagedAgentCreateBody = Readonly<{
  durability?: unknown;
  configuration?: AgentConfiguration;
  definition_id?: string;
  environment_template_id?: string;
  settings: ManagedAgentSettings;
  settingsProvided: boolean;
  settingsSelection?: InitialSettingsSelection;
}>;

export type ManagedAgentRunBody = Readonly<{
  creationBody: string;
  input: unknown;
}>;

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
  if (keys.some((key) => !["durability", "settings", "configuration", "definition_id", "environment_template_id", "settings_selection"].includes(key))
    || (Object.hasOwn(body, "durability") && body.durability === undefined)
    || (Object.hasOwn(body, "settings") && body.settings === undefined)) {
    throw new TypeError("agent creation body contains unsupported or missing fields");
  }
  for (const key of ["definition_id", "environment_template_id"]) {
    if (body[key] !== undefined && (typeof body[key] !== "string" || !/^[A-Za-z0-9_-]{1,64}$/.test(body[key] as string))) throw new TypeError("invalid template ID");
  }
  const settingsProvided = Object.hasOwn(body, "settings");
  const settingsSelection = body.settings_selection === undefined ? undefined : parseInitialSettingsSelection(body.settings_selection);
  if (settingsSelection && (settingsProvided || body.durability !== undefined || body.definition_id !== undefined || body.environment_template_id !== undefined)) {
    throw new TypeError("settings_selection cannot be combined with settings, templates, or imports");
  }

  if (settingsProvided && body.configuration && (body.configuration as AgentConfiguration).model_routing) {
    throw new TypeError("model_routing owns model and thinking; omit settings");
  }
  const configuration = body.configuration === undefined ? undefined : parseConfiguration(body.configuration);
  if (configuration?.settings) validateAgentAdmissionSettings(configuration.settings);
  if (settingsSelection && (configuration?.settings || configuration?.model_routing)) throw new TypeError("settings_selection conflicts with configuration settings");
  return {
    ...(Object.hasOwn(body, "durability") ? { durability: body.durability } : {}),
    settings: settingsProvided
      ? validateAgentAdmissionSettings(parseCompleteAgentSettings(body.settings))
      : DEFAULT_AGENT_SETTINGS,
    ...(configuration === undefined ? {} : { configuration }),
    ...(body.definition_id === undefined ? {} : { definition_id: body.definition_id as string }),
    ...(body.environment_template_id === undefined ? {} : { environment_template_id: body.environment_template_id as string }),
    settingsProvided,
    ...(settingsSelection ? { settingsSelection } : {}),
  };
}

/** Split one combined create-and-prompt request before either mutation starts. */
export function parseAgentRunBody(encoded: string): ManagedAgentRunBody {
  if (!encoded.trim()) {
    throw new TypeError("agent run body must be a JSON object");
  }
  const value = JSON.parse(encoded) as unknown;
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("agent run body must be a JSON object");
  }
  const { input, ...creation } = value as Record<string, unknown>;
  if (!Object.hasOwn(value, "input")) {
    throw new TypeError("agent run body requires input");
  }
  const creationBody = Object.keys(creation).length === 0
    ? ""
    : JSON.stringify(creation);
  // Validate every creation field before the caller can create an empty agent.
  parseAgentCreateBody(creationBody);
  return { creationBody, input };
}

/** Explicit opt-in preserves historical creation defaults for existing clients. */
export type InitialSettingsSelection = {
  policy: "cli" | "sdk";
  thinking?: ManagedAgentSettings["thinking"];
  reasoning_mode?: ManagedAgentSettings["reasoning_mode"];
  fast_mode?: boolean;
};
function parseInitialSettingsSelection(value: unknown): InitialSettingsSelection {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError("invalid settings_selection");
  const selection = value as Record<string, unknown>;
  if (Object.keys(selection).some(key => !["policy", "thinking", "reasoning_mode", "fast_mode"].includes(key))
    || !["cli", "sdk"].includes(selection.policy as string)
    || (selection.thinking !== undefined && !isAgentThinking(selection.thinking))
    || (selection.reasoning_mode !== undefined && !isAgentReasoningMode(selection.reasoning_mode))
    || (selection.fast_mode !== undefined && typeof selection.fast_mode !== "boolean")) throw new TypeError("invalid settings_selection");
  return selection as InitialSettingsSelection;
}

// Native AgentSettings::new policy, exhaustive so new models need an explicit default.
export const INITIAL_MODEL_THINKING = {
  "gpt-6-astra": "low", "gpt-6.1-sol": "low", "gpt-6-luna": "medium",
  "@cf/zai-org/glm-5.3": "low", "kimi-k3": "low", "mimo-v2.6-pro": "low",
  "claude-sonnet-4-6": "medium", "claude-opus-4-6": "medium",
  "claude-sonnet-5-5": "medium", "claude-opus-5-5": "medium",
} satisfies Record<ManagedAgentSettings["model"], ManagedAgentSettings["thinking"]>;
