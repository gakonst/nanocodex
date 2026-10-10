import { gatewayAvailability, type GatewaySecrets } from "./gateway-runtime";
import { fetchResponseWithDeadline } from "./deadline";
import { AGENT_REASONING_MODES, AGENT_THINKING, DEFAULT_AGENT_SETTINGS, DEFAULT_OPENAI_AGENT_SETTINGS, validateAgentSettings, type ManagedAgentSettings } from "./agent-settings";

type ModelRuntime = GatewaySecrets & { NANOCODEX_THREAD_ROUTING?: string };
const openAIModels = [["gpt-6-astra", "GPT-6 Astra"], ["gpt-6.1-sol", "GPT-6.1 Sol"], ["gpt-6-luna", "GPT-6 Luna"]] as const;
/**
 * A model's offered settings, derived from managed admission
 * (validateAgentSettings), which nanocodex's model-capabilities test checks
 * against the shared Rust capability source for every model and setting.
 */
function offeredSettings(model: ManagedAgentSettings["model"]) {
  const accepts = (thinking: string, reasoningMode: string, fastMode: boolean) => {
    try {
      validateAgentSettings({ model, thinking, reasoning_mode: reasoningMode, fast_mode: fastMode } as ManagedAgentSettings);
      return true;
    } catch { return false; }
  };
  const thinking = AGENT_THINKING.filter(level => accepts(level, "standard", false));
  const base = thinking[0];
  return {
    thinking: [...thinking] as string[],
    fast_mode: base !== undefined && accepts(base, "standard", true),
    reasoning_modes: AGENT_REASONING_MODES.filter(mode => base !== undefined && accepts(base, mode, false)) as string[],
  };
}
const connected = (value: unknown) => !!value && typeof value === "object" && (value as { connected?: unknown }).connected === true;

async function credentialStatus(broker: Fetcher, userId: string) {
  return fetchResponseWithDeadline(broker,
    `https://broker.internal/users/${encodeURIComponent(userId)}/credentials`, {}, 10_000,
    "model availability", async response => {
      if (!response.ok) throw new Error("model availability broker is unavailable");
      return response.json<Record<string, unknown>>();
    });
}

/** Choose the preferred default from the live, account-authorized catalog.
 * Claude Opus 5.5 is preferred; callers that cannot run Claude (Connect grants,
 * Responses-only configuration) pass `claude: false` for the OpenAI fallback.
 * Status is always live; the returned catalog is reused only within this request. */
export async function selectDefaultManagedModel(broker: Fetcher, userId: string, runtime: ModelRuntime = {}, options: { claude?: boolean } = {}): Promise<{
  default_model: ManagedAgentSettings["model"] | null;
  catalog?: Awaited<ReturnType<typeof availableManagedModels>>;
}> {
  const status = await credentialStatus(broker, userId);
  if ((options.claude === false || !connected(status.claude))
    && (connected(status.chatgpt) || connected(status.openai))) {
    return { default_model: DEFAULT_OPENAI_AGENT_SETTINGS.model };
  }
  if (options.claude === false) return { default_model: null };
  const catalog = await modelsFromStatus(broker, userId, runtime, status);
  return { default_model: catalog.default_model, catalog };
}

/** Initial settings for a selected default model: its own initial effort. */
export function defaultSettingsForModel(model: ManagedAgentSettings["model"]): ManagedAgentSettings {
  return model === DEFAULT_AGENT_SETTINGS.model ? DEFAULT_AGENT_SETTINGS
    : model === DEFAULT_OPENAI_AGENT_SETTINGS.model ? DEFAULT_OPENAI_AGENT_SETTINGS
      : { ...DEFAULT_OPENAI_AGENT_SETTINGS, model };
}

/** Catalog admission comes from the account broker, never a client label. */
/** Native Claude models children may use: every model the Rust Claude harness supports, gated by the account's live grant. */
const NATIVE_CLAUDE_CHILD_MODELS = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-1", "claude-opus-4-6", "claude-sonnet-4-6", "claude-haiku-4-5"] as const;
export async function availableClaudeChildModels(broker: Fetcher, userId: string): Promise<string[]> {
  if (!connected((await credentialStatus(broker, userId)).claude)) return [];
  const allowed = await fetchResponseWithDeadline(broker,
    `https://broker.internal/users/${encodeURIComponent(userId)}/credentials/claude/models`, {}, 15_000,
    "Claude model availability", async response => {
      if (!response.ok) throw new Error("Claude model catalog is unavailable");
      return response.json<{ models: Array<{ id: string }> }>();
    });
  if (!Array.isArray(allowed.models)) throw new Error("invalid Claude model catalog");
  // Provider IDs may carry a date suffix (claude-haiku-4-5-20251001).
  return NATIVE_CLAUDE_CHILD_MODELS.filter(id => allowed.models.some(model => model.id === id || model.id.startsWith(`${id}-`)));
}

export async function availableManagedModels(broker: Fetcher, userId: string, runtime: ModelRuntime = {}) {
  return modelsFromStatus(broker, userId, runtime, await credentialStatus(broker, userId));
}

/**
 * Creation-time check for one explicitly requested Claude model. Skips the
 * full credential status read (which wakes the Claude WASM state machine);
 * the egress listing resolves the credential itself and is isolate-cached.
 */
export async function claudeModelAvailable(broker: Fetcher, userId: string, model: string): Promise<boolean> {
  return fetchResponseWithDeadline(broker,
    `https://broker.internal/users/${encodeURIComponent(userId)}/credentials/claude/models`,
    { headers: { "x-nanocodex-catalog-cache": "allow" } }, 15_000,
    "Claude model availability", async response => {
      if (response.status === 409) { await response.body?.cancel(); return false; }
      if (!response.ok) throw new Error("Claude model catalog is unavailable");
      const allowed = await response.json<{ models: Array<{ id: string }> }>();
      if (!Array.isArray(allowed.models)) throw new Error("invalid Claude model catalog");
      return allowed.models.some(entry => entry.id === model);
    });
}

async function modelsFromStatus(broker: Fetcher, userId: string, runtime: ModelRuntime, status: Record<string, unknown>) {
  const data: Array<{ id: ManagedAgentSettings["model"]; name: string; provider: string; thinking: string[]; fast_mode: boolean; reasoning_modes: string[] }> = [];
  if (connected(status.chatgpt) || connected(status.openai)) {
    for (const [id, name] of openAIModels)
      data.push({ id, name, provider: "openai", ...offeredSettings(id) });
  }
  // Provider access is independent from credentials being connected. The public
  // catalog is the intersection of that live grant and this runtime's verified
  // effort-capable models; unknown/gated models are not silently advertised.
  // https://platform.claude.com/docs/en/about-claude/models/overview
  // https://platform.claude.com/docs/en/build-with-claude/effort
  let claudeUnavailable = false;
  let claudePartial = false;
  const routing = runtime.NANOCODEX_THREAD_ROUTING === "true" && typeof runtime.AI?.run === "function";
  if (routing) {
    if (typeof runtime.AI?.run === "function") data.push({ id: "@cf/zai-org/glm-5.3", name: "GLM 5.3", provider: "workers_ai", ...offeredSettings("@cf/zai-org/glm-5.3") });
    const gateway = gatewayAvailability(runtime);
    if (gateway.openrouter || gateway.vercel) {
      data.push({ id: "kimi-k3", name: "Kimi K3", provider: "gateway", ...offeredSettings("kimi-k3") });
      data.push({ id: "mimo-v2.6-pro", name: "MiMo V2.6 Pro", provider: "gateway", ...offeredSettings("mimo-v2.6-pro") });
    }
  }
  if (connected(status.claude)) {
    try {
    const allowed = await fetchResponseWithDeadline(broker,
      `https://broker.internal/users/${encodeURIComponent(userId)}/credentials/claude/models`, {}, 15_000,
      "Claude model availability", async response => {
        if (!response.ok) throw new Error("Claude model catalog is unavailable");
        return response.json<{ models: Array<{ id: string; display_name?: string }>; has_more: boolean }>();
      });
    claudePartial = allowed.has_more === true;
    if (!Array.isArray(allowed.models)) throw new Error("invalid Claude model catalog");
    for (const [id, name] of [["claude-sonnet-4-6", "Claude Sonnet 4.6"], ["claude-opus-4-6", "Claude Opus 4.6"],
      ["claude-sonnet-5-5", "Claude Sonnet 5.5"], ["claude-opus-5-5", "Claude Opus 5.5"]] as const) {
      if (allowed.models.some(model => model.id === id)) data.push({ id, name, provider: "claude", ...offeredSettings(id) });
    }
    } catch { claudeUnavailable = true; if (!data.length) throw new Error("Claude model catalog is unavailable"); }
  }
  return { partial: claudeUnavailable || claudePartial, availability: { claude: { connected: connected(status.claude), available: data.some(model => model.provider === "claude"), ...(claudeUnavailable ? { error: "claude_models_unavailable" } : {}) } }, object: "list" as const, data, default_model: data.find(model => model.id === DEFAULT_AGENT_SETTINGS.model)?.id
    ?? data.find(model => model.id === DEFAULT_OPENAI_AGENT_SETTINGS.model)?.id
    ?? data.find(model => model.provider === "claude")?.id ?? data[0]?.id ?? null };
}
