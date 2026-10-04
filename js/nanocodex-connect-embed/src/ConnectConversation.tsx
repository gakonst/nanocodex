"use client";

import { useMemo } from "react";
import type { ConnectAgent, Connection } from "nanocodex/connect";
import { createConnectAgentSource } from "nanocodex-react/connect";
import { AgentConversation, type AgentConversationProps } from "./conversation.js";

const noop = () => {};

export type ConnectConversationProps = Omit<AgentConversationProps,
  "agent" | "agentError" | "mode" | "onConversationActivity" | "onStateChange" | "retryAgent"
> & Partial<Pick<AgentConversationProps,
  "agentError" | "mode" | "onConversationActivity" | "onStateChange" | "retryAgent"
>> & Readonly<{
  agent: ConnectAgent;
  connection: Connection;
}>;

/** A ready-made conversation for an already approved Connect grant.
 * The transport enforces authorization. These presentation defaults also honor
 * the grant, and a changed grant remounts the transcript to discard old state.
 * No credentials or account access are created by rendering this component. */
export function ConnectConversation({
  agent, connection, showToolCalls = true, voice = false,
  agentError, mode = "full", onConversationActivity = noop,
  onStateChange = noop, retryAgent = noop, ...props
}: ConnectConversationProps) {
  const visibility = connection.grant.visibility;
  const connectionError = connection.grant.status !== "active"
    ? "This connection is no longer active. Reconnect to continue."
    : connection.agentId !== agent.id
      ? "This agent does not belong to the supplied connection."
      : undefined;
  const source = useMemo(() => connectionError ? undefined : createConnectAgentSource(agent, {
    history: visibility.conversationHistory,
  }), [agent, connection.grant.id, connectionError, visibility.conversationHistory]);
  const projectionKey = `${agent.sessionId}:${connection.grant.id}:${connectionError ?? "active"}:${JSON.stringify(visibility)}`;
  return <AgentConversation
    {...props}
    key={projectionKey}
    agent={source}
    agentError={connectionError ?? agentError}
    mode={mode}
    onConversationActivity={onConversationActivity}
    onStateChange={onStateChange}
    retryAgent={retryAgent}
    showToolCalls={showToolCalls && visibility.rawTraces}
    voice={voice && visibility.finalMessages}
  />;
}
