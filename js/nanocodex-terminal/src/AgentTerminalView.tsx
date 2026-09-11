"use client";

import {
  type ReactNode,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import {
  useAgentController,
  type Agent,
  type AgentControllerEvent,
} from "nanocodex-react/agent";
import {
  useVoice,
  Voice,
  type UseVoiceParameters,
  type UseVoiceReturnType,
} from "nanocodex-react";
import { SlidersHorizontal, X } from "lucide-react";
import { TerminalComposer } from "./TerminalComposer.js";
import { TerminalTranscriptSurface } from "./TerminalTranscriptSurface.js";
import type { VoiceTerminalEntry } from "./TerminalTranscriptSurface.js";
import type {
  AgentStatus,
  AgentTerminalMode,
  AgentTerminalState,
} from "./types.js";

export type AgentTerminalAccessory = Readonly<{
  agentReady: boolean;
  submit(input: string): void;
}>;

/** Shared website terminal presentation. Runtime and authorization policy stay with its consumer. */
export function AgentTerminalView({
  accessory,
  agent,
  agentError,
  composer,
  composerPlaceholder,
  controls,
  inactiveMessage,
  maxEntries,
  mode,
  onConversationActivity,
  onTerminalEvent,
  onStateChange,
  promptIntent,
  retryAgent,
  showToolCalls = true,
  voice = false,
  voiceOptions,
  welcome,
}: {
  accessory?(controls: AgentTerminalAccessory): ReactNode;
  agent: Agent | undefined;
  agentError: string | undefined;
  /** Replaces the default composer without detaching the transcript controller. */
  composer?: ReactNode;
  composerPlaceholder?: string;
  controls?(controls: Pick<AgentTerminalAccessory, "agentReady">): ReactNode;
  inactiveMessage?(state: Readonly<{
    agentError: string | undefined;
    agentStatus: AgentStatus;
  }>): string | undefined;
  maxEntries?: number;
  mode: AgentTerminalMode;
  onConversationActivity(input: string): void;
  onTerminalEvent?(event: AgentControllerEvent): void;
  onStateChange(state: AgentTerminalState): void;
  promptIntent?: "queue" | "steer";
  retryAgent(): void;
  showToolCalls?: boolean;
  /** Enables the package-owned microphone control. */
  voice?: boolean;
  voiceOptions?: Omit<UseVoiceParameters, "enabled">;
  welcome?: string;
}) {
  const [touchDraft, setTouchDraft] = useState("");
  const [pendingTouchSubmission, setPendingTouchSubmission] = useState<{
    input: string;
    submittedAt: number;
  }>();
  const [followTailRequest, setFollowTailRequest] = useState(0);
  const [readySessionId, setReadySessionId] = useState<string>();
  const [voiceEntries, setVoiceEntries] = useState<readonly VoiceTerminalEntry[]>([]);
  const submittedPrompts = useRef<Array<{ input: string; submittedAt: number }>>([]);
  const pendingRootPrompts = useRef<PromptTiming[]>([]);
  const currentRootPrompt = useRef<PromptTiming | undefined>(undefined);
  const voiceEntrySequence = useRef(0);
  const handleControllerEvent = useCallback((event: AgentControllerEvent) => {
    const observedEvent = observeControllerTiming({
      agentSessionId: agent?.sessionId,
      currentRootPrompt,
      event,
      pendingRootPrompts,
      submittedPrompts,
      onFirstOutput(firstOutput) {
        onTerminalEvent?.(firstOutput);
        const timingContext = {
          eventSeq: firstOutput.eventSeq,
          promptId: firstOutput.id,
          sessionId: firstOutput.sessionId,
        };
        markAgentTiming(
          "prompt.submit_to_first_token",
          Math.max(0, firstOutput.timestamp - firstOutput.submittedAt),
          timingContext,
        );
        markAgentTiming(
          "prompt.run_started_to_first_token",
          Math.max(0, firstOutput.timestamp - firstOutput.runStartedAt),
          timingContext,
        );
      },
    });
    onTerminalEvent?.(observedEvent);
    if (observedEvent.type === "controller.attached"
      && typeof observedEvent.sessionId === "string") {
      submittedPrompts.current.length = 0;
      pendingRootPrompts.current.length = 0;
      currentRootPrompt.current = undefined;
      setReadySessionId(observedEvent.sessionId);
      markAgentTiming("terminal.ready");
    } else if (observedEvent.type === "controller.detached"
      && typeof observedEvent.sessionId === "string") {
      setReadySessionId((current) => current === observedEvent.sessionId ? undefined : current);
    } else if (observedEvent.type === "prompt.accepted"
      && typeof observedEvent.input === "string") {
      onConversationActivity(observedEvent.input);
      markAgentTiming("prompt.accepted");
    }
  }, [agent?.sessionId, onConversationActivity, onTerminalEvent]);
  const controller = useAgentController(agent, {
    maxEntries,
    visible: mode !== "hidden",
    onEvent: handleControllerEvent,
  });
  const voiceState = useVoice(
    agent?.voiceSource ?? (agent as Parameters<typeof useVoice>[0]),
    { ...voiceOptions, enabled: voice && mode !== "hidden" },
  );
  const maxVoiceEntries = Number.isSafeInteger(maxEntries) && (maxEntries ?? 0) > 0
    ? maxEntries!
    : 200;
  const agentStatus: AgentStatus = agentError
    ? "error"
    : agent && readySessionId === agent.sessionId
      ? "ready"
      : "starting";
  const terminalRunning = agentStatus === "ready"
    && (controller.running || controller.pendingTurns > 0);

  useEffect(() => {
    setVoiceEntries([]);
    voiceEntrySequence.current = 0;
  }, [agent?.sessionId]);

  useEffect(() => {
    const transcripts = voiceState.transcripts;
    if (transcripts.length === 0) return;
    const afterEntryId = controller.entries.at(-1)?.id;
    setVoiceEntries((current) => {
      const rows = [...current];
      for (const transcript of transcripts) {
        const id = `voice-${agent?.sessionId ?? "detached"}-${transcript.id ?? voiceEntrySequence.current++}`;
        const index = rows.findIndex((entry) => entry.id === id);
        const entry: VoiceTerminalEntry = { afterEntryId: index < 0 ? afterEntryId : rows[index]!.afterEntryId,
          id, kind: transcript.speaker, source: "voice", streaming: transcript.isPartial === true, text: transcript.text };
        if (index < 0) rows.push(entry); else rows[index] = entry;
      }
      return rows.slice(-maxVoiceEntries);
    });
    setFollowTailRequest((current) => current + 1);
  }, [agent?.sessionId, maxVoiceEntries, voiceState.transcripts]);

  useEffect(() => {
    onStateChange({ error: agentError, retry: retryAgent, status: agentStatus });
  }, [agentError, agentStatus, onStateChange, retryAgent]);

  const unavailableMessage = inactiveMessage?.({ agentError, agentStatus });
  const submitTouchPrompt = useCallback((input: string) => {
    if (!input.trim()) return;
    const submittedAt = performance.now();
    setFollowTailRequest((current) => current + 1);
    if (agentStatus !== "ready") {
      setPendingTouchSubmission({ input, submittedAt });
      return;
    }
    void voiceState.noteTypedInput().then(() => submitPrompt(controller, submittedPrompts.current, input, submittedAt, promptIntent));
    setTouchDraft("");
  }, [agentStatus, controller, promptIntent, voiceState.noteTypedInput]);
  useEffect(() => {
    if (agentStatus !== "ready" || !pendingTouchSubmission) return;
    void voiceState.noteTypedInput().then(() => submitPrompt(
      controller,
      submittedPrompts.current,
      pendingTouchSubmission.input,
      pendingTouchSubmission.submittedAt,
      promptIntent,
    ));
    setPendingTouchSubmission(undefined);
    setTouchDraft("");
  }, [agentStatus, controller, pendingTouchSubmission, promptIntent, voiceState.noteTypedInput]);
  const cancelTouchTurn = useCallback(() => {
    if (agentStatus === "ready") void voiceState.noteTypedInput().then(() => controller.cancel());
  }, [agentStatus, controller, voiceState.noteTypedInput]);
  const submitAccessoryPrompt = useCallback((input: string) => {
    if (agentStatus !== "ready") return;
    const submittedAt = performance.now();
    setFollowTailRequest((current) => current + 1);
    retainSubmittedPrompt(submittedPrompts.current, input, submittedAt);
    void voiceState.noteTypedInput().then(() => controller.submit(input, { intent: "queue" }));
  }, [agentStatus, controller, voiceState.noteTypedInput]);

  const terminal = (
    <TerminalTranscriptSurface
      composer={composer === undefined ? (
        <TerminalComposer
          controls={(voice || controls) ? <>
            {voice ? <VoiceControl agentReady={agentStatus === "ready"} voice={voiceState} initialSettings={voiceOptions} /> : null}
            {controls?.({ agentReady: agentStatus === "ready" })}
          </> : undefined}
          draft={touchDraft}
          placeholder={composerPlaceholder}
          pending={pendingTouchSubmission !== undefined}
          running={terminalRunning}
          status={agentStatus}
          onCancel={cancelTouchTurn}
          onChange={(value) => {
            setPendingTouchSubmission(undefined);
            setTouchDraft(value);
          }}
          onSubmit={submitTouchPrompt}
        />
      ) : composer}
      canLoadOlder={controller.canLoadOlder}
      entries={controller.entries}
      followTailRequest={followTailRequest}
      inactiveMessage={unavailableMessage ?? ""}
      isLoadingOlder={controller.isLoadingOlder}
      mode={mode}
      showToolCalls={showToolCalls}
      status={agentStatus}
      voiceEntries={voiceEntries}
      welcome={welcome}
      onLoadOlder={controller.loadOlder}
    />
  );

  // A retained full terminal keeps its transcript and artifact frame mounted
  // while its owning route is hidden, preserving scroll and artifact state.
  return mode !== "preview" ? (
    <div className="agent-terminal-workspace">
      {terminal}
      {accessory?.({ agentReady: agentStatus === "ready", submit: submitAccessoryPrompt })}
    </div>
  ) : terminal;
}

export function VoiceControl({
  agentReady,
  voice,
  initialVoice,
  initialSettings,
}: {
  agentReady: boolean;
  voice: UseVoiceReturnType;
  initialVoice?: NonNullable<UseVoiceReturnType["voice"]> | undefined;
  initialSettings?: Voice.Settings | undefined;
}) {
  const engaged = voice.isActive || voice.isConnecting;
  const [settings, setSettings] = useState<Voice.Settings>(() => ({ ...savedVoiceSettings(), ...initialSettings }));
  const [selectedVoice, setSelectedVoice] = useState(voice.voice ?? initialVoice ?? settings.voice ?? Voice.defaultVoice);
  const [showSettings, setShowSettings] = useState(false);
  const [settingsError, setSettingsError] = useState<string>();
  const [applying, setApplying] = useState(false);
  const statusText = voice.statusText ?? (voice.isActive ? voice.voice : undefined);
  const saveSettings = async () => {
    const next = { ...settings, voice: selectedVoice };
    if (next.instructions?.includes("\0")) {
      setSettingsError("Speaking preferences cannot contain NUL characters.");
      return;
    }
    setApplying(true);
    setSettingsError(undefined);
    try {
      try { globalThis.localStorage?.setItem("nanocodex.voice.settings", JSON.stringify(next)); } catch { /* Storage may be disabled. */ }
      if (engaged) { await voice.stop(); await voice.start(next); }
      setShowSettings(false);
    } catch (error) {
      setSettingsError(error instanceof Error ? error.message : String(error));
    } finally { setApplying(false); }
  };
  return <>
    <button
      className="agent-voice-button"
      type="button"
      aria-label={engaged ? "Stop voice" : "Start voice"}
      aria-pressed={engaged}
      disabled={!agentReady}
      onClick={() => { void voice.toggle({ ...settings, voice: selectedVoice }).catch(() => {}); }}
    >
      <svg aria-hidden="true" viewBox="0 0 24 24">
        <path d="M12 15a3 3 0 0 0 3-3V6a3 3 0 1 0-6 0v6a3 3 0 0 0 3 3Zm-7-3a1 1 0 1 1 2 0 5 5 0 0 0 10 0 1 1 0 1 1 2 0 7 7 0 0 1-6 6.92V21h3a1 1 0 1 1 0 2H8a1 1 0 1 1 0-2h3v-2.08A7 7 0 0 1 5 12Z" />
      </svg>
      <span className="agent-terminal-sr-only">Voice</span>
    </button>
    {engaged ? <>
      <button type="button" className="agent-voice-mute-button" aria-label={voice.muted ? "Unmute microphone" : "Mute microphone"}
        aria-pressed={voice.muted} onClick={() => voice.toggleMuted()}>{voice.muted ? "Unmute" : "Mute"}</button>
      <meter className="agent-voice-level" aria-label="Microphone level" min={0} max={1} value={voice.microphoneLevel} />
      <meter className="agent-voice-level" aria-label="Speaker level" min={0} max={1} value={voice.speakerLevel} />
    </> : null}
    <select
      aria-label="Voice"
      className="agent-voice-select"
      value={selectedVoice}
      disabled={engaged}
      onChange={(event) => { setSelectedVoice(event.target.value as NonNullable<UseVoiceReturnType["voice"]>); }}
    >
      {Voice.voices.map((name) => <option key={name} value={name}>
        {name[0]!.toUpperCase() + name.slice(1)}
      </option>)}
    </select>
    <div className="agent-voice-preferences">
      <button type="button" aria-label="Voice settings" aria-expanded={showSettings}
        onClick={() => { setShowSettings(!showSettings); }}>
        <SlidersHorizontal aria-hidden="true" />
      </button>
      {showSettings ? <div className="agent-voice-settings" role="group" aria-label="Voice preferences">
        <label>Voice for this call<select value={selectedVoice} onChange={(event) => {
          setSelectedVoice(event.target.value as Voice.VoiceName);
        }}>
          {Voice.voices.map((name) => <option key={name} value={name}>{name[0]!.toUpperCase() + name.slice(1)}</option>)}
        </select></label>
        <label>Pace<select value={settings.pace ?? "natural"} onChange={(event) => {
          setSettings({ ...settings, pace: event.target.value as Voice.Settings["pace"] });
        }}>
          <option value="slow">Relaxed</option><option value="natural">Natural</option><option value="fast">Brisk</option>
        </select></label>
        <label>Spoken updates<select value={settings.updates ?? "auto"} onChange={(event) => {
          setSettings({ ...settings, updates: event.target.value as Voice.Settings["updates"] });
        }}>
          <option value="auto">As useful</option><option value="results">Results and blockers</option><option value="silent">Only when asked</option>
        </select></label>
        <label>Acknowledge requests<select value={settings.acknowledgements === undefined ? "auto" : String(settings.acknowledgements)} onChange={(event) => {
          setSettings({ ...settings, acknowledgements: event.target.value === "auto" ? undefined : event.target.value === "true" });
        }}>
          <option value="auto">Automatic</option><option value="true">On</option><option value="false">Off</option>
        </select></label>
        <label>Speaking style<textarea value={settings.instructions ?? ""} rows={3}
          placeholder="Keep answers short and speak Greek unless I ask otherwise."
          onChange={(event) => { setSettings({ ...settings, instructions: event.target.value }); }} /></label>
        {settingsError ? <span role="alert">{settingsError}</span> : null}
        {voice.isActive ? <button type="button" onClick={() => {
          void voice.speak("Voice is connected. You should hear this sentence.").catch((error: unknown) => {
            setSettingsError(error instanceof Error ? error.message : String(error));
          });
        }}>Test voice</button> : null}
        <button type="button" aria-label="Save voice settings" disabled={applying} onClick={() => { void saveSettings(); }}>
          {applying ? "Applying…" : engaged ? "Apply and reconnect" : "Save"}
        </button>
      </div> : null}
    </div>
    {voice.isActive ? (
      <button
        className="agent-voice-cancel-button"
        type="button"
        aria-label="Cancel voice turn"
        onClick={() => { void voice.cancel().catch(() => {}); }}
      >
        <X aria-hidden="true" />
      </button>
    ) : null}
    {statusText || voice.isError ? (
      <div className="agent-voice-feedback">
        {statusText && !voice.isError ? (
          <span className="agent-voice-status" role="status" aria-live="polite">
            {statusText}
          </span>
        ) : null}
        {voice.isError ? (
          <span className="agent-voice-error" role="alert">
            {voice.error?.message ?? "Voice failed. Check microphone access and retry."}
          </span>
        ) : null}
      </div>
    ) : null}
  </>;
}

function savedVoiceSettings(): Voice.Settings {
  try {
    const value: unknown = JSON.parse(globalThis.localStorage?.getItem("nanocodex.voice.settings") ?? "{}");
    if (!value || typeof value !== "object" || Array.isArray(value)) return {};
    const settings = value as Voice.Settings;
    return {
      voice: Voice.voices.includes(settings.voice!) ? settings.voice : undefined,
      instructions: typeof settings.instructions === "string"
        && !settings.instructions.includes("\0") ? settings.instructions : undefined,
      pace: ["slow", "natural", "fast"].includes(settings.pace!) ? settings.pace : undefined,
      updates: ["auto", "results", "silent"].includes(settings.updates!) ? settings.updates : undefined,
      acknowledgements: typeof settings.acknowledgements === "boolean" ? settings.acknowledgements : undefined,
    };
  } catch { return {}; }
}

type PromptTiming = {
  firstOutputReported: boolean;
  id: number;
  runStartedAt?: number;
  submittedAt: number;
};

type FirstOutputEvent = AgentControllerEvent & Readonly<{
  eventSeq: number;
  id: number;
  runStartedAt: number;
  sessionId: string;
  submittedAt: number;
}>;

function submitPrompt(
  controller: ReturnType<typeof useAgentController>,
  submittedPrompts: Array<{ input: string; submittedAt: number }>,
  input: string,
  submittedAt: number,
  intent?: "queue" | "steer",
) {
  retainSubmittedPrompt(submittedPrompts, input, submittedAt);
  void controller.submit(input, intent === undefined ? undefined : { intent });
}

function retainSubmittedPrompt(
  submissions: Array<{ input: string; submittedAt: number }>,
  input: string,
  submittedAt: number,
) {
  const prompt = input.trim();
  if (!prompt || prompt === "/clear" || prompt === "/cancel" || prompt === "/exit") return;
  submissions.push({ input: prompt, submittedAt });
}

function observeControllerTiming({
  agentSessionId,
  currentRootPrompt,
  event,
  onFirstOutput,
  pendingRootPrompts,
  submittedPrompts,
}: {
  agentSessionId: string | undefined;
  currentRootPrompt: { current: PromptTiming | undefined };
  event: AgentControllerEvent;
  onFirstOutput(event: FirstOutputEvent): void;
  pendingRootPrompts: { current: PromptTiming[] };
  submittedPrompts: { current: Array<{ input: string; submittedAt: number }> };
}): AgentControllerEvent {
  if (event.type === "prompt.accepted"
    && typeof event.id === "number"
    && typeof event.input === "string") {
    const submittedAt = claimSubmittedAt(submittedPrompts.current, event.input, event.timestamp);
    pendingRootPrompts.current.push({
      firstOutputReported: false,
      id: event.id,
      submittedAt,
    });
    return { ...event, submittedAt };
  }
  if ((event.type === "prompt.steered" || event.type === "prompt.steer_error")
    && typeof event.input === "string") {
    claimSubmittedAt(submittedPrompts.current, event.input, event.timestamp);
  }
  if ((event.type === "prompt.completed" || event.type === "prompt.failed")
    && typeof event.id === "number") {
    const pendingIndex = pendingRootPrompts.current.findIndex((timing) => timing.id === event.id);
    if (pendingIndex >= 0) pendingRootPrompts.current.splice(pendingIndex, 1);
    if (currentRootPrompt.current?.id === event.id) currentRootPrompt.current = undefined;
  }
  if (event.type === "prompt.rejected" && typeof event.input === "string") {
    claimSubmittedAt(submittedPrompts.current, event.input, event.timestamp);
  }
  if (event.type !== "agent.event" || !isObservedAgentEvent(event.event, agentSessionId)) {
    return event;
  }
  const agentEvent = event.event;
  if (agentEvent.type === "run.started") {
    const timing = pendingRootPrompts.current.shift();
    if (timing) timing.runStartedAt = event.timestamp;
    currentRootPrompt.current = timing;
  } else if (agentEvent.type === "run.completed" || agentEvent.type === "run.failed") {
    currentRootPrompt.current = undefined;
  } else if ((agentEvent.type === "assistant.delta" || agentEvent.type === "reasoning.summary.delta")
    && typeof agentEvent.payload.text === "string"
    && agentEvent.payload.text.length > 0) {
    const timing = currentRootPrompt.current;
    if (timing && !timing.firstOutputReported && timing.runStartedAt !== undefined && agentSessionId) {
      timing.firstOutputReported = true;
      onFirstOutput({
        type: "prompt.first_output",
        timestamp: event.timestamp,
        eventSeq: agentEvent.seq,
        id: timing.id,
        runStartedAt: timing.runStartedAt,
        sessionId: agentSessionId,
        submittedAt: timing.submittedAt,
      });
    }
  }
  return event;
}

function claimSubmittedAt(
  submissions: Array<{ input: string; submittedAt: number }>,
  input: string,
  fallback: number,
): number {
  const index = submissions.findIndex((submission) => submission.input === input);
  if (index < 0) return fallback;
  return submissions.splice(index, 1)[0]!.submittedAt;
}

function isObservedAgentEvent(
  value: unknown,
  sessionId: string | undefined,
): value is Readonly<{
  request_id: string;
  seq: number;
  type: string;
  payload: Readonly<Record<string, unknown>>;
}> {
  if (!value || typeof value !== "object") return false;
  const event = value as Record<string, unknown>;
  return event.request_id === sessionId
    && typeof event.seq === "number"
    && typeof event.type === "string"
    && typeof event.payload === "object"
    && event.payload !== null;
}

function markAgentTiming(
  stage: string,
  durationMs?: number,
  context: Record<string, unknown> = {},
) {
  const detail = { stage, ...(durationMs === undefined ? {} : { durationMs }), ...context };
  performance.mark(`nanocodex:${stage}`, { detail });
  console.info(`nanocodex:${stage}`, detail);
}
