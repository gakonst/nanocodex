import type { DefaultAgent } from "../types.mjs";
import type {
  Event,
  Options,
  Snapshot,
  Voice,
  VoiceName,
} from "../browser/Voice.mjs";

export function create(agent: DefaultAgent, options?: Options): Voice;
export function start(voice: Voice, options?: { voice?: VoiceName | undefined }): Promise<void>;
export function stop(voice: Voice): Promise<void>;
export function toggle(voice: Voice, options?: { voice?: VoiceName | undefined }): Promise<void>;
export function cancel(voice: Voice): Promise<boolean>;
export function destroy(voice: Voice): Promise<void>;
export function getSnapshot(voice: Voice): Snapshot;
export function subscribe(voice: Voice, listener: () => void): () => void;
export function onEvent(voice: Voice, listener: (event: Event) => void): () => void;
