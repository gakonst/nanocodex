import { WebSocketServer } from "ws";

export async function startResponsesServer() {
  const websocketServer = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise((resolve, reject) => {
    websocketServer.once("listening", resolve);
    websocketServer.once("error", reject);
  });
  const queued = [];
  const waiters = [];
  let connections = 0;
  websocketServer.on("connection", (socket, request) => {
    connections += 1;
    socket.request = request;
    const resolve = waiters.shift();
    if (resolve) resolve(socket);
    else queued.push(socket);
  });
  return Object.freeze({
    websocketServer,
    get connections() { return connections; },
    get url() {
      return `ws://127.0.0.1:${websocketServer.address().port}`;
    },
    nextConnection() {
      if (queued.length) return Promise.resolve(queued.shift());
      return new Promise((resolve) => waiters.push(resolve));
    },
    close() {
      for (const socket of websocketServer.clients) socket.terminate();
      return new Promise((resolve, reject) => {
        websocketServer.close((error) => error ? reject(error) : resolve());
      });
    },
  });
}

export function messageReader(socket) {
  const messages = [];
  const waiters = [];
  socket.on("message", (data) => {
    const message = JSON.parse(data.toString("utf8"));
    const resolve = waiters.shift();
    if (resolve) resolve(message);
    else messages.push(message);
  });
  return Object.freeze({
    next() {
      if (messages.length) return Promise.resolve(messages.shift());
      return new Promise((resolve) => waiters.push(resolve));
    },
  });
}

export function sendWarmup(socket, responseId) {
  send(socket, {
    type: "response.completed",
    response: { id: responseId, usage: null },
  });
}

export function sendFinal(socket, responseId, text) {
  sendCompleted(socket, responseId, [{
    type: "message",
    role: "assistant",
    content: [{ type: "output_text", text }],
  }]);
}

export function sendCompleted(socket, responseId, output) {
  send(socket, {
    type: "response.completed",
    response: {
      id: responseId,
      status: "completed",
      output,
      usage: {
        input_tokens: 10,
        input_tokens_details: { cached_tokens: 5 },
        output_tokens: 2,
        output_tokens_details: { reasoning_tokens: 1 },
        total_tokens: 12,
      },
    },
  });
}

export function sendCompaction(socket, responseId) {
  send(socket, {
    type: "response.output_item.done",
    item: {
      id: "cmp-js-binding",
      type: "compaction",
      encrypted_content: "opaque-js-summary",
    },
  });
  sendCompleted(socket, responseId, []);
}

export function send(socket, message) {
  socket.send(JSON.stringify(message));
}

export function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return Object.freeze({ promise, reject, resolve });
}
