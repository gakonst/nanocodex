import type { Turn, TurnResult } from "nanocodex";

import type { ServerMessage, TurnCompleted } from "./protocol";

export type TurnTerminal = Extract<ServerMessage, {
  type: "turn_completed" | "turn_cancelled" | "turn_failed";
}>;

export type TurnResolution =
  | Readonly<{ kind: "terminal"; terminal: TurnTerminal; reopenAgent: false }>
  | Readonly<{ kind: "retry"; error: string; reopenAgent: boolean; blockedBy?: string }>;

export type ManagedTurnTransition = TurnTerminal | Extract<ServerMessage, {
  type: "turn_cancelling" | "turn_retryable";
}>;

export function managedControlTransitionForResolution(
  id: string,
  cancelling: boolean,
  resolution: TurnResolution,
): ManagedTurnTransition {
  const error = resolution.kind === "retry"
    ? resolution.error
    : resolution.terminal.type === "turn_failed"
    ? resolution.terminal.error
    : undefined;
  if (cancelling
    && error === "the targeted turn has already completed or been cancelled") {
    return { type: "turn_cancelled", id };
  }
  if (resolution.kind === "retry") {
    return cancelling ? {
      type: "turn_cancelling",
      id,
      error: resolution.error,
    } : {
      type: "turn_retryable",
      id,
      error: resolution.error,
    };
  }
  const terminal = resolution.terminal;
  if (cancelling && terminal.type !== "turn_cancelled") {
    return {
      type: "turn_cancelling",
      id,
      error: "error" in terminal ? terminal.error : "cancellation did not settle",
    };
  }
  return terminal;
}

export function managedCancellationAlarmTarget(options: Readonly<{
  now: number;
  retryAt: number | null;
  cancellationInFlight: boolean;
  deliveredToLiveTurn: boolean;
  recoveryLeaseMs: number;
}>): number {
  return options.cancellationInFlight || options.deliveredToLiveTurn
    ? options.now + options.recoveryLeaseMs
    : options.retryAt ?? options.now + 1;
}

export function cancellationDeliveryMatchesLiveTurn<T>(options: Readonly<{
  cancelling: boolean;
  deliveredTurn: T | undefined;
  liveTurn: T | undefined;
}>): boolean {
  return options.cancelling
    && options.deliveredTurn !== undefined
    && options.deliveredTurn === options.liveTurn;
}

export async function materializeTurnResolution(
  id: string,
  turn: Turn,
): Promise<TurnResolution> {
  let result: TurnResult | undefined;
  try {
    result = await turn.result();
    let usage: Awaited<ReturnType<TurnResult["usage"]>> | null = null;
    let usageError: string | undefined;
    try {
      usage = await result.usage();
    } catch (error) {
      usageError = errorMessage(error);
    }
    return {
      kind: "terminal",
      terminal: {
        type: "turn_completed",
        id,
        final_message: result.finalMessage,
        usage,
        citations: [],
        ...(usageError === undefined ? {} : { usage_error: usageError }),
      },
      reopenAgent: false,
    };
  } catch (error) {
    return classifyTurnFailure(id, error);
  } finally {
    result?.dispose();
  }
}

export function classifyTurnFailure(id: string, error: unknown): TurnResolution {
  const selected = selectFailure(errorTree(error));
  if (selected.code === "cancelled"
    || (selected.code === undefined && /\bturn was cancelled\b/i.test(selected.message))) {
    return {
      kind: "terminal",
      terminal: { type: "turn_cancelled", id },
      reopenAgent: false,
    };
  }
  if (isRetryable(selected)) {
    return {
      kind: "retry",
      error: selected.message,
      ...(selected.blockedBy === undefined ? {} : { blockedBy: selected.blockedBy }),
      reopenAgent: selected.code === "reopen_required"
        || /\bagent (?:has been |was |is )?(?:already )?disposed\b/i.test(selected.message),
    };
  }
  return {
    kind: "terminal",
    terminal: { type: "turn_failed", id, error: selected.message },
    reopenAgent: false,
  };
}

type ClassifiedError = Readonly<{ code: string | undefined; message: string; blockedBy?: string }>;

function errorTree(root: unknown): ClassifiedError[] {
  const failures: ClassifiedError[] = [];
  const pending = [root];
  const seen = new Set<unknown>();
  while (pending.length > 0) {
    const error = pending.shift();
    if ((typeof error === "object" && error !== null) || typeof error === "function") {
      if (seen.has(error)) continue;
      seen.add(error);
    }
    const blockedBy = (error as { blockedBy?: unknown } | null)?.blockedBy;
    failures.push({ code: errorCode(error), message: errorMessage(error),
      ...(typeof blockedBy === "string" && blockedBy.length > 0 ? { blockedBy } : {}),
    });
    if (error instanceof AggregateError) pending.push(...error.errors);
    const cause = (error as { cause?: unknown } | null)?.cause;
    if (cause !== undefined) pending.push(cause);
  }
  return failures;
}

function selectFailure(failures: readonly ClassifiedError[]): ClassifiedError {
  for (const code of ["reopen_required", "cancelled", "retryable"]) {
    const match = failures.find((failure) => failure.code === code);
    if (match) return match;
  }
  const terminal = failures.find((failure) =>
    failure.code === "failed" || failure.code === "invalid_request" || failure.code === "conflict");
  if (terminal) return terminal;
  return failures.find((failure) => isRetryable(failure))
    ?? failures.find((failure) => /\bturn was cancelled\b/i.test(failure.message))
    ?? failures.find((failure) => failure.code === "failed")
    ?? failures[0]
    ?? { code: undefined, message: "unknown turn failure" };
}

function isRetryable(failure: ClassifiedError): boolean {
  // Rust's operation settlement is authoritative. Text from a committed
  // failure may mention a transport, a cancelled tool, or an old retry.
  if (failure.code !== undefined) {
    return failure.code === "reopen_required" || failure.code === "retryable";
  }
  return /\bagent (?:has been |was |is )?(?:already )?disposed\b|already active|agent stopped|turn completed|durability (?:store|driver)|transport|websocket|startup (?:validation )?timed out|connection rejected with HTTP 5\d\d/i.test(failure.message);
}

function errorCode(error: unknown): string | undefined {
  const code = (error as { code?: unknown } | null)?.code;
  return typeof code === "string" ? code : undefined;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export type { TurnCompleted };
