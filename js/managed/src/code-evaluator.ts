import asyncVariant from "@jitl/quickjs-wasmfile-release-asyncify";
import { createQuickJsEvaluator } from "nanocodex/host";
import {
  newQuickJSAsyncWASMModuleFromVariant,
  newVariant,
} from "quickjs-emscripten-core";

import quickJsWasm from "./quickjs.wasm";

let quickJsModule:
  | ReturnType<typeof newQuickJSAsyncWASMModuleFromVariant>
  | undefined;

/** Creates a session-owned evaluator while sharing the isolate's QuickJS module. */
export async function managedCodeEvaluator(): Promise<ReturnType<typeof createQuickJsEvaluator>> {
  const initialization = quickJsModule ??= newQuickJSAsyncWASMModuleFromVariant(
    newVariant(asyncVariant, { wasmModule: quickJsWasm }),
  );
  let module: Awaited<typeof initialization>;
  try {
    module = await initialization;
  } catch (error) {
    if (quickJsModule === initialization) quickJsModule = undefined;
    throw error;
  }
  return createQuickJsEvaluator(module, {
    memoryLimitBytes: 64 * 1024 * 1024,
    stackLimitBytes: 512 * 1024,
  });
}

