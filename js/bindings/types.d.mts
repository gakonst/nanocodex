export type Thinking = "none" | "low" | "medium" | "high" | "xhigh" | "max";
export type ReasoningMode = "standard" | "pro";

export type PromptItem =
  | { type: "text"; text: string }
  | { type: "image"; image_url: string; detail?: "auto" | "low" | "high" | "original" | undefined }
  | { type: "audio"; audio_url: string };

export type PromptInput = string | readonly PromptItem[];

export type AgentEvent = {
  protocol_version: number;
  request_id: string;
  seq: number;
  type: string;
  payload: Record<string, unknown>;
};

export type AgentOptions = {
  instructions?: string | undefined;
  reasoningMode?: ReasoningMode | undefined;
  sessionId?: string | undefined;
  thinking?: Thinking | undefined;
};

export type ForkOptions = { at?: Turn | undefined };
export type WatchEventsOptions = { includeAllSessions?: boolean | undefined };

export type EventWatcher = Readonly<{
  onEvent(listener: (event: AgentEvent) => void): () => void;
  off(): void;
  [Symbol.asyncIterator](): AsyncIterableIterator<AgentEvent>;
}>;

export type AgentActions = {
  events: {
    watch(options?: WatchEventsOptions): EventWatcher;
  };
  session: {
    fork(options?: ForkOptions): Promise<DefaultAgent>;
    setThinking(thinking: Thinking): Promise<void>;
    spawn(): Promise<DefaultAgent>;
  };
  turn: {
    prompt(options: { input: PromptInput }): Turn;
  };
};

export type Agent<extended extends object = {}> = {
  readonly key: string;
  readonly name: string;
  readonly sessionId: string;
  readonly type: string;
  readonly uid: string;
  extend<const extension extends object>(
    decorator: (agent: Agent<extended>) => extension,
  ): Agent<extended & extension>;
  dispose(): void;
} & extended;

export type DefaultAgent = Agent<AgentActions>;

export type Turn<agent extends Agent<object> = Agent<object>> = Readonly<{
  readonly agent: agent;
  result(): Promise<string>;
  steer(options: { input: PromptInput }): Promise<void>;
  cancel(): Promise<void>;
  dispose(): void;
}>;

export type ToolContext = {
  callId: string;
  parentCallId: string;
  sessionId: string;
};

export type Tool = {
  description: string;
  parameters: Record<string, unknown>;
  handler(input: unknown, context: ToolContext): unknown | Promise<unknown>;
};

export type ToolMap = Record<string, Tool>;
