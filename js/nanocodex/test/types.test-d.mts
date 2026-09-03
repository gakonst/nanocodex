import {
  Actions,
  Agent,
  type AgentLifecycle,
  type AgentSessionContext,
  ChatGptSubscription,
  type AccountsWallet,
  type CostStatus,
  type LifecycleTurn,
  type LifecycleTurnResult,
  type McpServer,
  createTempoProviderFromAccounts,
  createMemoryChatGptSubscriptionStore,
  Subagents,
  type SessionSnapshot,
  type Tool,
  Transport,
  type Turn,
  type TurnResult,
  Workspace,
} from "../node/index.mjs";
import {
  Agent as BrowserAgent,
  Subagents as BrowserSubagents,
  Transport as BrowserTransport,
  Workspace as BrowserWorkspace,
} from "../browser/index.mjs";
import {
  Agent as HostAgent,
  type BrowserWebSocketRequest,
  type DefaultAgent,
  Transport as HostTransport,
} from "../host/index.mjs";
import { Client as ConnectClient, type McpConnection } from "../cloud/index.mjs";
import type * as RootPublicTypes from "../index.mjs";
import type * as BrowserPublicTypes from "../browser/index.mjs";
import type * as HostPublicTypes from "../host/index.mjs";
import type * as NodePublicTypes from "../node/index.mjs";
import {
  createTools,
  type HostedMachine,
  type Tools as ToolsCapability,
} from "../index.mjs";
import type { WorkspaceEntry as BrowserWorkspaceEntry } from "../browser/workspace.mjs";
import type { WorkspaceEntry as NodeWorkspaceEntry } from "../node/workspace.mjs";

const toolsCapability: ToolsCapability = await createTools();
const hostedMachine: HostedMachine = {
  id: "laptop",
  name: "Laptop",
  workspace: "/workspace",
  capabilities: ["filesystem"],
};
await createTools({ attachmentId: "laptop", machines: [hostedMachine] });
// @ts-expect-error Attachment identifiers are strings.
await createTools({ attachmentId: 1 });
void toolsCapability;
toolsCapability.attach("wss://managed.example/tools");
const attachmentClient = await toolsCapability.attach("wss://managed.example/tools").connect();
// @ts-expect-error Graceful drain owns the close code and reason.
attachmentClient.close(1000);
// @ts-expect-error Attachment policy is private; the public API accepts only a target.
toolsCapability.attach("wss://managed.example/tools", { reconnect: false });
// @ts-expect-error Tools is nominal and cannot be forged from lifecycle-shaped methods.
const forgedToolsCapability: ToolsCapability = { attach() {}, async close() {} };
void forgedToolsCapability;
import {
  createWorkerAgent,
  prepareWorkerAgent,
  type WorkerAgentOptions,
} from "../browser/WorkerAgent.mjs";
import {
  dataset,
  imageGeneration,
  type HostedMachine as ToolsHostedMachine,
  updatePlan,
  viewImage,
  web,
} from "../tools/index.mjs";
const toolsHostedMachine: ToolsHostedMachine = hostedMachine;
void toolsHostedMachine;
import {
  dataset as leafDataset,
  type DatasetOptions,
} from "../tools/dataset.mjs";
import { browser as browserTools } from "../tools/browser/index.mjs";
import {
  createMemoryDurabilityStore,
  DurabilityImportConflictError,
  durabilityRevision,
  durabilityStateDigest,
  exportDurabilityState,
  exportDurabilityStatePage,
  importDurabilityState,
  importDurabilityStatePages,
  type DurabilityAcquiredState,
  type DurabilityAcquireRequest,
  type DurabilityReplaceRequest,
  type DurabilityReplaceResult,
  type DurabilityFence,
  type DurabilityPortableStateArchive,
  type DurabilityPortableStatePage,
  type DurabilityPortableStore,
  type DurabilityRevision,
  type DurabilitySqliteQuery,
  type DurabilitySqliteRow,
  type DurabilitySqliteTransaction,
  type DurabilitySqliteValue,
  type DurabilityStore,
  type DurabilityStoredState,
  type MemoryDurabilityStore,
  type SqliteDurabilityStoreOptions,
} from "nanocodex/durability";
import {
  createPostgresDurabilityStore,
  type PostgresDurabilityClient,
  type PostgresDurabilityPool,
  type PostgresDurabilityQueryResult,
  PostgresDurabilityUnavailableError,
} from "../runtime/postgres-durability-store.mjs";
import {
  createCloudflareDurabilityStore,
  type CloudflareDurableObjectStorage,
} from "nanocodex/durability/cloudflare";
import {
  Agent as CloudflareAgent,
  cloudflareEgress,
} from "nanocodex/cloudflare";

declare const apiKey: string;
declare const accountsWallet: AccountsWallet;
declare const browserModule: WebAssembly.Module;
declare const postgresPool: PostgresDurabilityPool;
declare const cloudflareStorage: CloudflareDurableObjectStorage;
declare const cloudflareBinding: import("../cloudflare/egress.mjs").CloudflareEgressBinding;
declare const cloudflareContext: CloudflareAgent.DurableObjectContext;
declare const cloudflareOwner: CloudflareAgent.DurableObjectOwner;

// @ts-expect-error durability-only types are exported from nanocodex/durability.
type RootDurabilityStore = RootPublicTypes.DurabilityStore;
// @ts-expect-error durability-only types are exported from nanocodex/durability.
type BrowserDurabilityStore = BrowserPublicTypes.DurabilityStore;
// @ts-expect-error durability-only types are exported from nanocodex/durability.
type HostDurabilityStore = HostPublicTypes.DurabilityStore;
// @ts-expect-error durability-only types are exported from nanocodex/durability.
type NodeDurabilityStore = NodePublicTypes.DurabilityStore;

async function check() {
  const connectClient = ConnectClient.create({ appId: "typed-connect" });
  const hostedMcpConnection: McpConnection = {
    id: "abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE",
    name: "Linear workspace",
  };
  void connectClient.connection.connect({
    focusMcpConnectionId: hostedMcpConnection.id,
    mcpConnections: [hostedMcpConnection],
  });
  void connectClient.connection.reconnect({ mcpConnectionIds: [hostedMcpConnection.id] });
  void connectClient.connection.connect({
    mcpConnections: [{
      id: hostedMcpConnection.id,
      name: hostedMcpConnection.name,
      // @ts-expect-error hosted MCP endpoints are broker-owned, not host request metadata.
      endpoint: "https://mcp.linear.app/mcp",
    }],
  });
  const workerResource = {
    accountConnectionRequests: true,
    module: browserModule,
    origin: "https://example.com",
    sessionId: "session-1",
    threadId: "thread-1",
  } as const;
  await prepareWorkerAgent(workerResource);
  const workerOptions: WorkerAgentOptions = {
    onFailure(error) { error.message; },
  };
  await createWorkerAgent(workerResource, workerOptions);
  // @ts-expect-error non-disabled preparation requires one stable harness identity.
  await prepareWorkerAgent({ origin: "https://example.com" });
  const parallelTool: Tool = {
    description: "Parallel read-only fixture.",
    supportsParallelToolCalls: true,
    handler: async (_input, context) => context.model.length > 0 && context.signal.aborted,
  };
  const parallelMcp: McpServer = {
    url: "https://mcp.example.com",
    supportsParallelToolCalls: false,
    parallelTools: ["lookup"],
    isAvailable: () => true,
  };
  // @ts-expect-error per-tool parallel safety is boolean.
  parallelTool.supportsParallelToolCalls = "yes";
  // @ts-expect-error MCP parallel allowlists contain remote tool names.
  parallelMcp.parallelTools = [1];
  // @ts-expect-error MCP availability guards are synchronous boolean functions.
  parallelMcp.isAvailable = async () => true;
  const storedState: DurabilityStoredState = {
    revision: durabilityRevision(0n),
    payload: null,
  };
  const revision: DurabilityRevision = storedState.revision;
  const memoryStore: MemoryDurabilityStore = createMemoryDurabilityStore("typed-memory");
  const portableStore: DurabilityPortableStore = memoryStore;
  const portableArchive: DurabilityPortableStateArchive = await exportDurabilityState(
    portableStore,
    "typed-memory",
  );
  await importDurabilityState(portableStore, portableArchive);
  const portablePage: DurabilityPortableStatePage = await exportDurabilityStatePage(
    portableStore,
    "typed-memory",
    { from: durabilityRevision("0"), limit: 1024 },
  );
  const stateDigest: string = await durabilityStateDigest(storedState);
  await importDurabilityStatePages(createMemoryDurabilityStore("typed-memory"), [portablePage]);
  const importConflict: Error = new DurabilityImportConflictError("typed-memory");
  const sqliteValue: DurabilitySqliteValue = revision;
  const sqliteRow: DurabilitySqliteRow = { revision: sqliteValue };
  const sqliteQuery: DurabilitySqliteQuery = <Row extends DurabilitySqliteRow>() => [] as Row[];
  const sqliteTransaction: DurabilitySqliteTransaction = (callback) => callback(sqliteQuery);
  const sqliteOptions: SqliteDurabilityStoreOptions = { transaction: sqliteTransaction };
  void memoryStore;
  void importConflict;
  void stateDigest;
  void sqliteOptions;
  const durabilityStore: DurabilityStore = {
    load: () => storedState,
    acquire: (
      _stateId: string,
      request: DurabilityAcquireRequest,
    ): DurabilityAcquiredState => ({
      ...storedState,
      ownerId: request.ownerId,
      fence: "1" as DurabilityFence,
    }),
    replace: (_stateId: string, request: DurabilityReplaceRequest): DurabilityReplaceResult => ({
      status: "not_committed",
      message: `revision ${request.expectedRevision} was not committed`,
    }),
  };
  const acquired = await durabilityStore.acquire("typed-leaf", { ownerId: "typed-owner" });
  const fence: DurabilityFence = acquired.fence;
  await durabilityStore.replace("typed-leaf", {
    ownerId: acquired.ownerId,
    fence,
    expectedRevision: acquired.revision,
    payload: "opaque",
  });
  const postgresStore: DurabilityStore = createPostgresDurabilityStore(postgresPool);
  const cloudflareStore: DurabilityStore = createCloudflareDurabilityStore(cloudflareStorage);
  HostTransport.hostManaged(cloudflareEgress({
    binding: cloudflareBinding,
  }));
  cloudflareEgress({
    binding: cloudflareBinding,
    // @ts-expect-error broker subjects are derived privately from the Durable Object identity.
    subject: "caller-selected",
  });
  const cloudflareAgent: CloudflareAgent.Agent = await CloudflareAgent.create(cloudflareOwner);
  cloudflareAgent.turn.prompt({ input: "hello" });
  cloudflareAgent.events.connect(new Request("https://agent.internal/events"));
  const extendedCloudflareAgent = cloudflareAgent.extend(() => ({ application: true as const }));
  extendedCloudflareAgent.events.connect(new Request("https://agent.internal/events"));
  const cloudflareApplication: true = extendedCloudflareAgent.application;
  void cloudflareApplication;
  await CloudflareAgent.pruneDurableReceipts(cloudflareOwner, {
    terminalReceiptRetention: 0,
  });
  CloudflareAgent.destroy(cloudflareOwner);
  const ephemeralCloudflareAgent: DefaultAgent = await CloudflareAgent.createEphemeral(
    cloudflareOwner,
    {
      instructions: "Search the caller's account history.",
      model: "gpt-5.6-sol",
      tools: [{
        name: "search",
        description: "Search account history",
        handler: async () => [],
      }],
    },
  );
  ephemeralCloudflareAgent.turn.prompt({ input: "find a prior answer" });
  await CloudflareAgent.createEphemeral(cloudflareOwner, {
    // @ts-expect-error transport remains owned by the Cloudflare adapter.
    transport: HostTransport.hostManaged(),
  });
  await CloudflareAgent.create(cloudflareOwner, {
    // @ts-expect-error broker subjects are not caller-selected.
    subject: "caller-selected",
  });
  await CloudflareAgent.create(cloudflareOwner, {
    // @ts-expect-error provider credentials belong to the private EGRESS broker.
    apiKey,
  });
  await CloudflareAgent.create(cloudflareOwner, {
    // @ts-expect-error transport and durability are owned by the Cloudflare adapter.
    transport: HostTransport.hostManaged(),
  });
  await CloudflareAgent.create(cloudflareOwner, {
    tools: [...BrowserSubagents.create()],
  });
  await CloudflareAgent.create(cloudflareOwner, {
    // @ts-expect-error Cloudflare policy is fixed by the adapter.
    model: "gpt-5.6-sol",
  });
  await CloudflareAgent.create(cloudflareOwner, {
    // @ts-expect-error Cloudflare filesystem policy is fixed by the adapter.
    filesystem: BrowserWorkspace.memory(),
  });
  const postgresClient: PostgresDurabilityClient = await postgresPool.connect();
  const postgresResult: PostgresDurabilityQueryResult<{ revision: string }> =
    await postgresClient.query<{ revision: string }>(
      "SELECT revision::text AS revision",
    );
  postgresClient.release(true);
  new PostgresDurabilityUnavailableError("typed-leaf", new Error("connection closed"));
  void postgresStore;
  void postgresResult;
  void cloudflareStore;
  const datasetOptions: DatasetOptions = { fetch: globalThis.fetch };
  leafDataset(datasetOptions);
  const nodeWorkspace = await Workspace.open({ path: "/tmp/nanocodex" });
  await nodeWorkspace.writeFile("notes.txt", "hello");
  Workspace.tools(nodeWorkspace);
  const browserWorkspace = await BrowserWorkspace.open({ name: "notebook" });
  BrowserWorkspace.tools(browserWorkspace);
  const browserEntries: readonly BrowserWorkspaceEntry[] = await browserWorkspace.list();
  const nodeEntries: readonly NodeWorkspaceEntry[] = await nodeWorkspace.list();
  void browserEntries;
  void nodeEntries;

  const agent = await Agent.create({
    transport: Transport.openAi({ apiKey }),
    filesystem: nodeWorkspace,
    model: "gpt-6-astra",
    thinking: "high",
    fastMode: false,
    workspace: nodeWorkspace.root,
    tools: [...Subagents.create({ maxConcurrency: 8 })],
  });
  const child = await Subagents.spawn(agent, {
    role: "memory-search",
    task: "Search the available history tools.",
    model: "luna",
    thinking: "low",
    outputSchema: {
      type: "object",
      properties: { answer: { type: "string" } },
      required: ["answer"],
      additionalProperties: false,
    },
  });
  await Subagents.spawn(agent, {
    role: "accept-any-json",
    task: "Return any JSON value.",
    outputSchema: true,
  });
  const subagentBatch = await Subagents.spawnMany(agent, [
    {
      role: "planner",
      task: "Plan the work.",
      outputSchema: { type: "object", additionalProperties: false },
    },
    {
      role: "reviewer",
      task: "Review the work.",
      outputSchema: true,
    },
  ]);
  const batchChildId: number = subagentBatch[0]!.agent_id;
  void batchChildId;
  const localLifecycle: AgentLifecycle = agent;
  void localLifecycle;
  const childWait = await Subagents.wait(agent, {
    agentIds: [child.agent_id],
    timeoutMs: 30_000,
  });
  childWait.agents[0]?.status.state;
  const directory = await Subagents.list(agent, { includeCompleted: true });
  directory.agents[0]?.can_message;
  const receipt = await Subagents.send(agent, {
    agentId: child.agent_id,
    message: "Please include the source.",
    purpose: "question",
  });
  receipt.disposition;
  await Subagents.interrupt(agent, child.agent_id);
  await Subagents.close(agent, child.agent_id);
  await agent.session.compact();
  const sessionContext: AgentSessionContext = await agent.session.appendDeveloperMessage(
    "voice started",
  );
  const retainedContext: AgentSessionContext = await agent.session.context();
  const retainedActionContext: AgentSessionContext = await Actions.session.context(agent);
  sessionContext.history;
  const realtimeContext: AgentSessionContext = await agent.session.realtime.start();
  const realtimeDelegation: string = await agent.session.realtime.delegation("inspect the workspace", [
    { role: "user", text: "Please inspect it." },
  ]);
  const realtimeTail: string | undefined = await agent.session.realtime.tailDelegation([
    { role: "assistant", text: "I will hand this back." },
  ]);
  await agent.session.realtime.end();
  void realtimeContext;
  void realtimeDelegation;
  void realtimeTail;
  await agent.session.setFastMode(true);
  const options: Actions.turn.prompt.Options = { input: "hello" };
  const turn: Turn = agent.turn.prompt(options);
  const sameTurn: Actions.turn.prompt.ReturnType = Actions.turn.prompt(agent, options);
  const acceptedId: string | undefined = await turn.accepted();
  const sameAcceptedId: Actions.turn.accepted.ReturnType = await Actions.turn.accepted(sameTurn);
  const completed: TurnResult = await sameTurn.result();
  const sameResult: Actions.turn.getResult.ReturnType = completed;
  const message: string = completed.finalMessage;
  const snapshotPromise: Promise<SessionSnapshot> = completed.snapshot();
  const usagePromise: Promise<Actions.turn.getUsage.ReturnType> = completed.usage();
  const snapshot: Actions.turn.getSnapshot.ReturnType = await Actions.turn.getSnapshot(completed);
  const usage: Actions.turn.getUsage.ReturnType = await Actions.turn.getUsage(completed);
  usage.estimated_cost?.usd;
  const serviceTier: "standard" | "priority" | "fast" | undefined =
    usage.estimated_cost?.service_tier;
  const costStatus: CostStatus = usage.cost_status;
  void message;
  void acceptedId;
  void sameAcceptedId;
  void serviceTier;
  void sameResult;
  void snapshotPromise;
  void usagePromise;
  void usage;
  void costStatus;

  await Agent.create({ transport: Transport.openAi({ apiKey }), resume: snapshot });
  const tempoProvider = await createTempoProviderFromAccounts({
    wallet: accountsWallet,
    accessKey: "0x0000000000000000000000000000000000000001",
    policy: { maxDeposit: "0.05", topUpAmount: "0.05" },
    session: { bootstrap: true },
  });
  await Agent.create({ transport: Transport.mpp({ session: tempoProvider }), mcp: false });
  const subscription = await ChatGptSubscription.open({
    id: "account-1",
    store: createMemoryChatGptSubscriptionStore("account-1"),
  });
  await subscription.status();
  await Agent.create({ transport: Transport.chatGpt({ subscription }) });
  const managedTransport = Transport.managed({
    agent: { id: "0198d3f0-8844-7000-8000-000000000001" },
    baseUrl: "https://managed.example",
    apiKey,
    toolsTransport: (_target, _options) => ({
      readyState: 1,
      send() {},
      close() {},
      addEventListener() {},
    }),
  });
  const managedAgent = await Agent.create({
    transport: managedTransport,
    tools: toolsCapability,
  });
  const commonManagedLifecycle: AgentLifecycle = managedAgent;
  void commonManagedLifecycle;
  const managedTurn: LifecycleTurn = managedAgent.turn.prompt({ input: "hello" });
  const managedResult: LifecycleTurnResult = await managedTurn.result();
  await managedResult.usage();
  await managedAgent.session.shutdown();
  const browserManaged = await BrowserAgent.create({
    transport: BrowserTransport.managed({ agent: { create: true } }),
    tools: toolsCapability,
  });
  await browserManaged.session.shutdown();
  // @ts-expect-error managed identity is always explicit.
  Transport.managed({ baseUrl: "https://managed.example" });
  // @ts-expect-error create and open-existing identities are mutually exclusive.
  Transport.managed({ agent: { create: true, id: "0198d3f0-8844-7000-8000-000000000001" } });
  // @ts-expect-error managed service owns model policy.
  await Agent.create({ transport: managedTransport, model: "gpt-5.6-sol" });
  // @ts-expect-error managed transport accepts only the unified Tools recipe.
  await Agent.create({ transport: managedTransport, tools: { echo: { handler() {} } } });
  // @ts-expect-error authentication belongs to the selected transport.
  await Agent.create({ transport: Transport.openAi({ apiKey }), subscription });

  const fork = await Actions.session.fork(agent, { at: completed });
  fork.turn.prompt({ input: [{ type: "text", text: "continue" }] });
  // @ts-expect-error historical forks require a completed typed result.
  await Actions.session.fork(agent, { at: turn });
  // @ts-expect-error snapshots belong to completed results, not active turns.
  turn.snapshot();
  completed.dispose();

  const watch: Actions.events.watch.Watcher = agent.events.watch();
  watch.onEvent((event) => event.payload);
  for await (const event of watch) event.seq;
  watch.off();

  const extended = agent.extend((client) => ({
    inspect: { session: () => client.sessionId },
  }));
  extended.inspect.session();
  await agent.session.shutdown();
  await Actions.session.shutdown(agent);

  // @ts-expect-error function-backed transports require the current-isolate host.
  await BrowserAgent.create({ transport: BrowserTransport.hostManaged({ createWebSocket: () => ({} as WebSocket) }) });
  await HostAgent.create({
    transport: HostTransport.hostManaged({
      websocketUrl: "wss://example.com",
      createWebSocket: () => ({} as WebSocket),
    }),
  });
  const workerTransport: BrowserTransport.WorkerTransport = BrowserTransport.hostManaged({
    websocketUrl: "wss://example.com/api/responses",
  });
  const durability = createMemoryDurabilityStore("journal-1");
  await HostAgent.create({
    transport: HostTransport.openAi({ apiKey }),
    durability,
    durabilityId: "journal-1",
  });
  // @ts-expect-error durability and durabilityId are one required pair.
  await HostAgent.create({ transport: HostTransport.openAi({ apiKey }), durability });
  // @ts-expect-error durability and durabilityId are one required pair.
  await HostAgent.create({ transport: HostTransport.openAi({ apiKey }), durabilityId: "journal-1" });
  await HostAgent.create({
    transport: HostTransport.openAi({ apiKey }),
    durability,
    durabilityId: "journal-1",
    tools: [...BrowserSubagents.create()],
  });
  await Agent.create({
    transport: Transport.openAi({ apiKey }),
    durability,
    durabilityId: "journal-1",
    tools: [...Subagents.create()],
  });
  // @ts-expect-error a function-valued durability store cannot cross the package Worker boundary.
  await BrowserAgent.create({ transport: workerTransport, durability, durabilityId: "journal-1" });
  const socketRequest = {} as BrowserWebSocketRequest;
  if (socketRequest.authorization === "preconnect") socketRequest.turnState;
  await BrowserAgent.create({ transport: workerTransport });
  await HostAgent.create({
    transport: HostTransport.openAi({ apiKey }),
    codeEvaluator: async (_source, environment) => {
      environment.signal.throwIfAborted();
    },
    filesystem: browserWorkspace,
    tools: [
      web({ url: "https://example.com/tools/web" }),
      dataset(),
      imageGeneration({
        url: "https://example.com/tools/images",
        recentImages: () => [],
        rememberImage: () => {},
      }),
      viewImage({ workspace: browserWorkspace }),
      updatePlan(),
      ...BrowserSubagents.create(),
    ],
  });
  const browserRuntime = await browserTools({
    threadId: "thread-1",
    origin: "https://example.com",
    web: { url: "https://example.com/tools/web" },
    images: { url: "https://example.com/tools/images" },
    dataset: { fetch: globalThis.fetch },
    recentImages: () => [],
    rememberImage: () => {},
  });
  await HostAgent.create({
    transport: HostTransport.openAi({ apiKey }),
    filesystem: browserRuntime.filesystem,
    instructions: browserRuntime.instructions,
    tools: [...browserRuntime.tools, ...BrowserSubagents.create()],
  });
  // @ts-expect-error Rust extensions must come from a branded constructor.
  await Agent.create({ transport: Transport.openAi({ apiKey }), tools: [{ maxConcurrency: 8 }] });
  // @ts-expect-error function-backed MPP transports cannot cross the package Worker boundary.
  await BrowserAgent.create({ transport: BrowserTransport.mpp({ session: { async ws() { return {} as WebSocket; } } }) });
  await HostAgent.create({
    transport: HostTransport.mpp({ session: { async ws() { return {} as WebSocket; } } }),
  });
  // @ts-expect-error subscription handles cannot cross the package Worker boundary.
  await BrowserAgent.create({ transport: BrowserTransport.chatGpt({ subscription }) });
  await HostAgent.create({ transport: HostTransport.chatGpt({ subscription }) });
  // @ts-expect-error authentication is not an Agent.create option.
  await BrowserAgent.create({ transport: BrowserTransport.openAi({ apiKey }), hostAuth: true });
  // @ts-expect-error a transport cannot be fabricated from an arbitrary object.
  await BrowserAgent.create({ transport: { key: "fake", name: "fake", type: "fake", setup: () => ({}) } });
  await Agent.create({
    transport: Transport.mpp({ session: {
      async ws() {
        return {} as WebSocket;
      },
      async close() {},
    } }),
  });
  await Agent.create({
    transport: Transport.openAi({ apiKey }),
    module: new WebAssembly.Module(new Uint8Array()),
  });
  // @ts-expect-error transport queue policy is private to the adapter.
  await Agent.create({ transport: Transport.openAi({ apiKey }), maxQueuedMessages: 1 });
  // @ts-expect-error browser send-buffer policy is private to the adapter.
  await BrowserAgent.create({
    transport: BrowserTransport.openAi({ apiKey }),
    maxBufferedSendBytes: 1,
  });

  const rolloutSnapshot: SessionSnapshot = {
    version: 1,
    model: "gpt-5.6-sol",
    lineage_id: "thread",
    prompt_cache_key: "thread",
    workspace: "/tmp",
    canonical_context: {
      type: "message",
      role: "user",
      content: [{ type: "input_text", text: "hello" }],
    },
    history: [],
  };
  await Agent.create({
    transport: Transport.openAi({ apiKey }),
    resume: rolloutSnapshot,
  });

  // @ts-expect-error actions are domain-grouped on the decorated Agent.
  agent.prompt("hello");
  // @ts-expect-error prompt accepts a named options bag.
  agent.turn.prompt("hello");
}

void check;
