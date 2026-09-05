import { Wata, postMessage } from "wata";

import { UserRejectedRequestError } from "./Errors.mjs";

export function create(parameters) {
  const host = walletHost(parameters.host);
  let session;
  let queue = Promise.resolve();
  let state = Object.freeze({ accounts: Object.freeze([]), activeAccount: 0, accessKeys: Object.freeze([]) });

  function ensureSession() {
    if (session) return session;
    const next = Wata.create({
      transports: [postMessage({
        connect: "eager",
        host,
        target: ({ host: targetHost }) => {
          const target = parameters.target({ host: targetHost });
          if (!target) throw new Error("The Nanocodex Connect wallet host is unavailable.");
          return target;
        },
      })],
    }).start();
    session = next;
    void next.ready.catch(() => {
      if (session === next) session = undefined;
    });
    next.onClose(() => {
      if (session === next) session = undefined;
    });
    next.onNotification((event) => {
      if (event.method !== "accountsChanged" || !Array.isArray(event.params)) return;
      state = Object.freeze({
        accounts: Object.freeze(event.params
          .filter((address) => typeof address === "string")
          .map((address) => Object.freeze({ address }))),
        activeAccount: 0,
        accessKeys: Object.freeze([]),
      });
    });
    return next;
  }

  function request(request) {
    const result = queue.then(async () => {
      try {
        const sent = await ensureSession().send({
          method: request.method,
          params: request.params ?? [],
          ...(request.context ? { context: request.context } : {}),
        });
        if (request.method === "wallet_disconnect") {
          state = Object.freeze({ accounts: Object.freeze([]), activeAccount: 0, accessKeys: Object.freeze([]) });
        }
        return sent.result;
      } catch (error) {
        if (error?.code === 4001) {
          throw new UserRejectedRequestError(error.message, { cause: error });
        }
        throw error;
      }
    });
    queue = result.catch(() => undefined);
    return result;
  }

  return Object.freeze({
    prepare() {
      return ensureSession().ready;
    },
    async reset() {
      const current = session;
      session = undefined;
      state = Object.freeze({ accounts: Object.freeze([]), activeAccount: 0, accessKeys: Object.freeze([]) });
      await current?.close();
    },
    request,
    store: Object.freeze({ getState: () => state }),
  });
}

function walletHost(host) {
  if (typeof window === "undefined") return host;
  const url = new URL(host);
  url.searchParams.set("origin", window.location.origin);
  if (!url.searchParams.has("mode")) url.searchParams.set("mode", "iframe");
  return url.toString();
}
