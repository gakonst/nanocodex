import type { ComponentProps } from "react";
import type { AgentLifecycle, DefaultAgent } from "nanocodex";
import { Transport, type AgentStatus } from "nanocodex/browser";
import type { ManagedAgent } from "nanocodex/managed";
import {
  NanocodexProvider,
  createConfig,
  useNanocodex,
  useAgentEvents,
  useConfig,
  useVoice,
  type UseNanocodexReturnType,
  type UseVoiceReturnType,
} from "../index.mjs";
import {
  AgentController,
  useAgentController,
  type Agent,
  type AgentControllerSnapshot,
  type AgentEntry,
} from "../agent/index.mjs";
import {
  createConnectAgentSource,
  useConnectAgent,
  type ConnectAgentSourceOptions,
  type HostedConnectOptions,
} from "../cloud/index.mjs";
import type { ConnectAgent } from "nanocodex/connect";

const config = createConfig({
  agent: { transport: Transport.hostManaged(), thinking: "high", webMcp: true, harness: false },
  retry: 1,
});
const managedConfig = createConfig({
  agent: {
    transport: Transport.managed({ agent: { create: true } }),
    webMcp: true,
  },
});
const provider: ComponentProps<typeof NanocodexProvider> = { children: null, config };
void provider;
const snapshot = config.getAgent();
if (snapshot.status === "success") {
  const agent: DefaultAgent = snapshot.data;
  const error: undefined = snapshot.error;
  void agent;
  void error;
} else {
  const agent: undefined = snapshot.data;
  void agent;
}
// @ts-expect-error the application owns exactly one explicit Config lifecycle.
const missingConfig: ComponentProps<typeof NanocodexProvider> = { children: null };
void missingConfig;
// @ts-expect-error undefined does not transfer Config lifecycle ownership to the provider.
const undefinedConfig: ComponentProps<typeof NanocodexProvider> = { children: null, config: undefined };
void undefinedConfig;

function Consumer() {
  const resolved = useConfig();
  const result: UseNanocodexReturnType = useNanocodex({
    config: resolved,
    enabled: true,
    threadId: "thread-1",
  });
  useAgentEvents(result.data, (event) => event.seq, { includeAllSessions: true });
  result.refetch();
  return result.data;
}
void Consumer;

function ManagedConsumer() {
  const result = useNanocodex({ config: managedConfig });
  if (result.status !== "success") return result.status;
  const agent: AgentLifecycle = result.data;
  useAgentEvents(agent, (event) => event.seq);
  return agent.sessionId;
}
void ManagedConsumer;

function VoiceConsumer(agent: DefaultAgent | ManagedAgent | ConnectAgent | undefined) {
  const voice: UseVoiceReturnType = useVoice(agent, {
    beforeAgentTurn: async () => {},
    voice: "cove",
  });
  void voice.start({ voice: "juniper" });
  void voice.stop();
  void voice.cancel();
  // @ts-expect-error platform-only voices are not accepted by ChatGPT V3.
  void voice.start({ voice: "marin" });
  return voice.isActive ? voice.voice : voice.status;
}
void VoiceConsumer;

function SelectedConsumer() {
  const selectedStatus: AgentStatus = useNanocodex({
    selector: (resource) => resource.status,
    equalityFn(previous, next) {
      const previousStatus: AgentStatus = previous;
      const nextStatus: AgentStatus = next;
      return previousStatus === nextStatus;
    },
  });
  const sessionId: string | undefined = useNanocodex({
    selector: (resource) => resource.data?.sessionId,
  });
  const fullResource: UseNanocodexReturnType = useNanocodex({
    equalityFn: (previous, next) => previous.status === next.status,
  });
  return selectedStatus === "success" ? sessionId : fullResource.data?.sessionId;
}
void SelectedConsumer;

function narrowResource(resource: UseNanocodexReturnType) {
  if (resource.status === "success") {
    const data: DefaultAgent = resource.data;
    const error: undefined = resource.error;
    const isSuccess: true = resource.isSuccess;
    const isError: false = resource.isError;
    void data;
    void error;
    void isSuccess;
    void isError;
  } else {
    const data: undefined = resource.data;
    void data;
  }

  if (resource.isError) {
    const status: "error" = resource.status;
    const data: undefined = resource.data;
    const isIdle: false = resource.isIdle;
    void status;
    void data;
    void isIdle;
  }

  if (resource.isPending) {
    const status: "pending" = resource.status;
    const data: undefined = resource.data;
    const error: undefined = resource.error;
    void status;
    void data;
    void error;
  }
}
void narrowResource;

// @ts-expect-error function-backed transports require nanocodex/host and cannot configure the Worker store.
createConfig({ agent: { transport: Transport.hostManaged({ createWebSocket() { return {} as WebSocket; } }) } });

declare const structuralAgent: Agent | undefined;
declare const defaultAgent: DefaultAgent;
const normalizedDefaultAgent: Agent = defaultAgent;
void normalizedDefaultAgent;

declare const connectAgent: ConnectAgent;
void VoiceConsumer(connectAgent);
const connectSourceOptions: ConnectAgentSourceOptions = { history: false };
const normalizedConnectAgent: Agent = createConnectAgentSource(connectAgent, connectSourceOptions);
void normalizedConnectAgent;
// @ts-expect-error history visibility is an explicit, required privacy decision.
createConnectAgentSource(connectAgent);
// @ts-expect-error a Connect source cannot infer conversation-history authorization.
createConnectAgentSource(connectAgent, {});

const hostedConnectOptions: HostedConnectOptions = {
  capabilities: { agent: { conversationHistory: true } },
  mcpConnections: [{ id: "abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE", name: "Linear" }],
  permission: "agent.run",
};
void hostedConnectOptions;

function HostedConnectConsumer() {
  const connect = useConnectAgent({ reconnectOnMount: false });
  connect.connect({
    focusMcpConnectionId: "abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE",
    mcpConnections: [{ id: "abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE", name: "Linear" }],
    permission: "agent.run",
  });
  return connect.connectionStatus;
}
void HostedConnectConsumer;

const unsafeHostedConnectOptions: HostedConnectOptions = {
  mcpConnections: [{
    id: "abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE",
    name: "Linear",
    // @ts-expect-error MCP endpoints are broker-owned and never accepted from a host app.
    endpoint: "https://mcp.linear.app/mcp",
  }],
};
void unsafeHostedConnectOptions;

function HeadlessConversation() {
  const controller: AgentControllerSnapshot = useAgentController(structuralAgent, {
    maxEntries: 100,
    visible: true,
    onEvent(event) { event.type satisfies string; },
  });
  const entries: readonly AgentEntry[] = controller.entries;
  void controller.submit("root", { intent: "queue" });
  void controller.steer("adjust");
  void controller.cancel();
  void controller.loadOlder();
  controller.clear();
  controller.setVisible(false);
  return entries;
}
void HeadlessConversation;

const agentControllerProps: ComponentProps<typeof AgentController> = {
  agent: structuralAgent,
  children(controller) {
    return controller.entries.length;
  },
};
void agentControllerProps;

// @ts-expect-error presentation entries are immutable controller output.
useAgentController(structuralAgent).entries.push({ id: "x", kind: "error", text: "no" });
// @ts-expect-error only queue and steer are supported submission intents.
void useAgentController(structuralAgent).submit("x", { intent: "replace" });
