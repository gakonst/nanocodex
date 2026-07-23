import * as events from "./events.mjs";
import * as session from "./session.mjs";
import * as turn from "./turn.mjs";

export { events, session, turn };

export function agentActions() {
  return (agent) => ({
    events: {
      watch: (options) => events.watch(agent, options),
    },
    session: {
      fork: (options) => session.fork(agent, options),
      setFastMode: (enabled) => session.setFastMode(agent, enabled),
      setThinking: (thinking) => session.setThinking(agent, thinking),
      spawn: () => session.spawn(agent),
    },
    turn: {
      prompt: (options) => turn.prompt(agent, options),
    },
  });
}
