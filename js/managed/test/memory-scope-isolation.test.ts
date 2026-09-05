import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";

const ORGANIZATION = "organization-a";
const ORGANIZATION_HEADER = "x-nanocodex-organization-id";
const TEAM_HEADER = "x-nanocodex-team-id";
const SUBJECT_HEADER = "x-nanocodex-subject-id";
const MUTATION_HEADER = "x-nanocodex-memory-mutation";

describe("MemoryScope team isolation", () => {
  it("does not expose one team's memory through another team's scan or keyed read", async () => {
    const memory = (env as unknown as {
      NANOCODEX_MEMORY: DurableObjectNamespace;
    }).NANOCODEX_MEMORY.getByName(crypto.randomUUID());
    const initialized = await memory.fetch("https://memory.internal/initialize", {
      method: "PUT",
      headers: { [ORGANIZATION_HEADER]: ORGANIZATION },
    });
    expect(initialized.status).toBe(204);

    await expect(operation(memory, "team-a", "agent:session-a", {
      operation: "scan",
      query: "copper lighthouse",
    })).resolves.toMatchObject({ operation: "scan", abstained: true });
    const stored = await operation(memory, "team-a", "agent:session-a", {
      operation: "put",
      content: "The deployment marker is copper lighthouse.",
    }, true) as { memory: { key: { id: number; version: number } } };

    await expect(operation(memory, "team-b", "agent:session-b", {
      operation: "scan",
      query: "copper lighthouse",
    })).resolves.toEqual({ operation: "scan", abstained: true, candidates: [] });
    await expect(operation(memory, "team-b", "agent:session-b", {
      operation: "read",
      keys: [stored.memory.key],
    })).resolves.toEqual({ operation: "read", memories: [] });

    await expect(operation(memory, "team-a", "agent:session-a", {
      operation: "scan",
      query: "copper lighthouse",
    })).resolves.toMatchObject({
      operation: "scan",
      abstained: false,
      candidates: [{ key: stored.memory.key }],
    });
    await expect(operation(memory, "team-a", "agent:session-a", {
      operation: "read",
      keys: [stored.memory.key],
    })).resolves.toMatchObject({
      operation: "read",
      memories: [{ key: stored.memory.key }],
    });
  });
});

async function operation(
  memory: DurableObjectStub,
  team: string,
  subject: string,
  body: unknown,
  mutating = false,
): Promise<unknown> {
  const response = await memory.fetch("https://memory.internal/memory", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      [ORGANIZATION_HEADER]: ORGANIZATION,
      [TEAM_HEADER]: team,
      [SUBJECT_HEADER]: subject,
      ...(mutating ? { [MUTATION_HEADER]: "1" } : {}),
    },
    body: JSON.stringify(body),
  });
  expect(response.status).toBe(200);
  return response.json();
}
