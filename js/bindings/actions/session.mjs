import {
  fork as forkAgent,
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
