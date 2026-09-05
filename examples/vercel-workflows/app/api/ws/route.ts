import { experimental_upgradeWebSocket } from "@vercel/functions";
import type { WebSocket } from "ws";
import { getRun } from "workflow/api";

import type { SessionEvent, StreamRecord } from "@/lib/protocol";
import { parseSessionId, parseStartIndex } from "@/lib/validation";

export const runtime = "nodejs";
export const maxDuration = 300;

export function GET(request: Request): Response | Promise<Response> {
  const url = new URL(request.url);
  let sessionId: string;
  let startIndex: number;
  try {
    sessionId = parseSessionId(url.searchParams.get("sessionId"));
    startIndex = parseStartIndex(url.searchParams.get("startIndex"));
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return Response.json(
      { error: { code: "invalid_stream", message } },
      { status: 400, headers: { "cache-control": "no-store" } },
    );
  }

  return experimental_upgradeWebSocket((socket) => {
    let closed = false;
    let reader: ReadableStreamDefaultReader<SessionEvent> | undefined;
    const close = () => {
      if (closed) return;
      closed = true;
      void reader?.cancel("WebSocket closed").catch(() => {});
    };
    socket.on("close", close);
    socket.on("error", close);
    void pump(socket, sessionId, startIndex, (nextReader) => {
      reader = nextReader;
    }, () => closed).catch((error) => {
      send(socket, {
        type: "stream_error",
        message: error instanceof Error ? error.message : String(error),
      });
      if (socket.readyState === 1) socket.close(1011, "workflow stream failed");
    });
  });
}

async function pump(
  socket: WebSocket,
  sessionId: string,
  startIndex: number,
  setReader: (reader: ReadableStreamDefaultReader<SessionEvent>) => void,
  isClosed: () => boolean,
): Promise<void> {
  const source = getRun(sessionId).getReadable<SessionEvent>({ startIndex });
  const tailIndex = await source.getTailIndex();
  send(socket, {
    type: "stream_ready",
    session_id: sessionId,
    start_index: startIndex,
    tail_index: tailIndex,
  });
  const reader = source.getReader();
  setReader(reader);
  let index = startIndex;
  while (!isClosed()) {
    const next = await reader.read();
    if (next.done) {
      send(socket, { type: "stream_closed", next_index: index });
      if (socket.readyState === 1) socket.close(1000, "workflow stream closed");
      return;
    }
    const record: StreamRecord = {
      type: "stream_event",
      index,
      event: next.value,
    };
    index += 1;
    send(socket, record);
  }
}

function send(socket: WebSocket, message: unknown): void {
  if (socket.readyState !== 1) return;
  socket.send(JSON.stringify(message));
}
