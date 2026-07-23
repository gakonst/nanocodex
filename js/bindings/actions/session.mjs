import {
  fork as forkAgent,
  setFastMode as setAgentFastMode,
  setThinking as setAgentThinking,
  spawn as spawnAgent,
} from "../internal.mjs";

export function fork(agent, options = {}) {
  return forkAgent(agent, options);
}

export function spawn(agent) {
  return spawnAgent(agent);
}

export function setThinking(agent, thinking) {
  return setAgentThinking(agent, thinking);
}

export function setFastMode(agent, enabled) {
  return setAgentFastMode(agent, enabled);
}
