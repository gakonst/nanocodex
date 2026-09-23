import { expect, it } from "vitest";
import { createPdfTextCommand, type Workspace } from "nanocodex-tools";
// Synthetic fixture shared with the package's Node integration tests.
// @ts-expect-error JavaScript test helper has no declarations.
import { pdf } from "../../nanocodex-tools/test/fixtures/pdf.mjs";

it("extracts compressed Unicode PDF text inside workerd without a sandbox or egress", async () => {
  const workspace: Workspace = {
    root: "/brain",
    async list() { return []; },
    async readFile() { return new Uint8Array(pdf()); },
    async writeFile() { throw new Error("unexpected write"); },
    async remove() { throw new Error("unexpected remove"); },
    async mkdir() { throw new Error("unexpected mkdir"); },
  };
  const command = createPdfTextCommand(() => workspace);
  const result = await command.execute(["-f", "2", "-nopgbrk", "/brain/example.pdf", "-"]);
  expect(result.exitCode, result.stderr).toBe(0);
  expect(result.stdout).toBe("Page two: Ω\n");
});
