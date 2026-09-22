import assert from "node:assert/strict";
import test from "node:test";
import { presentTool } from "../dist/toolPresentation.js";

for (const [output, title] of [
  [{ accepted: true, status: "accepted" }, "Accepted subagent result"],
  [{ accepted: false, status: "superseded" }, "Superseded subagent result"],
  [{ accepted: true }, "Accepted subagent result"],
  [undefined, "Submit subagent result"],
]) {
  test(`submission presentation: ${JSON.stringify(output)}`, () => {
    const presentation = presentTool({
      callId: "submission", name: "submit_result", children: [],
      status: output ? "completed" : "running",
      arguments: JSON.stringify({ output: { report: "done" } }),
      output: output && JSON.stringify(output),
    });
    assert.equal(presentation.title, title);
    if (output?.status === "superseded") {
      assert.equal(presentation.outputSummary, "Continue with updated instructions");
    }
  });
}
