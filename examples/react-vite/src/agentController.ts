import type {
  AgentEvent,
  DefaultAgent,
  Turn,
} from "nanocodex";

import type {
  AgentWorkerCommand,
  AgentWorkerMessage,
  StartMessage,
} from "./protocol";

export type ExamplePayment = {
  rootAddress: string;
  accessKeyAddress: string;
  channelId?: string;
  cumulative(): string;
  mcpCumulative?(): string;
};

export type ExampleAgentControllerDependencies = {
  createAgent(start: StartMessage): Promise<{
    agent: DefaultAgent;
    payment?: ExamplePayment;
  }>;
  postMessage(message: AgentWorkerMessage): void;
};

/** Thin lifecycle owner for the React example's dedicated Worker. */
export function createExampleAgentController({
  createAgent,
  postMessage,
}: ExampleAgentControllerDependencies) {
  let agent: DefaultAgent | undefined;
  let eventWatch: ReturnType<DefaultAgent["events"]["watch"]> | undefined;
  let payment: ExamplePayment | undefined;
  let generation = 0;
  let disposed = false;
  let disposal: Promise<void> | undefined;
  const turns = new Set<Turn>();
  const turnReleases = new WeakMap<Turn, Promise<void>>();

  async function handle(command: AgentWorkerCommand): Promise<void> {
    if (disposed) throw new Error("Agent controller is disposed");
    if (command.type === "start") {
      const currentGeneration = await reset();
      const created = await createAgent(command);
      if (disposed || currentGeneration !== generation) {
        await created.agent.session.shutdown();
        return;
      }
      agent = created.agent;
      payment = created.payment;
      eventWatch = agent.events.watch();
      const watchedAgent = agent;
      eventWatch.onEvent((event) => {
        if (
          disposed
          || currentGeneration !== generation
          || agent !== watchedAgent
        ) {
          return;
        }
        postMessage({ type: "event", event });
      });
      postMessage({
        type: "ready",
        transport: command.transport,
        ...(payment
          ? {
              rootAddress: payment.rootAddress,
              accessKeyAddress: payment.accessKeyAddress,
              channelId: payment.channelId,
            }
          : {}),
      });
      return;
    }

    const current = agent;
    if (!current) {
      postMessage({
        type: "error",
        id: command.id,
        message: "Start the agent first.",
      });
      return;
    }

    let turn: Turn;
    try {
      turn = current.turn.prompt({ input: command.prompt });
    } catch (error) {
      postMessage({
        type: "error",
        id: command.id,
        message: errorMessage(error),
      });
      return;
    }
    turns.add(turn);
    const turnGeneration = generation;
    void Promise.resolve()
      .then(() => turn.result())
      .then(
        (result) => {
          try {
            if (disposed || turnGeneration !== generation) return;
            postMessage({
              type: "result",
              id: command.id,
              message: result.finalMessage,
              payment: payment
                ? {
                    channelId: payment.channelId,
                    cumulative: payment.cumulative(),
                    ...(payment.mcpCumulative
                      ? { mcpCumulative: payment.mcpCumulative() }
                      : {}),
                  }
                : undefined,
            });
          } finally {
            result.dispose();
          }
        },
        (error) => {
          if (disposed || turnGeneration !== generation) return;
          postMessage({
            type: "error",
            id: command.id,
            message: errorMessage(error),
          });
        },
      )
      .finally(() => {
        turns.delete(turn);
        releaseTurn(turn);
      });
  }

  async function reset(): Promise<number> {
    const resetGeneration = ++generation;
    eventWatch?.off();
    eventWatch = undefined;
    const activeTurns = [...turns];
    turns.clear();
    const previousAgent = agent;
    agent = undefined;
    payment = undefined;
    await Promise.all(activeTurns.map(cancelAndReleaseTurn));
    await previousAgent?.session.shutdown();
    return resetGeneration;
  }

  function releaseTurn(turn: Turn): void {
    if (turnReleases.has(turn)) return;
    turn.dispose();
    turnReleases.set(turn, Promise.resolve());
  }

  function cancelAndReleaseTurn(turn: Turn): Promise<void> {
    const existing = turnReleases.get(turn);
    if (existing) return existing;
    const release = Promise.resolve()
      .then(() => turn.cancel())
      .catch(() => {})
      .then(() => {
        turn.dispose();
      });
    turnReleases.set(turn, release);
    return release;
  }

  function dispose(): Promise<void> {
    if (disposal) return disposal;
    disposed = true;
    disposal = reset().then(() => {});
    return disposal;
  }

  return Object.freeze({ handle, dispose });
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
