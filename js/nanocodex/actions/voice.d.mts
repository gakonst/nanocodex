import type {
  create as createResource,
  Event,
  Options,
  Snapshot,
  Settings,
  Voice,
} from "../browser/Voice.mjs";

export function create(agent: Parameters<typeof createResource>[0], options?: Options): Voice;
export function start(voice: Voice, options?: Settings): Promise<void>;
export function stop(voice: Voice): Promise<void>;
export function toggle(voice: Voice, options?: Settings): Promise<void>;
export function cancel(voice: Voice): Promise<boolean>;
export function destroy(voice: Voice): Promise<void>;
export function getSnapshot(voice: Voice): Snapshot;
export function subscribe(voice: Voice, listener: () => void): () => void;
export function onEvent(voice: Voice, listener: (event: Event) => void): () => void;

export function speak(voice: Voice, text: string): Promise<void>;
export function appendText(voice: Voice, text: string, options?: { role?: "user" | "developer" | "assistant" }): Promise<void>;
export function appendContext(voice: Voice, text: string): Promise<void>;

export function setMuted(voice: Voice, muted: boolean): void;
export function toggleMuted(voice: Voice): void;
export function noteTypedInput(voice: Voice): Promise<void>;
