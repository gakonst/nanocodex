import type { DefaultAgent } from "../types.mjs";
import type { Agent as ManagedAgent } from "../managed/Agent.mjs";
import type { ConnectAgent } from "../cloud/types.mjs";

export const voices: readonly ["juniper", "maple", "spruce", "ember", "vale", "breeze", "arbor", "sol", "cove"];
export const defaultVoice: "cove";
export type VoiceName = (typeof voices)[number];
export type VoiceErrorCode =
  | "ice_gathering_timeout"
  | "microphone_capture_cancelled"
  | "microphone_capture_timeout"
  | "microphone_not_found"
  | "microphone_permission_blocked"
  | "microphone_unavailable"
  | "realtime_call_timeout"
  | "sideband_open_timeout"
  | "peer_connection_timeout"
  | "peer_connection_failed"
  | "session_ready_timeout";
export declare class VoiceError extends Error {
  readonly code: VoiceErrorCode;
  constructor(code: VoiceErrorCode, message: string, options?: { cause?: unknown });
}
export type Transcript = Readonly<{
  speaker: "user" | "assistant";
  text: string;
  id?: string;
  isPartial?: boolean;
  recovered?: boolean;
}>;
export type Snapshot = Readonly<{
  muted: boolean;
  /** Latest normalized WebRTC audio levels, sampled without retaining a backlog. */
  microphoneLevel: number;
  speakerLevel: number;
  error: Error | undefined;
  status: "idle" | "connecting" | "active" | "error";
  statusText: string | undefined;
  /** Latest 200 rows, retained across stop/start. Subscribe to events for longer history. */
  transcripts: readonly Transcript[];
  voice: VoiceName | undefined;
}>;
export type Event =
  | Readonly<{ type: "answer.recovered"; speaker: "assistant"; text: string; id: string; recovered: true; isPartial: false }>
  | Readonly<{ type: "connecting"; voice: VoiceName }>
  | Readonly<{ type: "started"; voice: VoiceName }>
  | Readonly<{ type: "transcript"; speaker: "user" | "assistant"; text: string; id?: string; isPartial?: false }>
  | Readonly<{ type: "transcript.delta"; speaker: "user" | "assistant"; text: string; id: string; isPartial: true }>
  | Readonly<{ type: "error"; error: Error }>
  | Readonly<{ type: "stopped" }>;
export type Settings = Readonly<{
  voice?: VoiceName | undefined;
  /** Additional speaking preferences; base assistant instructions are retained. */
  instructions?: string | undefined;
  pace?: "slow" | "natural" | "fast" | undefined;
  updates?: "auto" | "results" | "silent" | undefined;
  handoffMode?: "thinking" | "commentary" | "bem_tags" | undefined;
  acknowledgements?: boolean | undefined;
}>;
export type Options = Settings & Readonly<{
  callUrl?: string | URL | undefined;
  sidebandUrl?(callId: string, sessionId: string): string | URL | Promise<string | URL>;
  captureMicrophone?(): Promise<MediaStream>;
  beforeAgentTurn?(): Promise<void>;
}>;
export type Voice = Readonly<{
  /** Applies to captured tracks immediately, including during startup. */
  setMuted(muted: boolean): void;
  toggleMuted(): void;
  /** Call before submitting or steering with typed input. Suppresses audio immediately. */
  noteTypedInput(): Promise<void>;
  cancel(): Promise<boolean>;
  /** Speak explicitly during an active call, independent of background update preferences. */
  speak(text: string): Promise<void>;
  /** Codex subscription voice treats every API role as context text. */
  appendText(text: string, options?: { role?: "user" | "developer" | "assistant" }): Promise<void>;
  appendContext(text: string): Promise<void>;
  destroy(): Promise<void>;
  getSnapshot(): Snapshot;
  onEvent(listener: (event: Event) => void): () => void;
  start(options?: Settings): Promise<void>;
  stop(): Promise<void>;
  subscribe(listener: () => void): () => void;
  toggle(options?: Settings): Promise<void>;
}>;
export function create(agent: DefaultAgent | ManagedAgent | ConnectAgent, options?: Options): Voice;
