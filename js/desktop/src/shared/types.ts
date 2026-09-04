import type { ManagedEvent } from "nanocodex/managed";

export type Settings = {
  model: string;
  thinking: string;
  reasoning_mode: "standard" | "pro";
  fast_mode: boolean;
};
export type Thread = {
  id: string;
  title: string;
  updatedAt: number;
  turnCount: number;
};
export type Tab = {
  id: string;
  threadId?: string;
  title?: string;
  draft: string;
  target: string;
  folder: string;
  seenCursor?: string;
};
export type Layout = {
  tabs: Tab[];
  activeTabId: string;
  tabPosition: "left" | "top";
  theme: "system" | "light" | "dark";
};
export type HandConfig = {
  id: string;
  name: string;
  kind: "local" | "vm";
  workspace: string;
  agentId?: string;
  rootfs?: string;
  guestRuntime?: string;
  binary?: string;
  cpus?: number;
  memoryMiB?: number;
  network?: boolean;
};
export type Hand = HandConfig & {
  status: "stopped" | "connecting" | "connected" | "error";
  error?: string;
  calls: number;
  activeCalls: number;
  logs: string[];
};
export type DesktopState = {
  connected: boolean;
  hasCredentials?: boolean;
  accountScope?: string;
  baseUrl: string;
  error?: string;
  threads: Thread[];
  hands: Hand[];
  layout?: Layout;
  defaults: Partial<HandConfig>;
  platform: string;
  version: string;
};
export type ThreadSnapshot = {
  id: string;
  events: ManagedEvent[];
  hasMore: boolean;
  connected: boolean;
  activeTurns: string[];
  acceptedTurns?: number;
  settings: Settings;
  error?: string;
};
export type DesktopEvent =
  | { type: "state"; state: DesktopState }
  | { type: "thread"; thread: ThreadSnapshot }
  | { type: "command"; command: string };

export interface DesktopBridge {
  state(): Promise<DesktopState>;
  connect(input: {
    baseUrl: string;
    apiKey: string;
    remember: boolean;
  }): Promise<DesktopState>;
  disconnect(): Promise<DesktopState>;
  startSignIn(input: {
    phone: string;
    baseUrl?: string;
  }): Promise<{ phone: string; resendAt: number; expiresAt: number }>;
  verifySignIn(input: { code: string }): Promise<DesktopState>;
  cancelSignIn(): Promise<void>;
  refresh(): Promise<DesktopState>;
  openThread(id: string): Promise<ThreadSnapshot>;
  closeThread(id: string): Promise<void>;
  older(id: string): Promise<ThreadSnapshot>;
  createThread(settings: Settings): Promise<Thread>;
  prompt(input: {
    agentId: string;
    input: string;
    requestId: string;
  }): Promise<string>;
  steer(input: {
    agentId: string;
    turnId: string;
    input: string;
  }): Promise<void>;
  cancel(input: { agentId: string; turnId: string }): Promise<void>;
  settings(input: { agentId: string; settings: Settings }): Promise<Settings>;
  choosePath(kind: "directory" | "file"): Promise<string | null>;
  saveLayout(layout: Layout & { accountScope?: string }): Promise<void>;
  saveHand(config: HandConfig): Promise<DesktopState>;
  prepareFolderHand(input: {
    agentId: string;
    workspace: string;
  }): Promise<Hand>;
  startHand(id: string): Promise<DesktopState>;
  stopHand(id: string): Promise<DesktopState>;
  removeHand(id: string): Promise<DesktopState>;
  openAccount(): Promise<void>;
  onEvent(listener: (event: DesktopEvent) => void): () => void;
}

declare global {
  interface Window {
    nanocodex: DesktopBridge;
  }
}
