import { AgentConversation, type AgentConversationProps, type AgentConversationStatus, ConnectConversation, TerminalComposer, GeneratedOutputView } from "nanocodex-connect-embed";
import { AgentEmbed, AgentProvider, AgentMessages, AgentComposer, AgentActivity, AgentStatus, useAgentEmbed } from "nanocodex-connect-embed/primitives";
import { createConnectAgentSource } from "nanocodex-connect-embed/connect";
import type { Agent, AgentEvent, AgentTurn } from "nanocodex-connect-embed/headless";
import type { AgentTerminalView } from "nanocodex-terminal";
import type { ComponentProps } from "react";

declare const source: Agent;
declare const existingProps: ComponentProps<typeof AgentTerminalView>;
const compatibilityProps: AgentConversationProps = existingProps;
const existingConsumer = <AgentConversation {...compatibilityProps} />;
const assembled = <AgentEmbed agent={source} theme="dark" composer={{ promptIntent: "queue" }} />;
const disconnected = <AgentEmbed agent={undefined} error="Connect first" retry={() => {}} />;
const composable = <AgentProvider agent={source} maxEntries={250} visible>
  <AgentStatus>{({ controller }) => controller.running ? "Working" : "Ready"}</AgentStatus>
  <AgentMessages showActivity={false} renderEntry={entry => "text" in entry ? entry.text : null} />
  <AgentActivity renderEntry={entry => entry.kind === "tool" ? entry.tool.name : "Plan"} />
  <AgentComposer draft="Hello" onDraftChange={value => { const s: string = value; void s; }} />
</AgentProvider>;
function CustomControl() { const { controller } = useAgentEmbed(); return <button onClick={() => { void controller.cancel(); }}>Stop</button>; }
const media = <GeneratedOutputView items={[{ kind: "image", url: "https://example.com/image.png" }]} />;
// @ts-expect-error theme choices are explicit
const badTheme = <AgentEmbed agent={source} theme="neon" />;
// @ts-expect-error normalized agents must expose turn and events lifecycle
const invalidSource = <AgentEmbed agent={{ sessionId: "missing-contract" }} />;
// @ts-expect-error history grants must be an explicit decision
createConnectAgentSource({});
void [existingConsumer, assembled, disconnected, composable, CustomControl, media, badTheme, invalidSource, TerminalComposer];

const status: AgentConversationStatus = "ready";
declare const event: AgentEvent;
declare const turn: AgentTurn;
void [status, event, turn, ConnectConversation];
