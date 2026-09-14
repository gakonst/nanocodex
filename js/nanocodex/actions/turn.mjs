import {
  awaitTurnAcceptance,
  cancel as cancelTurn,
  getTurnResult,
  getTurnSnapshot,
  getTurnUsage,
  prompt as promptTurn,
  steer as steerTurn,
  withdrawSteer as withdrawTurnSteer,
} from "../internal.mjs";

export function prompt(agent, options) {
  return promptTurn(agent, options);
}

export function getResult(turn) {
  return getTurnResult(turn);
}

export function accepted(turn) {
  return awaitTurnAcceptance(turn);
}

export function getSnapshot(result) {
  return getTurnSnapshot(result);
}

export function getUsage(result) {
  return getTurnUsage(result);
}

export function steer(turn, options) {
  return steerTurn(turn, options);
}

export function cancel(turn) {
  return cancelTurn(turn);
}

export function withdrawSteer(turn, options) {
  return withdrawTurnSteer(turn, options);
}
