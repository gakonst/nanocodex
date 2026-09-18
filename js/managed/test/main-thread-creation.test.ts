import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { mainThreadRequest, retainMainRoute, retainMainCoordinatorCreation, type CanonicalProject } from "../src/main-thread";

const input = { project_id: "research", name: "Research", id: "first", input: "Investigate" };
const agentId = "11111111-1111-4111-8111-111111111111";
const original = JSON.stringify({
  settings: { model: "gpt-6-astra", thinking: "high", reasoning_mode: "standard", fast_mode: false },
  configuration: { instructions: "Retain instructions", tools: ["web"],
    environment: { network: { access: "restricted", allowed_domains: ["example.com"] } }, multi_agent: { enabled: true } },
});
async function inside(test: (storage: DurableObjectStorage) => Promise<void>) {
  const ns = (env as unknown as { NANOCODEX_USERS: DurableObjectNamespace }).NANOCODEX_USERS;
  await runInDurableObject(ns.getByName(crypto.randomUUID()), async (_, state) => test(state.storage));
}

describe("Main project creation snapshot", () => {
  it("retries a failed registry write with the exact creation body after Main settings change", () => inside(async storage => {
    let project: CanonicalProject | undefined;
    let failLink = true;
    const creationBodies: string[] = [];
    const create = vi.fn(async (key: string, body: string) => {
      expect(key).toBe("canonical:team:project:research");
      creationBodies.push(body);
      // The creation service uses the same key/body to recover the same agent.
      expect(body).toBe(original);
      return Response.json({ agent_id: agentId });
    });
    async function route(creation: string) {
      const plan = retainMainRoute(storage, input, creation);
      return mainThreadRequest(new Request("https://example.com/v1/projects/research", {
        method: "PUT", body: JSON.stringify({ name: input.name }),
      }), {
        teamId: "team",
        registry: async (path, init) => {
          if (path === "/canonical-generations/projects/research") return Response.json({ generation: 0 });
          if (!init) return Response.json({ data: project ? [project] : [] });
          expect(path).toBe("/projects/research");
          if (failLink) { failLink = false; return new Response(null, { status: 503 }); }
          const body = JSON.parse(init.body as string);
          project = { id: input.project_id, ...body };
          return Response.json(project);
        },
        create: key => create(key, plan.creation),
      });
    }
    expect((await route(original)).status).toBe(503);
    expect((await route('{"settings":{"model":"changed"},"configuration":{}}')).status).toBe(200);
    expect(creationBodies).toEqual([original, original]);
    // Existing canonical coordinators keep their own configuration on later routes.
    expect((await route("{}")).status).toBe(200);
    expect(create).toHaveBeenCalledTimes(2);
    expect(project?.coordinator_agent_id).toBe(agentId);
  }));

  it("keeps snapshots immutable and rejects changed routing intent before replacing them", () => inside(async storage => {
    expect(retainMainRoute(storage, input, original)).toEqual({ creation: original });
    expect(retainMainRoute(storage, input, "{}")).toEqual({ creation: original });
    for (const change of [{ input: "Other work" }, { project_id: "other" }, { name: "Other" }]) {
      expect(() => retainMainRoute(storage, { ...input, ...change }, "{}")).toThrow("conflicts");
    }
    expect(retainMainRoute(storage, input, "{}")).toEqual({ creation: original });
  }));

  it("freezes one creation snapshot per generation across route IDs and recreation", () => inside(async storage => {
    let generation = 0;
    const bodies: Array<{ key: string; body: string }> = [];
    const route = async (routeId: string, creation: string) => {
      const plan = retainMainRoute(storage, { ...input, id: routeId }, creation);
      return mainThreadRequest(new Request("https://example.com/v1/projects/research", {
        method: "PUT", body: JSON.stringify({ name: input.name }),
      }), {
        teamId: "team",
        registry: async (path, init) => {
          if (path === "/canonical-generations/projects/research") return Response.json({ generation });
          if (!init) return Response.json({ data: [] });
          return new Response(null, { status: 503 }); // Lose registration after creation.
        },
        create: async (key, currentGeneration) => {
          bodies.push({ key, body: retainMainCoordinatorCreation(storage, input.project_id, plan.creation, currentGeneration) });
          return Response.json({ agent_id: agentId });
        },
      });
    };
    await route("first", original);
    await route("retry-other-id", "changed-before-deletion");
    generation = 1;
    await route("recreated", "new-generation-configuration");
    await route("recreated-retry", "changed-after-recreation");
    expect(bodies).toEqual([
      { key: "canonical:team:project:research", body: original },
      { key: "canonical:team:project:research", body: original },
      { key: "canonical:team:project:research:generation:1", body: "new-generation-configuration" },
      { key: "canonical:team:project:research:generation:1", body: "new-generation-configuration" },
    ]);
    expect(retainMainCoordinatorCreation(storage, input.project_id, "later", 0)).toBe(original);
    expect(() => retainMainCoordinatorCreation(storage, input.project_id, "invalid", -1)).toThrow("generation");
  }));

  it("upgrades old routing plans without losing their conflict protection", () => inside(async storage => {
    storage.sql.exec("CREATE TABLE main_route_plans (id TEXT PRIMARY KEY, request_json TEXT NOT NULL)");
    storage.sql.exec("INSERT INTO main_route_plans(id,request_json) VALUES (?,?)", input.id, JSON.stringify(input));
    expect(retainMainRoute(storage, input, original)).toEqual({ creation: original });
    expect(retainMainRoute(storage, input, "{}")).toEqual({ creation: original });
    expect(() => retainMainRoute(storage, { ...input, name: "Changed" }, "{}")).toThrow("conflicts");
  }));
});
