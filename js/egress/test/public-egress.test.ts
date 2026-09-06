import { env } from "cloudflare:workers";
import { SELF } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import { handleEgress, type EgressEnv } from "../src/egress";

const workerEnv = env as unknown as EgressEnv;

it("brokers public traffic without projecting account authority or imposing a redirect quota", async () => {
  const subject = "P".repeat(43);
  expect((await SELF.fetch(`https://broker.internal/subjects/${subject}`, {
    method: "PUT", headers: { "content-type": "application/json" },
    body: JSON.stringify({ user_id: "public-egress-owner" }),
  })).status).toBe(200);
  const requests: Request[] = [];
  const upstream = vi.fn(async (input: RequestInfo | URL) => {
    const request = input as Request;
    requests.push(request);
    expect(request.headers.get("x-nanocodex-subject")).toBeNull();
    expect(request.headers.get("x-nanocodex-target-url")).toBeNull();
    expect(request.headers.get("authorization")).toBeNull();
    const step = Number(new URL(request.url).searchParams.get("step"));
    return step < 7
      ? new Response(null, { status: 302, headers: { location: `/users/private?step=${step + 1}` } })
      : new Response("public result");
  });
  const response = await handleEgress(publicRequest("https://example.com/users/private", subject),
    workerEnv, undefined, upstream as typeof fetch);
  expect(await response.text()).toBe("public result");
  expect(requests).toHaveLength(8);

  await SELF.fetch(`https://broker.internal/subjects/${subject}`, {
    method: "DELETE", headers: { "content-type": "application/json" },
    body: JSON.stringify({ user_id: "public-egress-owner" }),
  });
  expect((await handleEgress(publicRequest("https://example.com", subject),
    workerEnv, undefined, upstream as typeof fetch)).status).not.toBe(200);
  expect(requests).toHaveLength(8);
});

it("rejects private destinations, credential headers, and redirect escapes before forwarding", async () => {
  const upstream = vi.fn(async () => new Response("must not fetch"));
  for (const target of ["http://127.0.0.1/", "https://broker.internal/users/private", "https://api.github.com/user"]) {
    expect((await handleEgress(publicRequest(target), workerEnv, undefined, upstream as typeof fetch)).status).toBe(403);
  }
  const credentialed = publicRequest("https://example.com");
  credentialed.headers.set("authorization", "Bearer caller-secret");
  expect((await handleEgress(credentialed, workerEnv, undefined, upstream as typeof fetch)).status).toBe(403);
  expect(upstream).not.toHaveBeenCalled();
  for (const location of ["http://127.0.0.1/", "https://api.github.com/user", "https://broker.internal/users/private"]) {
    const redirect = vi.fn(async () => new Response(null, { status: 302, headers: { location } }));
    expect((await handleEgress(publicRequest("https://example.com"), workerEnv, undefined, redirect as typeof fetch)).status).toBe(502);
    expect(redirect).toHaveBeenCalledTimes(1);
  }
});

function publicRequest(target: string, subject?: string): Request {
  return new Request("https://public-egress.internal/v1/request", { headers: {
    "x-nanocodex-target-url": target,
    ...(subject ? { "x-nanocodex-subject": subject } : {}),
  } });
}
