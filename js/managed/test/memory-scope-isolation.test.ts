import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { MEMORY_INITIALIZE_ASSERTION } from "../src/memory-scope";

const ORGANIZATION = "organization-a";
const ORGANIZATION_HEADER = "x-nanocodex-organization-id";
const TEAM_HEADER = "x-nanocodex-team-id";
const SUBJECT_HEADER = "x-nanocodex-subject-id";
const MUTATION_HEADER = "x-nanocodex-memory-mutation";

function memoryScope() {
  return (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY.getByName(crypto.randomUUID());
}
function operation(memory: DurableObjectStub, team: string, method: string, body: unknown, mutating = false) {
  return memory.fetch(`https://memory.internal/${method === "write" ? "markdown-memory/write" : `extension-memories/${method}`}`, {
    method: "POST",
    headers: {
      "content-type": "application/json", [ORGANIZATION_HEADER]: ORGANIZATION, [TEAM_HEADER]: team,
      [SUBJECT_HEADER]: `agent:${team}`, [MEMORY_INITIALIZE_ASSERTION]: "1", ...(mutating ? { [MUTATION_HEADER]: "1" } : {}),
    }, body: JSON.stringify(body),
  });
}

describe("MemoryScope team isolation", () => {
  it("initializes on the operation without allowing another organization to reclaim the scope", async () => {
    const memory = memoryScope();
    const search = (organization: string | undefined, team: string | undefined, initialize = true) => memory.fetch(
      "https://memory.internal/extension-memories/search", {
        method: "POST",
        headers: {
          "content-type": "application/json",
          ...(organization === undefined ? {} : { [ORGANIZATION_HEADER]: organization }),
          ...(team === undefined ? {} : { [TEAM_HEADER]: team }),
          [SUBJECT_HEADER]: "agent:session-a",
          ...(initialize ? { [MEMORY_INITIALIZE_ASSERTION]: "1" } : {}),
        }, body: JSON.stringify({ queries: ["copper lighthouse"] }),
      },
    );
    expect((await search(ORGANIZATION, "team-a", false)).status).toBe(404);
    expect((await search(undefined, "team-a")).status).toBe(404);
    expect((await search("unclaimed-organization", undefined)).status).toBe(404);
    const initialized = await search(ORGANIZATION, "team-a");
    expect(initialized.status).toBe(200);
    expect(await initialized.json()).toMatchObject({ queries: ["copper lighthouse"], matches: [] });
    expect((await search(ORGANIZATION, "team-a")).status).toBe(200);
    expect((await search("other-organization", "team-a")).status).toBe(404);
    expect((await search(ORGANIZATION, "team-a", false)).status).toBe(200);
    expect((await operation(memory, "team-a", "write", {
      operation: "put", path: "MEMORY.md", content: "The deployment marker is copper lighthouse.",
    })).status).toBe(403);
    expect((await operation(memory, "team-a", "add_ad_hoc_note", {
      filename: "2026-09-23T10-00-00-isolation.md", note: "copper lighthouse",
    })).status).toBe(403);
  });

  it("does not expose another team's canonical Markdown or ad-hoc files through Codex read/search/list", async () => {
    const memory = memoryScope();
    expect((await memory.fetch("https://memory.internal/initialize", {
      method: "PUT", headers: { [ORGANIZATION_HEADER]: ORGANIZATION },
    })).status).toBe(204);
    const note = { path: "MEMORY.md", content: "The deployment marker is copper lighthouse." };
    expect((await operation(memory, "team-a", "write", { operation: "put", ...note }, true)).status).toBe(200);
    const adHoc = { filename: "2026-09-23T10-00-00-isolation.md", note: "copper lighthouse release note" };
    expect((await operation(memory, "team-a", "add_ad_hoc_note", adHoc, true)).status).toBe(200);
    const adHocPath = `extensions/ad_hoc/notes/${adHoc.filename}`;
    expect(await (await operation(memory, "team-b", "search", { queries: ["copper lighthouse"] })).json()).toMatchObject({ matches: [] });
    expect(await (await operation(memory, "team-b", "list", {})).json()).toMatchObject({ entries: [] });
    for (const path of [note.path, adHocPath]) expect((await operation(memory, "team-b", "read", { path })).status).toBe(400);
    const search = await operation(memory, "team-a", "search", { queries: ["copper lighthouse"] });
    expect(search.status).toBe(200);
    expect(await search.json()).toMatchObject({ matches: [{ path: note.path }, { path: adHocPath }] });
    expect(await (await operation(memory, "team-a", "read", { path: note.path })).json()).toEqual({ ...note, start_line_number: 1, truncated: false });
    expect(await (await operation(memory, "team-a", "read", { path: adHocPath })).json()).toEqual({ path: adHocPath, content: adHoc.note, start_line_number: 1, truncated: false });
    expect(await (await operation(memory, "team-a", "list", {})).json()).toMatchObject({ entries: [
      { path: "MEMORY.md", entry_type: "file" }, { path: "extensions", entry_type: "directory" },
    ] });
  });

  it("returns 404 for retired versioned memory routes", async () => {
    const memory = memoryScope();
    expect((await operation(memory, "team-a", "write", { operation: "put", path: "MEMORY.md", content: "canonical" }, true)).status).toBe(200);
    for (const [method, path] of [["GET", "memories"], ["POST", "memory"], ["DELETE", "memory/1?version=1"]]) {
      const response = await memory.fetch(`https://memory.internal/${path}`, {
        method, headers: { [ORGANIZATION_HEADER]: ORGANIZATION, [TEAM_HEADER]: "team-a", [SUBJECT_HEADER]: "agent:team-a", [MUTATION_HEADER]: "1" },
        ...(method === "POST" ? { body: JSON.stringify({ operation: "scan", query: "canonical" }) } : {}),
      });
      expect(response.status).toBe(404);
    }
  });
});
