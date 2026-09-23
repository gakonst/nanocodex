export const AGENT_MODELS = [
    "gpt-6-sol",
    "gpt-6-luna",
    "gpt-6-astra",
    "@cf/zai-org/glm-5.3",
    "kimi-k3",
    "mimo-v2.6-pro",
];
export const AGENT_THINKING = [
    "none",
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
];
export const AGENT_REASONING_MODES = ["standard", "pro"];
export const DEFAULT_AGENT_SETTINGS = Object.freeze({
    model: "gpt-6-astra",
    thinking: "low",
    reasoning_mode: "standard",
    fast_mode: false,
});
const QUERY_KEYS = new Set([
    "model",
    "thinking",
    "reasoning_mode",
    "fast_mode",
]);
export function parseAgentSettingsQuery(search) {
    for (const key of search.keys()) {
        if (!QUERY_KEYS.has(key)
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
export function agentSettingsQuery(settings) {
    return new URLSearchParams({
        model: settings.model,
        thinking: settings.thinking,
        reasoning_mode: settings.reasoning_mode,
        fast_mode: String(settings.fast_mode),
    });
}
export function parseAgentSettingsPatch(value) {
    if (!value || typeof value !== "object" || Array.isArray(value)) {
        throw new TypeError("agent settings must be a JSON object");
    }
    const input = value;
    const keys = Object.keys(input);
    if (keys.length === 0 || keys.some((key) => !QUERY_KEYS.has(key))) {
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
        ...(Object.hasOwn(input, "model") ? { model: input.model } : {}),
        ...(Object.hasOwn(input, "thinking")
            ? { thinking: input.thinking }
            : {}),
        ...(Object.hasOwn(input, "reasoning_mode")
            ? { reasoning_mode: input.reasoning_mode }
            : {}),
        ...(Object.hasOwn(input, "fast_mode") ? { fast_mode: input.fast_mode } : {}),
    };
}
export function parseCompleteAgentSettings(value) {
    const settings = parseAgentSettingsPatch(value);
    if (Object.keys(settings).length !== 4) {
        throw new TypeError("agent settings must contain all four fields");
    }
    return validateAgentSettings(settings);
}
export function validateAgentSettings(settings) {
    if (["@cf/zai-org/glm-5.3", "kimi-k3", "mimo-v2.6-pro"].includes(settings.model)
        && (!(settings.model === "kimi-k3" ? ["low", "high"] : ["low", "medium", "high"]).includes(settings.thinking) || settings.reasoning_mode !== "standard" || settings.fast_mode)) {
        throw new TypeError("Gateway model requires a supported effort, standard mode, and no fast mode");
    }
    if (settings.model === "gpt-6-astra" && settings.thinking === "none") {
        throw new TypeError("GPT-6 Astra requires low, medium, high, xhigh, or max thinking");
    }
    if (settings.model === "gpt-6-astra" && settings.reasoning_mode === "pro") {
        throw new TypeError("GPT-6 Astra does not support pro reasoning mode");
    }
    return settings;
}
/** Public admission cannot select the OSS model without a committed thread route. */
export function validateAgentAdmissionSettings(settings) {
    if (["@cf/zai-org/glm-5.3", "kimi-k3", "mimo-v2.6-pro"].includes(settings.model)) {
        throw new TypeError("Gateway model is available only through model_routing; omit explicit model settings");
    }
    return validateAgentSettings(settings);
}
export function isAgentModel(value) {
    return typeof value === "string" && AGENT_MODELS.includes(value);
}
export function isAgentThinking(value) {
    return typeof value === "string" && AGENT_THINKING.includes(value);
}
export function isAgentReasoningMode(value) {
    return typeof value === "string"
        && AGENT_REASONING_MODES.includes(value);
}
