import { createServer } from "node:http";
import { randomBytes } from "node:crypto";

import {
  LOCAL_OAUTH_RELAY_HMAC_KEY,
  LOCAL_OAUTH_RELAY_HOST,
  LOCAL_OAUTH_RELAY_ORIGIN,
  LOCAL_OAUTH_RELAY_PORT,
  localOAuthRelayCallbackRedirect,
  localOAuthRelayChallengeProof,
} from "./oauth-relay.mjs";

export async function startLocalOAuthRelay() {
  const server = createServer(async (request, response) => {
    response.setHeader("cache-control", "no-store");
    response.setHeader("referrer-policy", "no-referrer");
    response.setHeader("x-content-type-options", "nosniff");
    if (typeof request.url !== "string") return json(response, 404, { error: "not_found" });

    let url;
    try { url = new URL(request.url, LOCAL_OAUTH_RELAY_ORIGIN); } catch {
      return json(response, 400, { error: "invalid_callback" });
    }
    if (request.method === "GET" && url.pathname === "/api/oauth-callback-relay") {
      try {
        return json(response, 200, {
          service: "nanocodex-local-oauth-relay",
          status: "ok",
          version: 1,
          proof: await localOAuthRelayChallengeProof(
            url.searchParams.get("challenge"),
            LOCAL_OAUTH_RELAY_HMAC_KEY,
          ),
        });
      } catch {
        return json(response, 400, { error: "invalid_challenge" });
      }
    }
    if (request.method !== "GET") return json(response, 404, { error: "not_found" });

    const destination = await localOAuthRelayCallbackRedirect(url, LOCAL_OAUTH_RELAY_HMAC_KEY);
    if (!destination) return json(response, 400, { error: "invalid_callback" });
    response.statusCode = 303;
    response.setHeader("location", destination.href);
    response.end();
  });
  server.on("clientError", (_error, socket) => {
    socket.end("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
  });
  try {
    await listen(server);
  } catch (error) {
    if (error?.code === "EADDRINUSE" && await compatibleRelayIsRunning()) {
      return followExistingRelay();
    }
    throw error;
  }
  return {
    close: () => new Promise((resolve, reject) => {
      server.close((error) => error ? reject(error) : resolve());
    }),
  };
}

function listen(server) {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(LOCAL_OAUTH_RELAY_PORT, LOCAL_OAUTH_RELAY_HOST, () => {
      server.off("error", reject);
      resolve();
    });
  });
}

function followExistingRelay() {
  let closed = false;
  let acquiring = false;
  let owned;
  const timer = setInterval(async () => {
    if (closed || acquiring || owned || await compatibleRelayIsRunning()) return;
    acquiring = true;
    try {
      const relay = await startLocalOAuthRelay();
      if (closed) await relay.close();
      else owned = relay;
    } catch {
      // The fixed callback port changed hands while reacquiring; retry while Vite lives.
    } finally {
      acquiring = false;
    }
  }, 1_000);
  timer.unref();
  return {
    async close() {
      closed = true;
      clearInterval(timer);
      await owned?.close();
    },
  };
}

async function compatibleRelayIsRunning() {
  const challenge = randomBytes(32).toString("base64url");
  const expected = await localOAuthRelayChallengeProof(challenge, LOCAL_OAUTH_RELAY_HMAC_KEY);
  try {
    const response = await fetch(
      `${LOCAL_OAUTH_RELAY_ORIGIN}/api/oauth-callback-relay?challenge=${challenge}`,
      { signal: AbortSignal.timeout(1_000) },
    );
    const body = await response.json();
    return response.ok
      && body?.service === "nanocodex-local-oauth-relay"
      && body.status === "ok"
      && body.version === 1
      && body.proof === expected;
  } catch {
    return false;
  }
}

function json(response, status, body) {
  response.statusCode = status;
  response.setHeader("content-type", "application/json; charset=utf-8");
  response.end(`${JSON.stringify(body)}\n`);
}
