/** Multiplex an already authenticated Responses WebSocket. The caller owns its lifetime. */
export function multiplex(socket) {
  if (!socket || typeof socket.addEventListener !== "function" || typeof socket.send !== "function") throw new TypeError("multiplex requires a WebSocket");
  const lanes = new Map();
  const used = new Set();
  const broadcast = event => { for (const lane of lanes.values()) lane.dispatchEvent(event()); };
  const onMessage = event => {
    try {
      const data = typeof event.data === "string" ? event.data
        : new TextDecoder().decode(event.data instanceof ArrayBuffer ? event.data : new Uint8Array(event.data.buffer, event.data.byteOffset, event.data.byteLength));
      const message = JSON.parse(data);
      if (message.stream_id !== undefined) lanes.get(message.stream_id)?.dispatchEvent(new MessageEvent("message", { data }));
      else if (message.type === "error") broadcast(() => new MessageEvent("message", { data }));
    } catch { broadcast(() => new Event("error")); }
  };
  const onOpen = () => broadcast(() => new Event("open"));
  const onError = () => broadcast(() => new Event("error"));
  const onClose = event => { for (const lane of [...lanes.values()]) lane.close(event.code, event.reason); };
  socket.addEventListener("message", onMessage);
  socket.addEventListener("open", onOpen);
  socket.addEventListener("error", onError);
  socket.addEventListener("close", onClose);
  return Object.freeze({
    lane(id) {
      if (typeof id !== "string" || !/^[A-Za-z0-9_.-]{1,256}$/.test(id)) throw new TypeError("invalid Responses stream ID");
      if (lanes.has(id)) return lanes.get(id);
      if (!used.has(id) && used.size >= 32) throw new RangeError("a Responses socket supports 32 distinct named streams");
      if (socket.readyState > 1) throw new Error("Responses socket is closed");
      used.add(id);
      let closed = false;
      const lane = new EventTarget();
      for (const type of ["open", "message", "error", "close"]) {
        let handler = null;
        Object.defineProperty(lane, `on${type}`, { configurable: true, enumerable: true,
          get: () => handler,
          set(value) { if (handler) lane.removeEventListener(type, handler); handler = typeof value === "function" ? value : null; if (handler) lane.addEventListener(type, handler); },
        });
      }
      Object.defineProperties(lane, {
        url: { get: () => socket.url },
        protocol: { get: () => socket.protocol ?? "" },
        extensions: { get: () => socket.extensions ?? "" },
        CONNECTING: { value: 0 }, OPEN: { value: 1 }, CLOSING: { value: 2 }, CLOSED: { value: 3 },
        readyState: { get: () => closed ? 3 : socket.readyState },
        bufferedAmount: { get: () => socket.bufferedAmount },
        binaryType: { get: () => socket.binaryType, set: value => { socket.binaryType = value; } },
        send: { value: data => {
          if (closed || socket.readyState !== 1) throw new Error("Responses lane is not open");
          const body = JSON.parse(data);
          if (body.type !== "response.create") throw new TypeError("multiplexed lanes accept response.create only");
          if (body.stream_id !== undefined && body.stream_id !== id) throw new TypeError("request belongs to a different lane");
          delete body.stream; delete body.background;
          // Lineage is caller-owned; never substitute the most recent response on another lane.
          socket.send(JSON.stringify({ ...body, stream_id: id }));
        } },
        close: { value: (code = 1000, reason = "lane released") => {
          if (closed) return; closed = true; lanes.delete(id);
          const event = new Event("close"); Object.assign(event, { code, reason, wasClean: code === 1000 });
          lane.dispatchEvent(event);
        } },
      });
      lanes.set(id, lane);
      return lane;
    },
    close(code = 1000, reason = "Responses pool closed") {
      for (const lane of [...lanes.values()]) lane.close(code, reason);
      socket.removeEventListener("message", onMessage); socket.removeEventListener("open", onOpen);
      socket.removeEventListener("error", onError); socket.removeEventListener("close", onClose);
      socket.close(code, reason);
    },
  });
}
