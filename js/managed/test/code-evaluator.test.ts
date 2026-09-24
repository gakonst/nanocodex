import { beforeEach, expect, it, vi } from "vitest";
import type { CodeEvaluator } from "nanocodex";

const quickJs = vi.hoisted(() => ({
  initialize: vi.fn(),
  createEvaluator: vi.fn(),
}));
vi.mock("quickjs-emscripten-core", () => ({
  newQuickJSAsyncWASMModuleFromVariant: quickJs.initialize,
  newVariant: () => ({}),
}));
vi.mock("nanocodex/host", () => ({
  createQuickJsEvaluator: quickJs.createEvaluator,
}));
vi.mock("../src/quickjs.wasm", () => ({ default: {} }));

beforeEach(() => {
  vi.resetModules();
  quickJs.initialize.mockReset();
  quickJs.createEvaluator.mockReset();
});

function environment(signal = new AbortController().signal): Parameters<CodeEvaluator>[1] {
  return {
    signal, tools: {}, toolDefinitions: [], text() {}, image() {}, generatedImage() {},
    audio() {}, notify() {}, yield_control() {}, setTimeout: () => 0, clearTimeout() {},
    store() {}, load() {}, exit(): never { throw new Error("exit"); },
  };
}

it("constructs the managed evaluator without starting or waiting for QuickJS", async () => {
  const { managedCodeEvaluator } = await import("../src/code-evaluator");
  quickJs.initialize.mockImplementation(() => { throw new Error("not needed for inference"); });
  expect(managedCodeEvaluator()).toBeTypeOf("function");
  expect(quickJs.initialize).not.toHaveBeenCalled();
  expect(quickJs.createEvaluator).not.toHaveBeenCalled();
});

it("shares pending module initialization but retains one evaluator per session", async () => {
  const { managedCodeEvaluator } = await import("../src/code-evaluator");
  const module = { newContext: vi.fn() };
  let resolve!: (value: typeof module) => void;
  quickJs.initialize.mockReturnValue(new Promise(value => { resolve = value; }));
  const firstQueue = vi.fn<CodeEvaluator>().mockResolvedValue(undefined);
  const secondQueue = vi.fn<CodeEvaluator>().mockResolvedValue(undefined);
  quickJs.createEvaluator.mockReturnValueOnce(firstQueue).mockReturnValueOnce(secondQueue);
  const first = managedCodeEvaluator(), second = managedCodeEvaluator();
  const context = environment();
  const calls = [first("first", context), first("second", context), second("other session", context)];
  expect(quickJs.initialize).toHaveBeenCalledTimes(1);
  expect(quickJs.createEvaluator).not.toHaveBeenCalled();
  resolve(module);
  await Promise.all(calls);
  expect(quickJs.createEvaluator).toHaveBeenCalledTimes(2);
  expect(quickJs.createEvaluator).toHaveBeenCalledWith(module, {
    memoryLimitBytes: 64 * 1024 * 1024, stackLimitBytes: 512 * 1024,
  });
  expect(firstQueue.mock.calls).toEqual([["first", context], ["second", context]]);
  expect(secondQueue.mock.calls).toEqual([["other session", context]]);
  await first("later", context);
  expect(firstQueue).toHaveBeenLastCalledWith("later", context);
  expect(quickJs.initialize).toHaveBeenCalledTimes(1);
  expect(quickJs.createEvaluator).toHaveBeenCalledTimes(2);
});

it("retries failed shared initialization on the next invocation", async () => {
  const { managedCodeEvaluator } = await import("../src/code-evaluator");
  const failure = new Error("initialization failed");
  quickJs.initialize.mockRejectedValueOnce(failure).mockResolvedValueOnce({ newContext: vi.fn() });
  const run = vi.fn<CodeEvaluator>().mockResolvedValue(undefined);
  quickJs.createEvaluator.mockReturnValue(run);
  const evaluate = managedCodeEvaluator();
  await expect(evaluate("first", environment())).rejects.toBe(failure);
  expect(run).not.toHaveBeenCalled();
  await evaluate("retry", environment());
  expect(quickJs.initialize).toHaveBeenCalledTimes(2);
  expect(run).toHaveBeenCalledTimes(1);
});

it("does not initialize QuickJS for an already cancelled cell", async () => {
  const { managedCodeEvaluator } = await import("../src/code-evaluator");
  const failure = new Error("cancelled");
  await expect(managedCodeEvaluator()("source", environment(AbortSignal.abort(failure)))).rejects.toBe(failure);
  expect(quickJs.initialize).not.toHaveBeenCalled();
});

it("cancellation during initialization prevents evaluation without discarding the shared module", async () => {
  const { managedCodeEvaluator } = await import("../src/code-evaluator");
  const module = { newContext: vi.fn() };
  let resolve!: (value: typeof module) => void;
  quickJs.initialize.mockReturnValue(new Promise(value => { resolve = value; }));
  const run = vi.fn<CodeEvaluator>().mockResolvedValue(undefined);
  quickJs.createEvaluator.mockReturnValue(run);
  const evaluate = managedCodeEvaluator();
  const controller = new AbortController();
  const failure = new Error("session retired");
  const pending = evaluate("cancelled", environment(controller.signal));
  controller.abort(failure);
  resolve(module);
  await expect(pending).rejects.toBe(failure);
  expect(quickJs.createEvaluator).not.toHaveBeenCalled();
  expect(run).not.toHaveBeenCalled();
  await managedCodeEvaluator()("active session", environment());
  expect(quickJs.initialize).toHaveBeenCalledTimes(1);
  expect(run).toHaveBeenCalledTimes(1);
});

it("preserves the session evaluator after a cell fails", async () => {
  const { managedCodeEvaluator } = await import("../src/code-evaluator");
  quickJs.initialize.mockResolvedValue({ newContext: vi.fn() });
  const failure = new Error("cell failed");
  const run = vi.fn<CodeEvaluator>().mockRejectedValueOnce(failure).mockResolvedValue(undefined);
  quickJs.createEvaluator.mockReturnValue(run);
  const evaluate = managedCodeEvaluator();
  await expect(evaluate("failed", environment())).rejects.toBe(failure);
  await evaluate("next", environment());
  expect(quickJs.createEvaluator).toHaveBeenCalledTimes(1);
  expect(run).toHaveBeenCalledTimes(2);
});
