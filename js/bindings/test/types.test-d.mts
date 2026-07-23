import { Actions, Agent, type SessionSnapshot, type Turn } from "../node/index.mjs";
import { Agent as BrowserAgent } from "../browser/index.mjs";

declare const apiKey: string;

async function check() {
  const agent = await Agent.create({ apiKey, thinking: "high", fastMode: false });
  await agent.session.setFastMode(true);
  const options: Actions.turn.prompt.Options = { input: "hello" };
  const turn: Turn = agent.turn.prompt(options);
  const sameTurn: Actions.turn.prompt.ReturnType = Actions.turn.prompt(agent, options);
  const message: Actions.turn.getResult.ReturnType = await sameTurn.result();
  const snapshot: SessionSnapshot = sameTurn.snapshot();
  Actions.turn.getSnapshot(sameTurn);
  void message;

  await Agent.create({ apiKey, resume: snapshot });

  const fork = await Actions.session.fork(agent, { at: turn });
  fork.turn.prompt({ input: [{ type: "text", text: "continue" }] });

  const watch: Actions.events.watch.Watcher = agent.events.watch();
  watch.onEvent((event) => event.payload);
  for await (const event of watch) event.seq;
  watch.off();

  const extended = agent.extend((client) => ({
    inspect: { session: () => client.sessionId },
  }));
  extended.inspect.session();

  await BrowserAgent.create({ websocketUrl: "wss://example.com" });

  // @ts-expect-error actions are domain-grouped on the decorated Agent.
  agent.prompt("hello");
  // @ts-expect-error prompt accepts a named options bag.
  agent.turn.prompt("hello");
}

void check;
