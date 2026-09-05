import type { PromptRequest } from "./protocol";

const SESSION_ID = /^[A-Za-z0-9_-]{1,200}$/;
const TURN_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const MAX_PROMPT_BYTES = 1024 * 1024;

export class RequestError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly status = 400,
  ) {
    super(message);
  }
}

export function parseSessionId(value: unknown): string {
  if (typeof value !== "string" || !SESSION_ID.test(value)) {
    throw new RequestError("invalid_session_id", "session ID is invalid");
  }
  return value;
}

export function parsePrompt(value: unknown): PromptRequest {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new RequestError("invalid_prompt", "prompt must be a JSON object");
  }
  const prompt = value as Record<string, unknown>;
  if (typeof prompt.id !== "string" || !TURN_ID.test(prompt.id)) {
    throw new RequestError("invalid_turn_id", "turn ID must be 1-128 safe ASCII characters");
  }
  if (typeof prompt.input !== "string" || !prompt.input.trim()) {
    throw new RequestError("empty_prompt", "prompt input must not be empty");
  }
  if (Buffer.byteLength(prompt.input, "utf8") > MAX_PROMPT_BYTES) {
    throw new RequestError("prompt_too_large", "prompt input exceeds 1 MiB", 413);
  }
  return { id: prompt.id, input: prompt.input };
}

export function parseStartIndex(value: string | null): number {
  if (value === null || value === "") return 0;
  if (!/^\d+$/.test(value)) {
    throw new RequestError("invalid_start_index", "startIndex must be a non-negative integer");
  }
  const index = Number(value);
  if (!Number.isSafeInteger(index)) {
    throw new RequestError("invalid_start_index", "startIndex is too large");
  }
  return index;
}

export function errorResponse(error: unknown): Response {
  const request = error instanceof RequestError
    ? error
    : new RequestError("internal_error", errorMessage(error), 500);
  return Response.json(
    { error: { code: request.code, message: request.message } },
    { status: request.status, headers: { "cache-control": "no-store" } },
  );
}

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
