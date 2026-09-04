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
import type { ManagedCreateSettings } from "nanocodex/managed";
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

type Model = ManagedCreateSettings["model"];
type Thinking = ManagedCreateSettings["thinking"];
const MODELS: readonly Model[] = [
  "gpt-5.6-sol",
  "gpt-5.6-terra",
  "gpt-5.6-luna",
  "gpt-6-astra",
];

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
  const defaultSettings = terminalDefaultSettings(source, authStatus);
  const [settings, setSettings] = useState(defaultSettings);
  const [conversationStarted, setConversationStarted] = useState(false);
  const settingsIdentity = `${threadId}:${source ?? "none"}:${authStatus?.state === "ready" && authStatus.astraEntitled}`;
  useEffect(() => {
    setSettings(defaultSettings);
    setConversationStarted(false);
  }, [settingsIdentity]);
  const agentConfig = useMemo(() => createConfig({
    agent: {
      accountConnectionRequests: true,
      mcp: browserMcpConfiguration(location.origin, threadId, accountMcpConnections),
      durability: false,
      ...(source === "sponsored" ? {
        model: "gpt-5.6-luna" as const,
        thinking: "none" as const,
        reasoningMode: "standard" as const,
        fastMode: false,
      } : source === "brokered" && authStatus?.state === "ready" && authStatus.astraEntitled ? {
        model: "gpt-6-astra" as const,
        thinking: "high" as const,
        reasoningMode: "standard" as const,
        fastMode: false,
      } : {}),
    },
  }), [accountMcpConnections, authStatus, source, threadId]);
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
  const recordConversationActivity = useCallback((input: string) => {
    setConversationStarted(true);
    onConversationActivity(input);
  }, [onConversationActivity]);
  const updateModel = useCallback(async (model: Model) => {
    if (!agent || conversationStarted) return;
    const thinking = model === "gpt-6-astra" && settings.thinking === "none"
      ? "high"
      : settings.thinking;
    if (thinking !== settings.thinking) await agent.session.setThinking(thinking);
    await agent.session.setModel(model);
    setSettings((current) => ({ ...current, model, thinking }));
  }, [agent, conversationStarted, settings.thinking]);
  const updateThinking = useCallback(async (thinking: Thinking) => {
    if (!agent || (settings.model === "gpt-6-astra" && thinking === "none")) return;
    await agent.session.setThinking(thinking);
    setSettings((current) => ({ ...current, thinking }));
  }, [agent, settings.model]);
  const updateFastMode = useCallback(async (fastMode: boolean) => {
    if (!agent) return;
    await agent.session.setFastMode(fastMode);
    setSettings((current) => ({ ...current, fastMode }));
  }, [agent]);
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
      onConversationActivity={recordConversationActivity}
      onTerminalEvent={onTerminalEvent}
      onStateChange={onStateChange}
      retryAgent={retryAgent}
      voice={voiceEnabled}
      welcome={welcome}
      controls={source === "brokered" ? ({ agentReady }) => (
        <TerminalSettingsControls
          agentReady={agentReady}
          modelLocked={conversationStarted}
          settings={settings}
          onFastMode={updateFastMode}
          onModel={updateModel}
          onThinking={updateThinking}
        />
      ) : undefined}
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
  const [settings, setSettings] = useState<ManagedCreateSettings>(() => (
    terminalDefaultSettings(source, authStatus)
  ));
  const [settingsReady, setSettingsReady] = useState(false);
  const [conversationStarted, setConversationStarted] = useState(true);
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
  useEffect(() => {
    let active = true;
    setSettingsReady(false);
    void Promise.all([managed.state(), managed.settings.read()]).then(([state, current]) => {
      if (!active) return;
      setSettings(current);
      setConversationStarted(state.accepted_turns > 0);
      setSettingsReady(true);
    }).catch((error) => {
      if (!active) return;
      console.warn("nanocodex:managed_settings_failed", { error: errorMessage(error) });
    });
    return () => { active = false; };
  }, [managed]);
  const retryAgent = useCallback(() => {
    setBrowserHandAttempt((current) => current + 1);
  }, []);
  const recordConversationActivity = useCallback((input: string) => {
    setConversationStarted(true);
    onConversationActivity(input);
  }, [onConversationActivity]);
  const updateManagedSettings = useCallback(async (
    patch: Partial<ManagedCreateSettings>,
  ) => {
    const updated = await managed.settings.update(patch);
    setSettings(updated);
  }, [managed]);
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
      onConversationActivity={recordConversationActivity}
      onStateChange={onStateChange}
      retryAgent={retryAgent}
      voice={voiceEnabled}
      controls={({ agentReady }) => (
        <TerminalSettingsControls
          agentReady={agentReady && settingsReady}
          modelLocked={conversationStarted}
          settings={settings}
          onFastMode={(fastMode) => updateManagedSettings({ fastMode })}
          onModel={(model) => updateManagedSettings({
            model,
            ...(model === "gpt-6-astra" && settings.thinking === "none"
              ? { thinking: "high" }
              : {}),
          })}
          onThinking={(thinking) => updateManagedSettings({ thinking })}
        />
      )}
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

function terminalDefaultSettings(
  source: CredentialSource | undefined,
  authStatus: ModelSessionStatus | undefined,
): ManagedCreateSettings {
  if (source === "sponsored") {
    return { model: "gpt-5.6-luna", thinking: "none", reasoningMode: "standard", fastMode: false };
  }
  return {
    model: authStatus?.state === "ready" && authStatus.astraEntitled
      ? "gpt-6-astra"
      : "gpt-5.6-sol",
    thinking: "high",
    reasoningMode: "standard",
    fastMode: false,
  };
}

function TerminalSettingsControls({
  agentReady,
  modelLocked,
  settings,
  onFastMode,
  onModel,
  onThinking,
}: Readonly<{
  agentReady: boolean;
  modelLocked: boolean;
  settings: ManagedCreateSettings;
  onFastMode(enabled: boolean): Promise<unknown>;
  onModel(model: Model): Promise<unknown>;
  onThinking(thinking: Thinking): Promise<unknown>;
}>) {
  const [error, setError] = useState<string>();
  const run = (operation: Promise<unknown>) => {
    setError(undefined);
    void operation.catch((cause) => setError(errorMessage(cause)));
  };
  const thinking: readonly Thinking[] = ["none", "low", "medium", "high", "xhigh", "max"];
  return <div className="agent-runtime-controls" title={error}>
    <select
      aria-label="Model"
      disabled={!agentReady || modelLocked}
      value={settings.model}
      onChange={(event) => run(onModel(event.currentTarget.value as Model))}
    >
      {MODELS.map((model) => <option key={model} value={model}>{model.replace("gpt-5.6-", "").replace("gpt-6-", "")}</option>)}
    </select>
    <select
      aria-label="Thinking"
      disabled={!agentReady}
      value={settings.thinking}
      onChange={(event) => run(onThinking(event.currentTarget.value as Thinking))}
    >
      {thinking.map((effort) => <option
        key={effort}
        value={effort}
        disabled={settings.model === "gpt-6-astra" && effort === "none"}
      >{effort}</option>)}
    </select>
    <button
      aria-label="Fast mode"
      aria-pressed={settings.fastMode}
      className={settings.fastMode ? "is-active" : undefined}
      disabled={!agentReady}
      type="button"
      onClick={() => run(onFastMode(!settings.fastMode))}
    >fast</button>
  </div>;
}

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
