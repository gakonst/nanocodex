import type { AttachmentClient, AttachmentTarget } from "nanocodex/tools";

export const HOSTED_ECHO_TOOL = "browser_echo";
export const DETACHED_REFUSAL_MARKER = "ATTACHED_TOOL_UNAVAILABLE";

export type HostedToolExecution = Readonly<{
  callId: string;
  generation: number;
  message: string;
  sessionId: string;
}>;

type HostedToolsConnector = Readonly<{
  connect(): Promise<AttachmentClient>;
  closed(): Promise<void>;
  close(): Promise<void>;
}>;

export type HostedToolsAttachment = Readonly<{
  client: AttachmentClient;
  connector: HostedToolsConnector;
}>;

export type HostedToolsRuntime = Readonly<{
  attach(target: AttachmentTarget): HostedToolsConnector;
}>;

export type HostedToolsAgent = Readonly<{
  id: string;
  toolsTarget(): AttachmentTarget;
  turn: Readonly<{
    prompt(options: Readonly<{ input: string }>): Readonly<{
      result(): Promise<Readonly<{ finalMessage: string }>>;
    }>;
  }>;
}>;

export async function publishHostedToolsCatalog(
  tools: HostedToolsRuntime,
  agent: HostedToolsAgent,
): Promise<HostedToolsAttachment> {
  const connector = tools.attach(agent.toolsTarget());
  try {
    const client = await connector.connect();
    if (!client.connected) throw new Error("the hosted tool catalog was acknowledged without a live attachment");
    return Object.freeze({ client, connector });
  } catch (error) {
    await connector.close().catch(() => {});
    throw error;
  }
}

export async function detachHostedToolsCatalog(
  attachment: HostedToolsAttachment,
): Promise<void> {
  await attachment.connector.close();
  await attachment.client.closed();
}

export async function runAttachedEcho(
  agent: HostedToolsAgent,
  message: string,
  executions: () => readonly HostedToolExecution[],
): Promise<Readonly<{ execution: HostedToolExecution; finalMessage: string }>> {
  const expected = message.trim();
  if (!expected) throw new Error("enter a message for the browser echo tool");
  const before = [...executions()];
  const turn = agent.turn.prompt({
    input: [
      `Find and call the deferred tool named ${HOSTED_ECHO_TOOL} exactly once.`,
      `Pass this exact message: ${JSON.stringify(expected)}.`,
      "Do not simulate the tool. After its result, briefly confirm what the browser returned.",
    ].join("\n"),
  });
  const result = await turn.result();
  const after = executions();
  const added = after.slice(before.length);
  if (added.length !== 1 || added[0]?.message !== expected) {
    throw new Error(`the managed turn did not execute ${HOSTED_ECHO_TOOL} exactly once in this browser`);
  }
  return Object.freeze({ execution: added[0], finalMessage: result.finalMessage });
}

export async function proveDetachedToolRefusal(
  agent: HostedToolsAgent,
  executions: () => readonly HostedToolExecution[],
): Promise<string> {
  const before = executions().length;
  const turn = agent.turn.prompt({
    input: [
      `Try to find and call the deferred tool named ${HOSTED_ECHO_TOOL}.`,
      `If it is unavailable, do not invent a result; reply with ${DETACHED_REFUSAL_MARKER}.`,
    ].join("\n"),
  });
  const result = await turn.result();
  if (executions().length !== before) {
    throw new Error("the browser echo handler ran after its catalog was detached");
  }
  if (!result.finalMessage.includes(DETACHED_REFUSAL_MARKER)) {
    throw new Error(`the managed agent did not confirm ${HOSTED_ECHO_TOOL} was unavailable`);
  }
  return result.finalMessage;
}

export async function replaceAndFenceHostedToolsCatalog(
  tools: HostedToolsRuntime,
  agent: HostedToolsAgent,
  current: HostedToolsAttachment,
  timeoutMs = 10_000,
): Promise<HostedToolsAttachment> {
  const successor = await publishHostedToolsCatalog(tools, agent);
  try {
    await bounded(
      current.client.closed(),
      timeoutMs,
      "the broker did not fence the stale browser tool host",
    );
    if (!successor.client.connected) {
      throw new Error("the successor browser tool host closed before fencing completed");
    }
    return successor;
  } catch (error) {
    await successor.connector.close().catch(() => {});
    throw error;
  }
}

async function bounded<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(message)), timeoutMs);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}
