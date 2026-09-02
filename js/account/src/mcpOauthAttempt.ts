export type McpOauthAttemptMode = "idle" | "blocking" | "recoverable";

/**
 * Describes whether an MCP OAuth attempt owns the account-page action fence.
 * A live attempt has a popup handle; a restored continuation does not.
 */
export function mcpOauthAttemptMode(
  attempt: Readonly<{ popup?: unknown }> | undefined,
): McpOauthAttemptMode {
  if (!attempt) return "idle";
  return attempt.popup === undefined ? "recoverable" : "blocking";
}
