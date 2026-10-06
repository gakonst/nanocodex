import module from "../../nanocodex/pkg-web/nanocodex_bg.wasm";
import { initializeBrowserEngine } from "../../nanocodex/browser/engine.mjs";
export const initializeNativeEngine = () => initializeBrowserEngine({module});
import * as wasm from "../../nanocodex/pkg-web/nanocodex.js";
function native(name) {
  const fn = wasm[name];
  if (typeof fn !== "function") throw new Error(`Native WASM capability ${name} is unavailable; rebuild the packaged WASM before running managed tools`);
  return fn;
}
export const fileSchemas = () => JSON.parse(native("claudeFileToolSchemas")());
export const filePlan = request => JSON.parse(native("claudeFileToolPlan")(JSON.stringify(request)));
export const taskSchemas = () => JSON.parse(native("claudeTaskToolSchemas")());
export const taskPlan = async request => JSON.parse(await native("claudeTaskToolPlan")(JSON.stringify(request)));
export const patchPaths = patch => JSON.parse(native("nativePatchPaths")(patch));
export const rebasePatch = (patch, mappings) => native("rebaseNativePatch")(patch, JSON.stringify(mappings));
