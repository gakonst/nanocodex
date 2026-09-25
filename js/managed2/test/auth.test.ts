import { describe, expect, it } from "vitest";
import { authenticate } from "../src/auth";

const synthetic = `ncx2_${"A".repeat(43)}`;
const digest = async (key: string) => btoa(String.fromCharCode(...new Uint8Array(
  await crypto.subtle.digest("SHA-256", new TextEncoder().encode(key)),
))).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");

describe("local API key auth", () => {
  it("maps a key to its owner and rejects absent or mismatched keys without an account lookup", async () => {
    const hashes = JSON.stringify({ [await digest(synthetic)]: "synthetic-user" });
    const request = (key?: string) => new Request("https://api.example/v1/agents", {
      headers: key ? { authorization: `Bearer ${key}` } : {},
    });
    expect(await authenticate(request(synthetic), hashes)).toEqual({ sub: "synthetic-user" });
    expect(await authenticate(request(), hashes)).toBeUndefined();
    expect(await authenticate(request(`${synthetic.slice(0, -1)}B`), hashes)).toBeUndefined();
    expect(await authenticate(request(synthetic), undefined)).toBeUndefined();
    expect(await authenticate(request(synthetic), JSON.stringify({ [await digest(synthetic)]: "other-user" })))
      .toEqual({ sub: "other-user" }); // secret updates replace the isolate cache
  });
});
