import { expect, it } from "vitest";
import { createPdfTextCommandWithExtractor } from "nanocodex-tools/pdf-command";
import { extractPdfText } from "nanocodex-tools/pdf-extract";
import { extractPdfTextFromMediaService } from "../src/pdf-runtime";
import { type Workspace } from "nanocodex-tools";
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
  let called = 0;
  const service = { fetch: async (request: Request) => {
    called++;
    expect(new URL(request.url).pathname).toBe("/pdf-text");
    const form = await request.formData();
    const file = form.get("input") as File;
    const options = JSON.parse(String(form.get("options")));
    return new Response(await extractPdfText(new Uint8Array(await file.arrayBuffer()), options));
  } } as Fetcher;
  const command = createPdfTextCommandWithExtractor(() => workspace,
    (data, options, signal) => extractPdfTextFromMediaService(service, data, options, signal));
  const result = await command.execute(["-f", "2", "-nopgbrk", "/brain/example.pdf", "-"]);
  expect(result.exitCode, result.stderr).toBe(0);
  expect(result.stdout).toBe("Page two: Ω\n");
  expect(called).toBe(1);
});
