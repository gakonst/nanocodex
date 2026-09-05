const hosts = new Map();
const RAW_SUBSCRIPTION = Symbol.for("nanocodex.chatgpt.subscription");
const DEFAULT_MAX_RESPONSE_BYTES = 16 * 1024;

export async function openSubscription(options, openRaw, { replaceHost = false } = {}) {
  if (!options || typeof options !== "object") {
    throw new TypeError("ChatGptSubscription.open requires options");
  }
  const id = requiredId(options.id);
  const fetch = options.fetch;
  const host = {
    store: options.store,
    // Cloudflare's global fetch requires the Worker global as its receiver.
    // Keep the stored capability receiver-neutral so calling it through the
    // host record cannot accidentally bind `this` to that record.
    fetch: fetch === undefined
      ? (...args) => globalThis.fetch(...args)
      : (...args) => fetch(...args),
    references: 0,
  };
  validateHost(host);
  bind(id, host, replaceHost);
  try {
    const raw = await openRaw(JSON.stringify({
      id,
      ...(options.issuer === undefined ? {} : { issuer: options.issuer }),
      ...(options.seed === undefined ? {} : { seed: options.seed }),
    }));
    const view = Object.freeze({
      id,
      [RAW_SUBSCRIPTION]: raw,
      async startLogin() {
        return JSON.parse(await raw.startLogin());
      },
      async status() {
        return JSON.parse(await raw.status());
      },
      async credential() {
        return parseCredential(await raw.credential());
      },
      async recover(rejectedRevision) {
        return parseCredential(await raw.recover(revision(
          rejectedRevision,
          "rejected credential revision",
        )));
      },
      async logout() {
        await raw.logout();
      },
      dispose() {
        release(id, host);
        raw.free();
      },
    });
    host.references += 1;
    return view;
  } catch (error) {
    release(id, host);
    throw error;
  }
}

export function rawSubscription(subscription) {
  const raw = subscription?.[RAW_SUBSCRIPTION];
  if (!raw || typeof raw.status !== "function") {
    throw new TypeError("subscription must be an open ChatGptSubscription");
  }
  return raw;
}

export async function load(subscriptionId) {
  const stored = await requiredHost(subscriptionId).store.load(subscriptionId);
  if (!stored || typeof stored !== "object") {
    throw new TypeError("subscription store load() must return { revision, payload? }");
  }
  return JSON.stringify({
    revision: revision(stored.revision, "subscription load revision"),
    ...(stored.payload === undefined || stored.payload === null
      ? {}
      : { payload: requiredString(stored.payload, "subscription payload") }),
  });
}

export async function compareAndSwap(subscriptionId, expectedRevision, payload) {
  const result = await requiredHost(subscriptionId).store.compareAndSwap(subscriptionId, {
    expectedRevision,
    payload,
  });
  if (result?.status === "committed") {
    return JSON.stringify({
      status: "committed",
      revision: revision(result.revision, "subscription commit revision"),
    });
  }
  if (result?.status === "conflict") {
    return JSON.stringify({
      status: "conflict",
      actual_revision: revision(result.actualRevision, "subscription conflict revision"),
    });
  }
  throw new TypeError("subscription compareAndSwap() must return committed or conflict");
}

export async function request(subscriptionId, encoded) {
  const host = requiredHost(subscriptionId);
  const request = JSON.parse(encoded);
  if (request.method !== "POST") throw new TypeError("subscription HTTP method must be POST");
  const url = new URL(request.url);
  if (url.origin !== "https://auth.openai.com" && url.hostname !== "127.0.0.1") {
    throw new TypeError("subscription HTTP is restricted to auth.openai.com");
  }
  const maxResponseBytes = positiveInteger(
    request.maxResponseBytes ?? DEFAULT_MAX_RESPONSE_BYTES,
    "subscription response limit",
  );
  const response = await host.fetch(url, {
    method: "POST",
    headers: { "content-type": requiredString(request.contentType, "subscription content type") },
    body: requiredString(request.body, "subscription request body"),
    redirect: "manual",
  });
  if (!(response instanceof Response)) {
    throw new TypeError("subscription fetch() must return a Response");
  }
  return JSON.stringify({
    status: response.status,
    body: await readBoundedText(response, maxResponseBytes),
  });
}

function bind(id, host, replaceHost) {
  if (!replaceHost && hosts.has(id)) {
    throw new Error(`ChatGPT subscription is already open: ${id}`);
  }
  hosts.set(id, host);
}

function release(id, host) {
  if (hosts.get(id) !== host) return;
  if (host.references > 0) host.references -= 1;
  if (host.references === 0) hosts.delete(id);
}

function requiredHost(id) {
  const host = hosts.get(id);
  if (!host) throw new Error(`no host owns ChatGPT subscription: ${id}`);
  return host;
}

function validateHost(host) {
  if (!host.store || typeof host.store.load !== "function"
      || typeof host.store.compareAndSwap !== "function") {
    throw new TypeError("ChatGPT subscription requires load and compareAndSwap storage");
  }
  if (typeof host.fetch !== "function") {
    throw new TypeError("ChatGPT subscription requires a fetch capability");
  }
}

async function readBoundedText(response, limit) {
  if (!response.body) return "";
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let size = 0;
  let body = "";
  while (true) {
    const { done, value } = await reader.read();
    if (done) return body + decoder.decode();
    size += value.byteLength;
    if (size > limit) {
      await reader.cancel();
      throw new Error(`subscription response exceeded ${limit} bytes`);
    }
    body += decoder.decode(value, { stream: true });
  }
}

function revision(value, name) {
  if (typeof value !== "string" || !/^(0|[1-9][0-9]*)$/.test(value)) {
    throw new TypeError(`${name} must be an unsigned decimal string`);
  }
  return value;
}

function requiredId(value) {
  if (typeof value !== "string" || !/^[A-Za-z0-9._:-]{1,200}$/.test(value)) {
    throw new TypeError("subscription id must be a bounded stable identifier");
  }
  return value;
}

function requiredString(value, name) {
  if (typeof value !== "string") throw new TypeError(`${name} must be a string`);
  return value;
}

function positiveInteger(value, name) {
  if (!Number.isSafeInteger(value) || value <= 0) throw new TypeError(`${name} must be positive`);
  return value;
}

function parseCredential(encoded) {
  const credential = JSON.parse(encoded);
  if (credential?.kind !== "chatgpt" || typeof credential.accessToken !== "string"
      || typeof credential.accountId !== "string" || typeof credential.fedramp !== "boolean") {
    throw new TypeError("Rust returned an invalid ChatGPT credential");
  }
  return Object.freeze({
    ...credential,
    revision: revision(credential.revision, "credential revision"),
  });
}
