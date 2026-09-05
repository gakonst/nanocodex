import { describe, expect, it } from "vitest";

import { handleEgress, type EgressEnv } from "../src/egress";

describe("credential subject directory routing", () => {
  it("routes each subject directly to its own authoritative Durable Object", async () => {
    const routed: string[] = [];
    const stub = {
      fetch: async () => Response.json({ status: "bound" }),
    } as unknown as DurableObjectStub;
    const env = {
      AGENT_SUBJECTS: {
        getByName(name: string) {
          routed.push(name);
          return stub;
        },
      },
    } as unknown as EgressEnv;
    const subjectA = "A".repeat(43);
    const subjectB = "B".repeat(43);

    for (const subject of [subjectA, subjectB, subjectA]) {
      const response = await handleEgress(new Request(
        `https://broker.internal/subjects/${subject}`,
        {
          method: "PUT",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ user_id: "user-routing" }),
        },
      ), env);
      expect(response.status).toBe(200);
    }

    expect(routed).toEqual([
      `agent-subject-v1:${subjectA}`,
      `agent-subject-v1:${subjectB}`,
      `agent-subject-v1:${subjectA}`,
    ]);
  });
});
