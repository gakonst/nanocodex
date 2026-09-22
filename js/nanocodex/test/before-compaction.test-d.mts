import type { BeforeCompactionRequest, CompactionReceipt } from "../index.mjs";
import { Agent as NodeAgent, Transport } from "../node/index.mjs";
import { Agent as BrowserAgent } from "../browser/index.mjs";
import { Agent as HostAgent } from "../host/index.mjs";
import { Agent as CloudflareAgent } from "../cloudflare/index.mjs";

const beforeCompaction = async (request: BeforeCompactionRequest): Promise<CompactionReceipt> => {
  const boundaryId: string = request.boundaryId;
  const sessionId: string = request.sessionId;
  const rootSessionId: string = request.rootSessionId;
  const truncated: boolean = request.truncated;
  const signal: AbortSignal = request.signal;
  for (const message of request.messages) {
    const role: "user" | "assistant" = message.role;
    const text: string = message.text;
    // @ts-expect-error Message snapshots are immutable.
    message.text = "changed";
    void role; void text;
  }
  // @ts-expect-error Boundary identity is immutable.
  request.boundaryId = "changed";
  // @ts-expect-error The bounded transcript is immutable.
  request.messages.push({ role: "user", text: "changed" });
  // @ts-expect-error Callbacks receive an AbortSignal, not its controller.
  request.signal.abort();
  void sessionId; void rootSessionId; void truncated; void signal;
  return { receiptId: boundaryId };
};

const transport = Transport.openAi({ apiKey: "fixture" });
const nodeOptions: NodeAgent.create.Options = { transport, beforeCompaction };
const hostOptions: HostAgent.create.Options = { beforeCompaction };
const durableOptions: CloudflareAgent.create.Options = { beforeCompaction };
const ephemeralOptions: CloudflareAgent.createEphemeral.Options = { beforeCompaction };
const omitted: NodeAgent.create.Options = { transport };
const disabled: HostAgent.create.Options = { beforeCompaction: undefined };
void nodeOptions; void hostOptions; void durableOptions; void ephemeralOptions; void omitted; void disabled;

const invalidCallback: HostAgent.create.Options = {
  // @ts-expect-error A callback must acknowledge a durable receipt.
  beforeCompaction: async () => undefined,
};
const invalidReceipt: NodeAgent.create.Options = {
  transport,
  // @ts-expect-error Receipt identities are strings.
  beforeCompaction: async () => ({ receiptId: 123 }),
};
const invalidToggle: CloudflareAgent.createEphemeral.Options = {
  // @ts-expect-error Enabling the hook requires an actual callback.
  beforeCompaction: true,
};
const receipt: CompactionReceipt = { receiptId: "synthetic-commit" };
// @ts-expect-error Receipt values are immutable.
receipt.receiptId = "replacement";
void invalidCallback; void invalidReceipt; void invalidToggle;

const workerOptions: BrowserAgent.create.Options = {
  // @ts-expect-error Host callbacks cannot cross the browser Worker boundary.
  beforeCompaction,
};
void workerOptions;
