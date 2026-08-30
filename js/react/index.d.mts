import type { AgentEvent, AgentLifecycle, DefaultAgent } from "nanocodex";
import type { ManagedAgent } from "nanocodex/managed";
import type { ConnectAgent } from "nanocodex/connect";
import type { Config } from "nanocodex/browser";
import type {
  Options as VoiceOptions,
  Snapshot as VoiceSnapshot,
  VoiceName,
} from "nanocodex/browser/voice";
import type { ReactNode } from "react";

export {
  createConfig,
  type AgentSnapshot,
  type AgentStatus,
  type Config,
  type CreateConfigParameters,
  type CreateManagedConfigParameters,
} from "nanocodex/browser";

export type UseNanocodexParameters<
  Selection = UseNanocodexReturnType,
  agent extends AgentLifecycle = DefaultAgent,
> = Readonly<{
  /** Defaults to true. Disabled hooks stay idle and do not prepare or create an Agent. */
  enabled?: boolean | undefined;
  /** Stable OPFS/Git workspace identity. Omitted or empty values use the config's stable default. */
  threadId?: string | undefined;
  /** Optional provider bypass for libraries and isolated consumers. */
  config?: Config<agent> | undefined;
  /** Selects the value observed by this component. Defaults to the full Agent resource. */
  selector?: ((resource: UseNanocodexReturnType<agent>) => Selection) | undefined;
  /** Controls whether two selected values are observably different. Defaults to Object.is. */
  equalityFn?: ((previous: Selection, next: Selection) => boolean) | undefined;
}>;

export type UseNanocodexReturnType<agent extends AgentLifecycle = DefaultAgent> =
  | Readonly<{
    data: undefined;
    error: undefined;
    status: "idle";
    isError: false;
    isIdle: true;
    isPending: false;
    isSuccess: false;
    refetch(): void;
  }>
  | Readonly<{
    data: undefined;
    error: undefined;
    status: "pending";
    isError: false;
    isIdle: false;
    isPending: true;
    isSuccess: false;
    refetch(): void;
  }>
  | Readonly<{
    data: agent;
    error: undefined;
    status: "success";
    isError: false;
    isIdle: false;
    isPending: false;
    isSuccess: true;
    refetch(): void;
  }>
  | Readonly<{
    data: undefined;
    error: unknown;
    status: "error";
    isError: true;
    isIdle: false;
    isPending: false;
    isSuccess: false;
    refetch(): void;
  }>;

export function NanocodexProvider(props: {
  children: ReactNode;
  config: Config<AgentLifecycle>;
}): ReactNode;
export function useConfig<agent extends AgentLifecycle = DefaultAgent>(
  parameters?: { config?: Config<agent> | undefined },
): Config<agent>;
export function useNanocodex<agent extends AgentLifecycle, Selection>(
  options: UseNanocodexParameters<Selection, agent> & {
    selector: (resource: UseNanocodexReturnType<agent>) => Selection;
  },
): Selection;
export function useNanocodex<agent extends AgentLifecycle = DefaultAgent>(
  options?: Omit<UseNanocodexParameters<UseNanocodexReturnType<agent>, agent>, "selector"> & {
    selector?: undefined;
  },
): UseNanocodexReturnType<agent>;
export function useAgentEvents(
  agent: AgentLifecycle | undefined,
  listener: (event: AgentEvent) => void,
  options?: { includeAllSessions?: boolean | undefined },
): void;

export type UseVoiceParameters = VoiceOptions & Readonly<{
  /** Defaults to true. Disabled hooks do not create a voice resource. */
  enabled?: boolean | undefined;
}>;
export type UseVoiceReturnType = VoiceSnapshot & Readonly<{
  isActive: boolean;
  isConnecting: boolean;
  isError: boolean;
  isIdle: boolean;
  cancel(): Promise<boolean>;
  start(options?: { voice?: VoiceName | undefined }): Promise<void>;
  stop(): Promise<void>;
  toggle(options?: { voice?: VoiceName | undefined }): Promise<void>;
}>;
export function useVoice(
  agent: DefaultAgent | ManagedAgent | ConnectAgent | undefined,
  options?: UseVoiceParameters,
): UseVoiceReturnType;
