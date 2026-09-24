import { env } from "cloudflare:workers";
import { runInDurableObject, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { EgressEnv } from "../src/egress";
import { UserConnectorBroker } from "../src/connector-broker";
import { CredentialVault, type EncryptedEnvelope } from "../src/credential-vault";
import {
  SPOTIFY_SCOPES, SPOTIFY_LOOPBACK_CLIENT_ID, type MusicProviderId,
} from "../src/connectors/music";

const workerEnv = env as unknown as EgressEnv;

describe.each(["spotify", "soundcloud"] as const)("%s accounts", (provider) => {
  const origin = provider === "spotify" ? "https://api.spotify.com" : "https://api.soundcloud.com";
  const path = provider === "spotify" ? "/v1/me/playlists" : "/me/playlists";

  it("connects multiple accounts, refreshes, reads and writes as the selected user, and disconnects", async () => {
    const user = `music-${provider}`;
    const alpha = await connect(user, provider, `${provider}-alpha`);
    const beta = await connect(user, provider, `${provider}-beta`);
    const subject = (provider === "spotify" ? "S" : "C").repeat(43);
    expect((await control(`/subjects/${subject}`, "PUT", { user_id: user })).status).toBe(200);
    const status = await SELF.fetch(`https://broker.test/users/${user}/connectors`);
    const listing = await status.json<{ connectors: Record<string, { connections: { id: string }[] }> }>();
    expect(listing.connectors[provider]!.connections.map(({ id }) => id)).toEqual([alpha, beta]);
    expect(JSON.stringify(listing)).not.toContain("music-secret-");
    const baseHeaders = { authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "x-nanocodex-subject": subject };
    expect((await SELF.fetch(origin + path, { headers: baseHeaders })).status).toBe(409);
    for (const [id, account] of [[alpha, `${provider}-alpha`], [beta, `${provider}-beta`]]) {
      const headers = { ...baseHeaders, "x-nanocodex-connector-connection": id!, "content-type": "application/json" };
      for (const method of ["GET", "POST", "PUT", "DELETE"]) {
        const body = method === "GET" ? undefined : JSON.stringify({ title: "New playlist" });
        const response = await SELF.fetch(origin + path, { method, headers, ...(body === undefined ? {} : { body }) });
        expect(response.status).toBe(200);
        expect(await response.json()).toEqual({ account, refreshed: true, method, body: body ?? "" });
      }
    }
    const headers = { ...baseHeaders, "x-nanocodex-connector-connection": alpha };
    for (const denied of ["/oauth/token", "/sign-out", "/disconnect", `${path}?access_token=secret`, "/v1/%252e%252e/token"]) {
      expect((await workerEnv.USER_CONNECTORS.getByName(user).fetch(origin + denied, { headers })).status).toBe(403);
    }
    expect((await SELF.fetch(origin + path + "/redirect", { headers })).status).toBe(502);
    // Opaque selectors cannot select another Nanocodex user's stored credentials.
    expect((await workerEnv.USER_CONNECTORS.getByName(`${user}-other`).fetch(origin + path, { headers })).status).toBe(404);
    await runInDurableObject(workerEnv.USER_CONNECTORS.getByName(user), async (_instance: UserConnectorBroker, state) => {
      const row = await state.storage.get<{ envelope: EncryptedEnvelope }>("connector-state");
      expect(JSON.stringify(row)).not.toContain("music-secret-");
      const opened = await new CredentialVault(workerEnv, `connectors/${state.id.toString()}`).open<{
        connections: Record<string, Record<string, { refreshToken: string }>>;
      }>(row!.envelope);
      expect(opened.value.connections[provider]![alpha]!.refreshToken).toBe(
        `music-refresh-${provider}-alpha${provider === "soundcloud" ? "-rotated" : ""}`,
      );
    });
    expect((await SELF.fetch(`https://broker.test/users/${user}/connectors/${provider}/connections/${alpha}`, { method: "DELETE" })).status).toBe(204);
    expect((await SELF.fetch(origin + path, { headers })).status).toBe(404);
    expect((await SELF.fetch(origin + path, { headers: { ...headers, "x-nanocodex-connector-connection": beta } })).status).toBe(200);
  });

  it("rejects invalid OAuth state, replay, and expired grants without losing accounts on transient refresh failures", async () => {
    for (const failure of ["denied", "unavailable"]) {
      const user = `${provider}-${failure}`;
      const id = await connect(user, provider, user);
      const broker = workerEnv.USER_CONNECTORS.getByName(user);
      const request = () => broker.fetch(origin + path, { headers: { "x-nanocodex-connector-connection": id } });
      expect((await request()).status).toBe(failure === "denied" ? 409 : 503);
      expect((await request()).status).toBe(failure === "denied" ? 404 : 503);
    }
  });
});

async function connect(user: string, provider: MusicProviderId, code: string): Promise<string> {
  const route = `/users/${user}/connectors/${provider}`;
  const started = await control(route, "POST", {
    redirect_uri: `https://nanocodex.test/v1/connectors/${provider}/callback`, return_to: "/profile",
  });
  expect(started.status).toBe(200);
  const url = new URL((await started.json<{ authorization_url: string }>()).authorization_url);
  expect(url.origin).toBe(provider === "spotify" ? "https://accounts.spotify.com" : "https://secure.soundcloud.com");
  expect(url.searchParams.get("code_challenge_method")).toBe("S256");
  expect(url.searchParams.get("code_challenge")).toMatch(/^[A-Za-z0-9_-]{43}$/);
  if (provider === "spotify") expect(url.searchParams.get("scope")?.split(" ")).toEqual(SPOTIFY_SCOPES);
  else expect(url.searchParams.get("display")).toBe("popup");
  const state = url.searchParams.get("state");
  expect((await control(route + "/callback", "POST", { code, state: "wrong" })).status).toBe(400);
  const callback = await control(route + "/callback", "POST", { code, state });
  expect(callback.status).toBe(200);
  expect((await control(route + "/callback", "POST", { code, state })).status).toBe(400);
  const result = await callback.json<{ connection_id: string }>();
  expect(result.connection_id).toMatch(/^[A-Za-z0-9_-]{43}$/);
  return result.connection_id;
}

function control(path: string, method: string, body: unknown) {
  return SELF.fetch(`https://broker.test${path}`, { method, headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
}

it("binds native loopback OAuth to the owner, fixed registration, one-time state and refresh client", async () => {
  const route = "/users/phone-owner/connectors/spotify";
  const start = await control(route, "POST", { flow: "ncspot_loopback", return_to: "/profile", redirect_uri: "https://attacker.test" });
  expect(start.status).toBe(200);
  const value = await start.json<{ authorization_url: string }>();
  const url = new URL(value.authorization_url);
  expect(url.searchParams.get("client_id")).toBe(SPOTIFY_LOOPBACK_CLIENT_ID);
  expect(url.searchParams.get("redirect_uri")).toBe("http://127.0.0.1:8989/login");
  expect(value).not.toHaveProperty("code_verifier");
  const body = { flow: "ncspot_loopback", state: url.searchParams.get("state"), code: "loopback-account" };
  expect((await control("/users/other-phone/connectors/spotify/callback", "POST", body)).status).toBe(400);
  expect((await control(route + "/callback", "POST", { ...body, flow: undefined })).status).toBe(400);
  const callback = await control(route + "/callback", "POST", body);
  expect(callback.status).toBe(200);
  expect((await callback.json<{ connected: boolean }>()).connected).toBe(true);
  expect((await control(route + "/callback", "POST", body)).status).toBe(400);
  const read = await workerEnv.USER_CONNECTORS.getByName("phone-owner").fetch("https://api.spotify.com/v1/me/playlists");
  expect(read.status).toBe(200);
  expect(await read.json()).toMatchObject({ account: "loopback-account", refreshed: true });
  expect((await control("/users/phone-owner/connectors/soundcloud", "POST", { flow: "ncspot_loopback", return_to: "/profile" })).status).toBe(400);
});


it("binds SoundCloud phone OAuth to its registered loopback and keeps the app secret in the broker", async () => {
  const route = "/users/soundcloud-phone-owner/connectors/soundcloud";
  const start = await control(route, "POST", { flow: "soundcloud_loopback", return_to: "/profile", redirect_uri: "https://attacker.test" });
  expect(start.status).toBe(200);
  const value = await start.json<{ authorization_url: string }>();
  const url = new URL(value.authorization_url);
  expect(url.origin).toBe("https://secure.soundcloud.com");
  expect(url.searchParams.get("client_id")).toBe("soundcloud-client-id");
  expect(url.searchParams.get("redirect_uri")).toBe("http://127.0.0.1:8788/callback");
  expect(url.searchParams.get("display")).toBe("popup");
  expect(JSON.stringify(value)).not.toMatch(/client_secret|code_verifier/);
  const body = { flow: "soundcloud_loopback", state: url.searchParams.get("state"), code: "soundcloud-loopback-account" };
  expect((await control("/users/other-phone/connectors/soundcloud/callback", "POST", body)).status).toBe(400);
  expect((await control(route + "/callback", "POST", { ...body, flow: undefined })).status).toBe(400);
  expect((await control(route + "/callback", "POST", { ...body, flow: "ncspot_loopback" })).status).toBe(400);
  expect((await control(route + "/callback", "POST", body)).status).toBe(200);
  expect((await control(route + "/callback", "POST", body)).status).toBe(400);
  const read = await workerEnv.USER_CONNECTORS.getByName("soundcloud-phone-owner").fetch("https://api.soundcloud.com/me/playlists");
  expect(read.status).toBe(200);
  expect(await read.json()).toMatchObject({ account: "soundcloud-loopback-account", refreshed: true });
  expect((await control("/users/soundcloud-phone-owner/connectors/spotify", "POST", { flow: "soundcloud_loopback", return_to: "/profile" })).status).toBe(400);
});

 it("resolves full SoundCloud streams only on the explicit metadata route", async () => {
  const user = "soundcloud-stream-resolution";
  const connection = await connect(user, "soundcloud", "soundcloud-alpha");
  const broker = workerEnv.USER_CONNECTORS.getByName(user);
  const url = "https://api.soundcloud.com/tracks/soundcloud:tracks:123/streams/safe/hls";
  const headers = { "x-nanocodex-connector-connection": connection, "x-nanocodex-resolve-stream": "1" };
  const response = await broker.fetch(url, { headers });
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual({ url: "https://media.sndcdn.com/audio.m3u8?Policy=signed" });
  const modern = await broker.fetch(url.replace("/safe/", "/modern/"), { headers });
  expect(modern.status).toBe(200);
  expect(await modern.json()).toEqual({ url: "https://playback.media-streaming.soundcloud.cloud/track/aac_160k/uuid/playlist.m3u8?Policy=signed" });
  expect((await broker.fetch(url, { headers: { "x-nanocodex-connector-connection": connection } })).status).toBe(502);
  for (const variant of [url.replace("/safe/", "/lookalike/"), url.replace("/safe/", "/evil/"), url.replace("/safe/", "/credential/"), url.replace("/hls", "/http-preview")]) {
    expect((await broker.fetch(variant, { headers })).status).toBe(502);
  }
 });
