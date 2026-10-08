import { AccountHostedToolsProvider, type AccountHostedTools, type AccountHostedToolsCallRoutes } from "./account-hosted-tools";
import type { ToolContext } from "nanocodex";
import { AppError, type AppValidationResult, type AppValidator } from "./prompt-apps";

/** Use the installed runtime on an account-owned, online native Hand. No source
 * approximation, shell execution, or live agent/network callback is involved. */
export function nativeAppValidator(
  namespace: DurableObjectNamespace<AccountHostedTools>, owner: string,
  context: Pick<ToolContext, "sessionId" | "callId" | "signal"> & Partial<Pick<ToolContext, "model" | "turnId" | "subagent">>,
  authorized: () => boolean,
  callRoutes?: AccountHostedToolsCallRoutes,
): AppValidator {
  return async input => {
    context.signal.throwIfAborted();
    if (!authorized()) throw new AppError("forbidden", 403);
    const provider = new AccountHostedToolsProvider(namespace, owner, authorized, undefined, callRoutes);
    try { await provider.refresh(); } catch { throw new AppError("app_validation_unavailable", 503); }
    const candidates = provider.machines(context).filter(machine => provider.machineOnline(machine.id, context)
      && provider.machineTool(machine.id, "validate_app", context));
    // Prefer the phone renderer when available; validation never opens the saved
    // app or writes its state. Deterministic selection avoids accidental fanout.
    candidates.sort((a, b) => Number(!a.capabilities.includes("background_limited")) - Number(!b.capabilities.includes("background_limited")) || a.id.localeCompare(b.id));
    const machine = candidates[0];
    if (!machine) throw new AppError("app_validation_unavailable", 503);
    const tool = provider.machineTool(machine.id, "validate_app", context)!;
    const response = await tool.handler(input, { ...context, callId: context.callId + ":app-preflight" }) as {
      success?: boolean; structuredResult?: AppValidationResult;
    };
    context.signal.throwIfAborted();
    if (!authorized()) throw new AppError("forbidden", 403);
    if (response?.success !== true || !response.structuredResult) throw new AppError("app_validation_unavailable", 503);
    return { ...response.structuredResult, validator_machine: machine.id };
  };
}
