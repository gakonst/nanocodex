import { create as createResource } from "../browser/Voice.mjs";

/** Creates one browser voice resource backed by the Agent's Rust/WASM controller. */
export function create(agent, options) {
  return createResource(agent, options);
}

export function start(voice, options) {
  return voice.start(options);
}

export function stop(voice) {
  return voice.stop();
}

export function toggle(voice, options) {
  return voice.toggle(options);
}

export function cancel(voice) {
  return voice.cancel();
}

export function destroy(voice) {
  return voice.destroy();
}

export function getSnapshot(voice) {
  return voice.getSnapshot();
}

export function subscribe(voice, listener) {
  return voice.subscribe(listener);
}

export function onEvent(voice, listener) {
  return voice.onEvent(listener);
}
