import { reportError, subscribeAgentEvents } from "../internal.mjs";
import { utf8ByteLength } from "../runtime/utf8.mjs";

const MAX_BUFFERED_EVENTS = 4_096;
const MAX_BUFFERED_EVENT_BYTES = 32 * 1024 * 1024;

export function watch(agent, options = {}) {
  const listeners = new Set();
  const iterators = new Set();
  let unsubscribe;
  let closed = false;

  const emit = (event, encodedLength, encodedEvent, agentId) => {
    for (const listener of listeners) {
      try {
        listener(event, encodedLength, encodedEvent, agentId);
      } catch (error) {
        reportError(error);
      }
    }
    for (const iterator of iterators) iterator.push(event, encodedLength);
  };

  const start = () => {
    if (closed || unsubscribe) return;
    unsubscribe = subscribeAgentEvents(agent, emit, options, () => watcher.off());
  };

  const stopIfIdle = () => {
    if (listeners.size || iterators.size) return;
    unsubscribe?.();
    unsubscribe = undefined;
  };

  const watcher = {
    onEvent(listener) {
      if (typeof listener !== "function") throw new TypeError("events.watch.onEvent requires a listener");
      if (closed) return () => {};
      listeners.add(listener);
      start();
      return () => {
        listeners.delete(listener);
        stopIfIdle();
      };
    },
    off() {
      if (closed) return;
      closed = true;
      unsubscribe?.();
      unsubscribe = undefined;
      listeners.clear();
      for (const iterator of [...iterators]) iterator.end();
      iterators.clear();
    },
    [Symbol.asyncIterator]() {
      if (closed) return emptyIterator();
      const iterator = eventIterator(() => {
        iterators.delete(iterator);
        stopIfIdle();
      });
      iterators.add(iterator);
      start();
      return iterator;
    },
  };
  return Object.freeze(watcher);
}

function eventIterator(onEnd) {
  const queue = [];
  let head = 0;
  let bufferedBytes = 0;
  const pending = [];
  let pendingHead = 0;
  let ended = false;
  let failure;
  let failureReported = false;
  let detached = false;

  const detach = () => {
    if (detached) return;
    detached = true;
    onEnd();
  };

  const iterator = {
    push(event, encodedLength) {
      if (ended || failure) return;
      if (pendingHead < pending.length) {
        const resolve = pending[pendingHead++];
        if (pendingHead === pending.length) {
          pending.length = 0;
          pendingHead = 0;
        }
        resolve({ done: false, value: event });
      } else {
        const bytes = encodedLength ?? utf8ByteLength(JSON.stringify(event));
        if (
          queue.length - head >= MAX_BUFFERED_EVENTS
          || bufferedBytes + bytes > MAX_BUFFERED_EVENT_BYTES
        ) {
          failure = new RangeError(
            `event iterator exceeded its private buffer of ${MAX_BUFFERED_EVENTS} events or `
              + `${MAX_BUFFERED_EVENT_BYTES} encoded bytes`,
          );
          detach();
          return;
        }
        queue.push({ bytes, event });
        bufferedBytes += bytes;
      }
    },
    end() {
      if (ended) return;
      ended = true;
      detach();
      while (pendingHead < pending.length) {
        pending[pendingHead++]({ done: true, value: undefined });
      }
      pending.length = 0;
      pendingHead = 0;
      queue.length = 0;
      head = 0;
      bufferedBytes = 0;
    },
    next() {
      if (head < queue.length) {
        const entry = queue[head++];
        bufferedBytes -= entry.bytes;
        if (head === queue.length) {
          queue.length = 0;
          head = 0;
        }
        return Promise.resolve({ done: false, value: entry.event });
      }
      if (failure && !failureReported) {
        failureReported = true;
        return Promise.reject(failure);
      }
      if (ended || failure) return Promise.resolve({ done: true, value: undefined });
      if (pending.length - pendingHead >= MAX_BUFFERED_EVENTS) {
        return Promise.reject(new RangeError(
          `event iterator exceeded its private buffer of ${MAX_BUFFERED_EVENTS} pending reads`,
        ));
      }
      return new Promise((resolve) => { pending.push(resolve); });
    },
    return() {
      iterator.end();
      return Promise.resolve({ done: true, value: undefined });
    },
    [Symbol.asyncIterator]() { return this; },
  };
  return iterator;
}

function emptyIterator() {
  return {
    next: () => Promise.resolve({ done: true, value: undefined }),
    return: () => Promise.resolve({ done: true, value: undefined }),
    [Symbol.asyncIterator]() { return this; },
  };
}
