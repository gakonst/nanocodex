import {
  memo,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  createConfig,
  useNanocodex,
} from "nanocodex-react";
import type { AgentControllerEvent } from "nanocodex-react/agent";
import type { ArtifactDocument } from "nanocodex/tools/artifact";
import {
  AgentTerminalView,
  type AgentTerminalMode,
  type AgentTerminalState,
} from "nanocodex-terminal";
import {
  inactiveTerminalMessage,
  type ModelSessionStatus,
  type CredentialSource,
} from "./modelSession";
import { ArtifactDock } from "./ArtifactDock";
import {
  ACCOUNT_MCP_CATALOG_CHANGED,
  browserMcpConfiguration,
  loadBrowserAccountMcpConnections,
  type BrowserAccountMcpConnection,
} from "./browserMcp";
import { clientFailureMessage } from "./clientFailure";
import { attachManagedBrowserHand } from "./managedBrowserHand";
import { managedTerminalAgent, openManagedAgent } from "./managedAgentRuntime";

export type { AgentTerminalMode, AgentTerminalState } from "nanocodex-terminal";
export { AgentTerminalView } from "nanocodex-terminal";

/** Authenticated website policy around the headless Agent SDK and shared transcript view. */
type AgentTerminalProps = Readonly<{
  authStatus: ModelSessionStatus | undefined;
  capabilityError?: string;
  composer?: ReactNode;
  enabled: boolean;
  mode: AgentTerminalMode;
  onConversationActivity(input: string): void;
  onTerminalEvent?(event: AgentControllerEvent): void;
  onStateChange(state: AgentTerminalState): void;
  source: CredentialSource | undefined;
  threadId: string;
  voiceEnabled: boolean;
  welcome?: string;
}>;

export const AgentTerminal = memo(function AgentTerminal(props: AgentTerminalProps) {
  const [accountMcpConnections, setAccountMcpConnections] =
    useState<readonly BrowserAccountMcpConnection[]>();
  const [catalogRevision, setCatalogRevision] = useState(0);
  const hasConversationActivity = useRef(false);
  const onConversationActivity = useCallback((input: string) => {
    hasConversationActivity.current = true;
    props.onConversationActivity(input);
  }, [props.onConversationActivity]);
  useEffect(() => {
    const refreshUnusedAgent = () => {
      if (!hasConversationActivity.current) setCatalogRevision((current) => current + 1);
    };
    window.addEventListener(ACCOUNT_MCP_CATALOG_CHANGED, refreshUnusedAgent);
    return () => window.removeEventListener(ACCOUNT_MCP_CATALOG_CHANGED, refreshUnusedAgent);
  }, []);
  useEffect(() => {
    if (!props.enabled) return;
    const controller = new AbortController();
    setAccountMcpConnections(undefined);
    void loadBrowserAccountMcpConnections(controller.signal).then(
      setAccountMcpConnections,
      (error) => {
        if (controller.signal.aborted) return;
        console.warn("nanocodex:account_mcp_listing_failed", {
          error: errorMessage(error),
        });
        setAccountMcpConnections((current) => current ?? []);
      },
    );
    return () => controller.abort();
  }, [catalogRevision, props.enabled]);
  return <BrowserAgentTerminal
    {...props}
    accountMcpConnections={accountMcpConnections ?? []}
    enabled={props.enabled && accountMcpConnections !== undefined}
    onConversationActivity={onConversationActivity}
  />;
});

const BrowserAgentTerminal = memo(function BrowserAgentTerminal({
  authStatus,
  accountMcpConnections,
  capabilityError,
  composer,
  enabled,
  mode,
  onConversationActivity,
  onTerminalEvent,
  onStateChange,
  source,
  threadId,
  voiceEnabled,
  welcome,
}: AgentTerminalProps & {
  accountMcpConnections: readonly BrowserAccountMcpConnection[];
}) {
  const agentConfig = useMemo(() => createConfig({
    agent: {
      mcp: browserMcpConfiguration(location.origin, threadId, accountMcpConnections),
      durability: false,
      ...(source === "sponsored" ? {
        model: "gpt-5.6-luna" as const,
        thinking: "none" as const,
        reasoningMode: "standard" as const,
        fastMode: false,
      } : {}),
    },
  }), [accountMcpConnections, source, threadId]);
  const {
    data: agent,
    error,
    isError,
    refetch,
  } = useNanocodex({ config: agentConfig, enabled, threadId });
  const refetchRef = useRef(refetch);
  refetchRef.current = refetch;
  const retryAgent = useCallback(() => {
    refetchRef.current();
  }, []);
  return (
    <AgentTerminalView
      agent={agent}
      agentError={isError ? errorMessage(error) : undefined}
      composer={composer}
      inactiveMessage={({ agentError, agentStatus }) => inactiveTerminalMessage({
        agentError,
        agentStatus,
        authStatus,
        capabilityError,
        source,
      })}
      mode={mode}
      onConversationActivity={onConversationActivity}
      onTerminalEvent={onTerminalEvent}
      onStateChange={onStateChange}
      retryAgent={retryAgent}
      voice={voiceEnabled}
      welcome={welcome}
      accessory={({ agentReady, submit }) => (
        <ArtifactDock
          agentReady={agentReady}
          onPrompt={(artifact, prompt, path) => submit(artifactFollowOnPrompt(artifact, path, prompt))}
        />
      )}
    />
  );
});

export const ManagedAgentTerminal = memo(function ManagedAgentTerminal({
  agentId,
  authStatus,
  mode,
  onConversationActivity,
  onStateChange,
  source,
  voiceEnabled,
}: {
  agentId: string;
  authStatus: ModelSessionStatus | undefined;
  mode: AgentTerminalMode;
  onConversationActivity(input: string): void;
  onStateChange(state: AgentTerminalState): void;
  source: Exclude<CredentialSource, null>;
  voiceEnabled: boolean;
}) {
  const managed = useMemo(() => openManagedAgent(agentId), [agentId]);
  const agent = useMemo(() => managedTerminalAgent(managed), [managed]);
  const [browserHand, setBrowserHand] = useState<Awaited<ReturnType<typeof attachManagedBrowserHand>>>();
  const [browserHandAttempt, setBrowserHandAttempt] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    let hand: Awaited<ReturnType<typeof attachManagedBrowserHand>> | undefined;
    let retry: ReturnType<typeof setTimeout> | undefined;
    const reconnect = () => {
      if (!controller.signal.aborted) {
        retry = setTimeout(() => setBrowserHandAttempt((current) => current + 1), 1_000);
      }
    };
    setBrowserHand(undefined);
    void attachManagedBrowserHand(managed, controller.signal).then((attached) => {
      if (controller.signal.aborted) {
        void attached.close();
        return;
      }
      hand = attached;
      setBrowserHand(attached);
      void hand.closed().then(() => {
        if (controller.signal.aborted) return;
        setBrowserHand(undefined);
        reconnect();
      });
    }).catch((error) => {
      if (controller.signal.aborted) return;
      console.warn("nanocodex:browser_hand_attach_failed", { error: errorMessage(error) });
      reconnect();
    });
    return () => {
      controller.abort();
      if (retry) clearTimeout(retry);
      if (hand) void hand.close();
    };
  }, [browserHandAttempt, managed]);
  const retryAgent = useCallback(() => {
    setBrowserHandAttempt((current) => current + 1);
  }, []);
  return (
    <AgentTerminalView
      agent={agent}
      agentError={undefined}
      inactiveMessage={({ agentError, agentStatus }) => inactiveTerminalMessage({
        agentError,
        agentStatus,
        authStatus,
        capabilityError: undefined,
        runtime: "managed",
        source,
      })}
      mode={mode}
      onConversationActivity={onConversationActivity}
      onStateChange={onStateChange}
      retryAgent={retryAgent}
      voice={voiceEnabled}
      accessory={({ agentReady, submit }) => browserHand ? (
        <ArtifactDock
          agentReady={agentReady}
          onPrompt={(artifact, prompt, path) => submit(artifactFollowOnPrompt(artifact, path, prompt))}
          workspace={browserHand.workspace}
          workspaceId={browserHand.workspaceId}
        />
      ) : null}
    />
  );
});

function artifactFollowOnPrompt(
  artifact: ArtifactDocument,
  path: string,
  prompt: string,
): string {
  return [
    `Continue the current artifact with id ${JSON.stringify(artifact.id)}.`,
    `Artifact path: ${JSON.stringify(path)}.`,
    "",
    prompt.trim(),
  ].join("\n");
}

function errorMessage(error: unknown): string {
  return clientFailureMessage(
    error,
    "The agent connection was interrupted. Check your network and retry.",
  );
}
