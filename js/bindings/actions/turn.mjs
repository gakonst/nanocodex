import {
  cancel as cancelTurn,
  getTurnResult,
  getTurnSnapshot,
  prompt as promptTurn,
  steer as steerTurn,
} from "../internal.mjs";

export function prompt(agent, options) {
  return promptTurn(agent, options);
}

export function getResult(turn) {
  return getTurnResult(turn);
}

export function getSnapshot(turn) {
  return getTurnSnapshot(turn);
}

export function steer(turn, options) {
  return steerTurn(turn, options);
}

export function cancel(turn) {
  return cancelTurn(turn);
}
