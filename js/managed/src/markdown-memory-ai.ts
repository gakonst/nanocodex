import { MarkdownMemoryError } from './markdown-memory';

// Verified against the official Workers AI model catalog, 2026-09-22:
// https://developers.cloudflare.com/workers-ai/models/llama-3.3-70b-instruct-fp8-fast/
export const MARKDOWN_MEMORY_MODEL = '@cf/meta/llama-3.3-70b-instruct-fp8-fast';
export interface MarkdownMemoryCompletionRequest {
  system: string;
  input: unknown;
  schema: Record<string, unknown>;
  signal?: AbortSignal;
}
export type MarkdownMemoryCompletion = (request: MarkdownMemoryCompletionRequest) => Promise<unknown>;
export interface MarkdownMemoryAi {
  run(model: string, input: Record<string, unknown>): Promise<unknown>;
}
const bytes = (value: string) => new TextEncoder().encode(value).length;
export function memoryAbortError(): MarkdownMemoryError {
  return new MarkdownMemoryError('memory inference was cancelled', 503, 'memory_inference_cancelled');
}
/** Also races bindings that do not support AbortSignal; late results never reach a writer. */
export async function boundedMemoryOperation<T>(operation: () => Promise<T>, signal?: AbortSignal, timeoutMs = 15_000): Promise<T> {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) throw new Error('invalid memory inference timeout');
  if (signal?.aborted) throw memoryAbortError();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let aborted: (() => void) | undefined;
  try {
    return await Promise.race([
      Promise.resolve().then(() => {
        if (signal?.aborted) throw memoryAbortError();
        return operation();
      }),
      new Promise<never>((_, reject) => {
        aborted = () => reject(memoryAbortError());
        signal?.addEventListener('abort', aborted, { once: true });
        timer = setTimeout(() => reject(new MarkdownMemoryError('memory inference timed out', 503, 'memory_inference_timeout')), timeoutMs);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
    if (aborted) signal?.removeEventListener('abort', aborted);
  }
}
export function createMarkdownMemoryCompletion(ai: MarkdownMemoryAi, options: { timeoutMs?: number } = {}): MarkdownMemoryCompletion {
  return async request => {
    const content = JSON.stringify(request.input);
    if (typeof content !== 'string' || bytes(content) + bytes(request.system) > 80_000 || bytes(JSON.stringify(request.schema)) > 8192)
      throw new MarkdownMemoryError('memory inference prompt is too large');
    const raw = await boundedMemoryOperation(() => ai.run(MARKDOWN_MEMORY_MODEL, {
      messages: [{ role: 'system', content: request.system }, { role: 'user', content }],
      response_format: { type: 'json_schema', json_schema: request.schema },
      temperature: 0, max_tokens: 2048, stream: false,
    }), request.signal, options.timeoutMs);
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) throw invalidOutput();
    const result = raw as Record<string, unknown>;
    if (result.tool_calls !== undefined && (!Array.isArray(result.tool_calls) || result.tool_calls.length)) throw invalidOutput();
    if (typeof result.response !== 'string' || bytes(result.response) > 16_384) throw invalidOutput();
    try { return JSON.parse(result.response); } catch { throw invalidOutput(); }
  };
}
function invalidOutput() {
  return new MarkdownMemoryError('invalid memory inference output', 502, 'memory_inference_invalid');
}
