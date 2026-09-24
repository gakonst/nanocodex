import { expect, it } from "vitest";
import { SpotifyReadCache } from "../src/spotify-read-cache";

it("bounds cached bodies and streams oversized responses without truncation", async () => {
  const cache = new SpotifyReadCache();
  const text = JSON.stringify({ value: "x".repeat(1024 * 1024 + 10) });
  const response = await cache.store("large", new Response(text, { headers: { "content-type": "application/json" } }));
  expect(await response.text()).toBe(text);
  expect(cache.get("large")).toBeUndefined();
  await cache.store("small", Response.json({ value: "ok" }));
  expect(await cache.get("small")!.json()).toEqual({ value: "ok" });
  await new Promise(resolve => setTimeout(resolve, 1_100));
  expect(cache.get("small")).toBeUndefined();
});
