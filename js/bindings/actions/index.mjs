import * as events from "./events.mjs";
import * as session from "./session.mjs";
import { startSubagent } from "../internal.mjs";
import * as turn from "./turn.mjs";
import * as voice from "./voice.mjs";

export { events, session, turn, voice };

export function agentActions() {
  return (agent) => ({
    events: {
      watch: (options) => events.watch(agent, options),
    },
    session: {
      appendDeveloperMessage: (text) => session.appendDeveloperMessage(agent, text),
      compact: () => session.compact(agent),
      context: () => session.context(agent),
      fork: (options) => session.fork(agent, options),
      setFastMode: (enabled) => session.setFastMode(agent, enabled),
      setThinking: (thinking) => session.setThinking(agent, thinking),
      shutdown: () => session.shutdown(agent),
      spawn: () => session.spawn(agent),
      realtime: {
        start: () => session.startRealtimeConversation(agent),
        end: () => session.endRealtimeConversation(agent),
        delegation: (input, transcript) => session.realtimeDelegation(agent, input, transcript),
        tailDelegation: (transcript) => session.realtimeTailDelegation(agent, transcript),
      },
    },
    subagents: {
      start: (task) => startSubagent(agent, task),
    },
    turn: {
      prompt: (options) => turn.prompt(agent, options),
    },
  });
}
