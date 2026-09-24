import { expect, it } from "vitest";
import type { CodeEvaluatorEnvironment } from "nanocodex";
import { managedCodeEvaluator } from "../src/code-evaluator";

it("runs the first lazy Code Mode cell in real QuickJS and reuses the evaluator after failure", async () => {
  const output: unknown[] = [];
  const environment: CodeEvaluatorEnvironment = {
    signal: new AbortController().signal,
    tools: { add: async input => { const { left, right } = input as { left: number; right: number }; return left + right; } },
    toolDefinitions: [], text: value => { output.push(value); },
    image() {}, generatedImage() {}, audio() {}, notify() {}, yield_control() {},
    setTimeout: () => 0, clearTimeout() {}, store() {}, load() {},
    exit(): never { throw new Error("exit"); },
  };
  const evaluate = managedCodeEvaluator();
  await evaluate("text(await tools.add({ left: 20, right: 22 }));", environment);
  expect(output).toEqual(["42"]);
  await expect(evaluate('throw new Error("cell failed")', environment)).rejects.toThrow("cell failed");
  await evaluate('text("next cell");', environment);
  expect(output).toEqual(["42", "next cell"]);
});
