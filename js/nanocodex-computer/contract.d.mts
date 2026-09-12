export const CUA_JS_NAME: "mcp__cua_repl__js";
export const CUA_RESET_NAME: "mcp__cua_repl__js_reset";
export const CUA_DESCRIPTION: string;
export const CUA_PARAMETERS: Readonly<{ type: "object"; properties: Record<string, unknown>; required: readonly string[]; additionalProperties: false }>;
export const CUA_RESET_DESCRIPTION: string;
export const CUA_RESET_PARAMETERS: Readonly<{ type: "object"; properties: Record<string, unknown>; additionalProperties: false }>;
export function validateInput(input: unknown, reset?: boolean): { code?: string; title?: string; timeout_ms?: number };
