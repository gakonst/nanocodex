import type { Thinking } from "nanocodex";

const MAX_BODY_CHARS = 64 * 1024;
const THINKING = new Set<Thinking>(["none", "low", "medium", "high", "xhigh", "max"]);

export type FetchPrompt = {
  prompt: string;
  thinking: Thinking;
};

export function parseFetchPrompt(encoded: string): FetchPrompt {
  if (encoded.length > MAX_BODY_CHARS) throw new RequestError(413, "request body exceeds 64 KiB");
  let value: unknown;
  try {
    value = JSON.parse(encoded);
  } catch {
    throw new RequestError(400, "request body must be JSON");
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new RequestError(400, "request body must be an object");
  }
  const body = value as Record<string, unknown>;
  if (typeof body.prompt !== "string" || !body.prompt.trim()) {
    throw new RequestError(400, "prompt must be a non-empty string");
  }
  const thinking = body.thinking ?? "high";
  if (typeof thinking !== "string" || !THINKING.has(thinking as Thinking)) {
    throw new RequestError(400, "thinking must be none, low, medium, high, xhigh, or max");
  }
  return { prompt: body.prompt.trim(), thinking: thinking as Thinking };
}

export function authorized(header: string | null, token: string): boolean {
  return token.length > 0 && header === `Bearer ${token}`;
}

export class RequestError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}
