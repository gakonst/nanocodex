import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

import {
  type DefaultAgent,
  type EventWatcher,
} from "nanocodex";
import {
  importDurabilityState,
  type DurabilityPortableStateArchive,
} from "nanocodex/durability";
import { defineHook, getWorkflowMetadata, getWritable } from "workflow";

import type {
  PromptRequest,
  SessionEvent,
  TurnOutcome,
} from "@/lib/protocol";
import { errorMessage } from "@/lib/validation";
import {
  openApiKeyWebSocket,
  openSubscriptionWebSocket,
} from "./model-websocket";
import { postgresDurabilityStore } from "./postgres-durability";
import { vercelSandboxTools } from "./sandbox-tools";

const CHATGPT_WEBSOCKET_URL = "wss://chatgpt.com/backend-api/codex/responses";
const CHATGPT_API_BASE_URL = "https://chatgpt.com/backend-api/codex";
const wasmBytes = readFile(resolve(process.cwd(), "workflows/nanocodex.wasm"));

export const nanocodexPromptHook = defineHook<PromptRequest>();

export function promptHookToken(sessionId: string): string {
  return `nanocodex_actor:${sessionId}`;
}

export async function nanocodexActor(
  archive?: DurabilityPortableStateArchive,
): Promise<never> {
  "use workflow";

  const sessionId = getWorkflowMetadata().workflowRunId;
  const durabilityId = await prepareNanocodexDurability(sessionId, archive);
  const receivePrompt = nanocodexPromptHook.create({
    token: promptHookToken(sessionId),
  });
  const seen = new Set<string>();

  await writeSessionEvent({
    type: "ready",
    session_id: sessionId,
    restored: archive !== undefined,
  });

  for await (const request of receivePrompt) {
    await writeSessionEvent({
      type: "turn_accepted",
      id: request.id,
      input: request.input,
      replayed: seen.has(request.id),
    });
    const outcome = await runNanocodexTurn(sessionId, durabilityId, request);
    seen.add(request.id);
    if (!outcome.ok) {
      await writeSessionEvent({
        type: "turn_failed",
        id: request.id,
        error: outcome.error,
      });
      continue;
    }

    await writeSessionEvent(outcome.completed);
  }

  throw new Error("Nanocodex actor prompt hook closed unexpectedly");
}

export async function prepareNanocodexDurability(
  sessionId: string,
  archive?: DurabilityPortableStateArchive,
): Promise<string> {
  "use step";

  if (archive === undefined) return sessionId;
  await importDurabilityState(postgresDurabilityStore(), archive);
  return archive.stateId;
}

export async function writeSessionEvent(event: SessionEvent): Promise<void> {
  "use step";

  const writable = getWritable<SessionEvent>();
  const writer = writable.getWriter();
  try {
    await writer.write(event);
  } finally {
    writer.releaseLock();
  }
}

export async function runNanocodexTurn(
  sessionId: string,
  durabilityId: string,
  request: PromptRequest,
): Promise<TurnOutcome> {
  "use step";

  let agent: DefaultAgent | undefined;
  let events: EventWatcher | undefined;
  const writable = getWritable<SessionEvent>();
  const writer = writable.getWriter();
  let eventWrites = Promise.resolve();
  const durability = postgresDurabilityStore();

  try {
    const { Agent, Transport } = await import("nanocodex/host");
    const mode = modelAuthMode();
    const websocketUrl = process.env.OPENAI_WEBSOCKET_URL
      ?? (mode === "chatgpt" ? CHATGPT_WEBSOCKET_URL : undefined);
    const common = {
      instructions: "You are Nanocodex running as a durable Vercel Workflow actor. Use the sandbox_* tools for code, files, and previews; their /workspace is an isolated persistent Vercel Sandbox for this session.",
      module: await wasmBytes,
      durability,
      durabilityId,
      sessionId,
      toolMode: "direct" as const,
      tools: {
        ...vercelSandboxTools(sessionId),
        runtimeInfo: {
          description: "Return information about the current agent runtime.",
          parameters: { type: "object", additionalProperties: false },
          handler: () => ({
            runtime: "vercel-workflow",
            sandbox: "vercel-persistent-firecracker",
            session_id: sessionId,
            workspace: "/workspace",
          }),
        },
      },
      workspace: "/workspace",
    };

    agent = mode === "chatgpt"
      ? await Agent.create({
          ...common,
          transport: Transport.hostManaged({
            apiBaseUrl: CHATGPT_API_BASE_URL,
            websocketUrl,
            createWebSocket: openSubscriptionWebSocket,
          }),
        })
      : await Agent.create({
          ...common,
          transport: Transport.openAi({
            apiKey: requiredSecret("OPENAI_API_KEY"),
            websocketUrl,
            createWebSocket: openApiKeyWebSocket,
          }),
        });
    events = agent.events.watch();
    events.onEvent((event) => {
      eventWrites = eventWrites.then(() => writer.write({
        type: "event",
        turn_id: request.id,
        event,
      }));
    });

    const turn = agent.turn.prompt({ id: request.id, input: request.input });
    try {
      const result = await turn.result();
      try {
        await eventWrites;
        const usage = await result.usage();
        return {
          ok: true,
          completed: {
            type: "turn_completed",
            id: request.id,
            final_message: result.finalMessage,
            usage,
          },
        };
      } finally {
        result.dispose();
      }
    } finally {
      turn.dispose();
    }
  } catch (error) {
    await eventWrites.catch(() => {});
    return { ok: false, error: errorMessage(error) };
  } finally {
    events?.off();
    writer.releaseLock();
    if (agent) {
      try {
        await agent.session.shutdown();
      } catch {
        // The Rust total-state result or typed failure above is authoritative.
      } finally {
        agent.dispose();
      }
    }
  }
}

function modelAuthMode(): "api_key" | "chatgpt" {
  const mode = process.env.NANOCODEX_AUTH_MODE ?? "api_key";
  if (mode === "api_key" || mode === "chatgpt") return mode;
  throw new Error("NANOCODEX_AUTH_MODE must be api_key or chatgpt");
}

function requiredSecret(name: string): string {
  const value = process.env[name];
  if (!value?.trim()) throw new Error(`${name} is not configured`);
  return value;
}
