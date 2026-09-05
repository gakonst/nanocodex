import { getRun } from "workflow/api";

import type { SessionEvent, StreamRecord } from "@/lib/protocol";
import {
  errorResponse,
  parseSessionId,
  parseStartIndex,
} from "@/lib/validation";

export const runtime = "nodejs";
export const maxDuration = 300;

export async function GET(
  request: Request,
  context: { params: Promise<{ sessionId: string }> },
): Promise<Response> {
  try {
    const sessionId = parseSessionId((await context.params).sessionId);
    const startIndex = parseStartIndex(new URL(request.url).searchParams.get("startIndex"));
    const source = getRun(sessionId).getReadable<SessionEvent>({ startIndex });
    const tailIndex = await source.getTailIndex();
    const reader = source.getReader();
    const encoder = new TextEncoder();
    let index = startIndex;
    const body = new ReadableStream<Uint8Array>({
      async pull(controller) {
        try {
          const next = await reader.read();
          if (next.done) {
            controller.close();
            return;
          }
          const record: StreamRecord = {
            type: "stream_event",
            index,
            event: next.value,
          };
          index += 1;
          controller.enqueue(encoder.encode(`${JSON.stringify(record)}\n`));
        } catch (error) {
          controller.error(error);
        }
      },
      cancel(reason) {
        return reader.cancel(reason);
      },
    });
    return new Response(body, {
      headers: {
        "cache-control": "no-cache, no-store",
        "content-type": "application/x-ndjson; charset=utf-8",
        "x-content-type-options": "nosniff",
        "x-workflow-stream-tail-index": String(tailIndex),
      },
    });
  } catch (error) {
    return errorResponse(error);
  }
}
