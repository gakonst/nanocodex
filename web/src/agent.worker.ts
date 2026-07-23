import { Actions, Agent, type ReasoningMode, type Thinking, type Turn } from "nanocodex/browser";
import type { TuiCommand, TuiTarget } from "nanocodex-tui";
import { createBrowserTools } from "./browserTools";

type Target = TuiTarget;
type BrowserPromptItem =
  | { type: "image"; image_url: string; detail?: "auto" | "low" | "high" | "original" }
  | { type: "text"; text: string };

type IncomingMessage = TuiCommand;

type WorkerScope = {
  location: Location;
  onmessage: ((event: MessageEvent<IncomingMessage>) => void) | null;
  postMessage(message: unknown): void;
};

type TurnRecord = {
  id: number;
  turn: Turn;
  settled: boolean;
  completed: boolean;
};

type Branch = {
  id: number;
  parentId?: number;
  agent: Agent.Agent;
  promptOrder: number[];
  turns: Map<number, TurnRecord>;
};

type BtwBranch = Branch & { firstPrompt: boolean };

const BTW_BOUNDARY = `You are answering an ephemeral BTW side question.
Treat inherited conversation history only as reference context. Do not resume or complete an
earlier task. Answer only the question after this boundary. Do not modify the workspace unless
that side question explicitly requests a mutation.

BTW question:
`;

const worker = self as unknown as WorkerScope;
const routes = new Map<string, Target>();
const branches = new Map<number, Branch>();
const sessionImages = new Map<string, string[]>();
let btw: BtwBranch | undefined;
let eventWatch: Actions.events.watch.Watcher | undefined;
worker.onmessage = ({ data }: MessageEvent<IncomingMessage>) => {
  void handleMessage(data).catch((error) => {
    worker.postMessage({
      type: "fatal",
      message: errorMessage(error),
    });
  });
};

async function handleMessage(message: IncomingMessage): Promise<void> {
  switch (message.type) {
    case "start":
      await start(message.thinking, message.reasoningMode);
      return;
    case "prompt": {
      const branch = resolveTarget(message.target);
      if (!branch) {
        post("turnFinished", message.target, { id: message.id, error: "Branch is unavailable" });
        return;
      }
      if (message.intent === "immediate") {
        const active = firstUnsettled(branch);
        if (active) {
          try {
            const prompt = preparePrompt(branch, message.prompt);
            if (message.images?.length) {
              for (const image of message.images) rememberSessionImage(branch.agent.sessionId, image);
              await active.turn.steer({ input: promptContent(prompt, message.images) });
            } else {
              await active.turn.steer({ input: prompt });
            }
            post("steerAdmitted", message.target, { id: message.id });
            return;
          } catch (error) {
            if (!errorMessage(error).includes("not active for steering")) {
              post("steerFailed", message.target, {
                id: message.id,
                error: errorMessage(error),
              });
              return;
            }
            post("steerQueued", message.target, { id: message.id, prompt: message.prompt });
          }
        }
      }
      startTurn(branch, message.target, message.id, message.prompt, message.images);
      return;
    }
    case "cancel": {
      const branch = resolveTarget(message.target);
      const active = branch && firstUnsettled(branch);
      if (!active) {
        post("cancelFailed", message.target, { error: "No active or queued turn" });
        return;
      }
      try {
        await active.turn.cancel();
        post("cancelAccepted", message.target);
      } catch (error) {
        post("cancelFailed", message.target, { error: errorMessage(error) });
      }
      return;
    }
    case "openBtw": {
      const main = branches.get(message.sourceBranchId);
      if (!main) throw new Error("Main branch is unavailable");
      if (btw) sessionImages.delete(btw.agent.sessionId);
      btw?.agent.dispose();
      btw = undefined;
      try {
        const agent = await main.agent.session.fork();
        inheritSessionImages(main.agent.sessionId, agent.sessionId);
        btw = {
          id: message.id,
          agent,
          promptOrder: [],
          turns: new Map(),
          firstPrompt: true,
        };
        const target: Target = { pane: "btw", id: message.id };
        routes.set(agent.sessionId, target);
        worker.postMessage({ type: "btwOpened", id: message.id, sessionId: agent.sessionId });
        if (message.prompt && message.promptId !== undefined) {
          startTurn(btw, target, message.promptId, message.prompt, message.images);
        }
      } catch (error) {
        worker.postMessage({ type: "btwOpenFailed", id: message.id, error: errorMessage(error) });
      }
      return;
    }
    case "closeBtw":
      if (btw?.id === message.id) {
        routes.delete(btw.agent.sessionId);
        sessionImages.delete(btw.agent.sessionId);
        btw.agent.dispose();
        btw = undefined;
      }
      return;
    case "historicalFork": {
      try {
        const source = branches.get(message.sourceBranchId);
        if (!source) throw new Error("Source branch is unavailable");
        const position = source.promptOrder.indexOf(message.selectedPromptId);
        if (position < 0) throw new Error("Selected prompt is not part of this branch");
        const inherited = source.promptOrder.slice(0, position);
        const previous = [...inherited]
          .reverse()
          .map((id) => source.turns.get(id))
          .find((record) => record?.completed);
        const agent = previous
          ? await source.agent.session.fork({ at: previous.turn })
          : await source.agent.session.spawn();
        inheritSessionImages(source.agent.sessionId, agent.sessionId);
        const branch: Branch = {
          id: message.newBranchId,
          parentId: source.id,
          agent,
          promptOrder: inherited.slice(),
          turns: new Map(inherited.map((id) => [id, source.turns.get(id)!])),
        };
        branches.set(branch.id, branch);
        const target: Target = { pane: "main", branchId: branch.id };
        routes.set(agent.sessionId, target);
        worker.postMessage({
          type: "branchOpened",
          id: branch.id,
          parentId: source.id,
          sessionId: agent.sessionId,
        });
        startTurn(branch, target, message.newPromptId, message.prompt);
      } catch (error) {
        worker.postMessage({
          type: "branchOpenFailed",
          id: message.newBranchId,
          error: errorMessage(error),
        });
      }
      return;
    }
  }
}

async function start(thinking: Thinking, reasoningMode: ReasoningMode): Promise<void> {
  eventWatch?.off();
  eventWatch = undefined;
  for (const branch of branches.values()) branch.agent.dispose();
  branches.clear();
  routes.clear();
  sessionImages.clear();
  btw?.agent.dispose();
  btw = undefined;
  const agent = await Agent.create({
    apiKey: "worker-managed",
    websocketUrl: workerEndpoint(),
    createWebSocket: (endpoint: string, sessionId: string) => {
      const url = new URL(endpoint);
      url.searchParams.set("session_id", sessionId);
      return new WebSocket(url);
    },
    tools: {
      ...createBrowserTools({
        recentImages(sessionId, count) {
          return (sessionImages.get(sessionId) ?? []).slice(-count);
        },
        rememberImage: rememberSessionImage,
      }),
      browserInfo: {
        description: "Return basic information about the browser Worker runtime.",
        parameters: { type: "object", additionalProperties: false },
        handler: async () => ({
          language: navigator.language,
          online: navigator.onLine,
          userAgent: navigator.userAgent,
        }),
      },
    },
    thinking,
    reasoningMode,
  });
  eventWatch = agent.events.watch({ includeAllSessions: true });
  eventWatch.onEvent((event) => {
    const target = event.request_id ? routes.get(event.request_id) : undefined;
    if (target) worker.postMessage({ type: "event", target, event });
  });
  const main: Branch = { id: 0, agent, promptOrder: [], turns: new Map() };
  branches.set(0, main);
  routes.set(agent.sessionId, { pane: "main", branchId: 0 });
  worker.postMessage({ type: "ready", sessionId: agent.sessionId });
}

function startTurn(branch: Branch, target: Target, id: number, prompt: string, images: string[] = []): void {
  let turn: Turn;
  try {
    for (const image of images) rememberSessionImage(branch.agent.sessionId, image);
    const prepared = preparePrompt(branch, prompt);
    turn = branch.agent.turn.prompt({
      input: images.length ? promptContent(prepared, images) : prepared,
    });
  } catch (error) {
    post("turnFinished", target, { id, error: errorMessage(error) });
    return;
  }
  const record: TurnRecord = { id, turn, settled: false, completed: false };
  branch.promptOrder.push(id);
  branch.turns.set(id, record);
  void turn.result().then(
    (message) => {
      record.settled = true;
      record.completed = true;
      post("turnFinished", target, { id, message });
    },
    (error) => {
      record.settled = true;
      post("turnFinished", target, { id, error: errorMessage(error) });
    },
  );
}

function rememberSessionImage(sessionId: string, imageUrl: string): void {
  const images = sessionImages.get(sessionId) ?? [];
  images.push(imageUrl);
  if (images.length > 10) images.splice(0, images.length - 10);
  sessionImages.set(sessionId, images);
}

function inheritSessionImages(sourceSessionId: string, targetSessionId: string): void {
  const images = sessionImages.get(sourceSessionId);
  if (images?.length) sessionImages.set(targetSessionId, images.slice());
}

function promptContent(prompt: string, images: string[]): BrowserPromptItem[] {
  const content: BrowserPromptItem[] = images.map((image_url) => ({ type: "image", image_url }));
  if (prompt) content.push({ type: "text", text: prompt });
  return content;
}

function preparePrompt(branch: Branch, prompt: string): string {
  if ("firstPrompt" in branch && branch.firstPrompt) {
    branch.firstPrompt = false;
    return BTW_BOUNDARY + prompt;
  }
  return prompt;
}

function firstUnsettled(branch: Branch): TurnRecord | undefined {
  return [...branch.turns.values()].find((turn) => !turn.settled);
}

function resolveTarget(target: Target): Branch | undefined {
  return target.pane === "main"
    ? branches.get(target.branchId)
    : btw?.id === target.id
      ? btw
      : undefined;
}

function post(type: string, target: Target, detail: Record<string, unknown> = {}): void {
  worker.postMessage({ type, target, ...detail });
}

function workerEndpoint(): string {
  const protocol = self.location.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${self.location.host}/api/responses`;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
