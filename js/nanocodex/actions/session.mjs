import {
  appendDeveloperMessage as appendAgentDeveloperMessage,
  compact as compactAgent,
  context as agentContext,
  fork as forkAgent,
  setFastMode as setAgentFastMode,
  setModel as setAgentModel,
  setThinking as setAgentThinking,
  shutdown as shutdownAgent,
  spawn as spawnAgent,
} from "../internal.mjs";

export {
  endRealtimeConversation,
  realtimeDelegation,
  realtimeTailDelegation,
  startRealtimeConversation,
} from "../internal.mjs";

export function appendDeveloperMessage(agent, text) {
  return appendAgentDeveloperMessage(agent, text);
}

export function compact(agent) {
  return compactAgent(agent);
}

export function context(agent) {
  return agentContext(agent);
}

export function fork(agent, options = {}) {
  return forkAgent(agent, options);
}

export function spawn(agent) {
  return spawnAgent(agent);
}

export function setThinking(agent, thinking) {
  return setAgentThinking(agent, thinking);
}

export function setModel(agent, model) {
  return setAgentModel(agent, model);
}

export function setFastMode(agent, enabled) {
  return setAgentFastMode(agent, enabled);
}

export function shutdown(agent) {
  return shutdownAgent(agent);
}
