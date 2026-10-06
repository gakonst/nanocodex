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

/** Defers QuickJS until Code Mode runs, retaining one evaluator queue per session. */
export function managedCodeEvaluator(): ReturnType<typeof createQuickJsEvaluator> {
  let evaluator: ReturnType<typeof createQuickJsEvaluator> | undefined;
  return async (source, environment) => {
    environment.signal?.throwIfAborted();
    if (!evaluator) {
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
      // A retired session must not create a VM or call tools after initialization.
      environment.signal?.throwIfAborted();
      evaluator ??= createQuickJsEvaluator(module, {
        memoryLimitBytes: 64 * 1024 * 1024,
        stackLimitBytes: 512 * 1024,
      });
    }
    return evaluator(source, environment);
  };
}
