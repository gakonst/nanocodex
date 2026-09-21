import { StringDecoder } from 'node:string_decoder';

// Private build-specific operational observation only. This is never an approval,
// capability, attestation, or authenticated host-identity boundary. The official
// GUI remains responsible for consent. A matched log describes one moment;
// its owner must invalidate readiness when the GUI/server connection is lost.
export const KNOWN_GUI_BUILD = '9922';
export const MAX_GUI_READY_LINE_BYTES = 16 * 1024;
const PREFIX = '[electron-message-handler] maybe_resume_success ';
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const KEY = /^[A-Za-z_][A-Za-z0-9_.-]*$/;
const BARE = /^[A-Za-z0-9_./:@+-]+$/;

function readValue(line, start) {
  const first = line[start];
  if (first === '"' || first === '[' || first === '{') {
    const stack = [];
    let quoted = false;
    let escaped = false;
    for (let index = start; index < line.length; index++) {
      const char = line[index];
      if (quoted) {
        if (escaped) escaped = false;
        else if (char === '\\') escaped = true;
        else if (char === '"') quoted = false;
      } else if (char === '"') quoted = true;
      else if (char === '[' || char === '{') stack.push(char);
      else if (char === ']' || char === '}') {
        if (stack.pop() !== (char === ']' ? '[' : '{')) return null;
      }
      if (!quoted && stack.length === 0) {
        const end = index + 1;
        if (end < line.length && line[end] !== ' ') return null;
        try { return { value: JSON.parse(line.slice(start, end)), end }; }
        catch { return null; }
      }
    }
    return null;
  }
  const space = line.indexOf(' ', start);
  const end = space === -1 ? line.length : space;
  const raw = line.slice(start, end);
  if (!BARE.test(raw)) return null;
  // Preserve the logger's scalar types, particularly boolean vs quoted "true".
  let value = raw;
  if (raw === 'true') value = true;
  else if (raw === 'false') value = false;
  else if (raw === 'null') value = null;
  else if (raw === 'undefined') value = undefined;
  else if (/^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?$/.test(raw)) value = Number(raw);
  return { value, end };
}

/** Parse one complete stdout line, without its terminating newline. */
export function parseGuiReady(line, expectedThreadId) {
  if (typeof expectedThreadId !== 'string' || !UUID.test(expectedThreadId)
      || typeof line !== 'string' || Buffer.byteLength(line) > MAX_GUI_READY_LINE_BYTES
      || /[\u0000-\u001f\u007f\u0085\u2028\u2029]/u.test(line) || !line.startsWith(PREFIX)) return false;
  const fields = new Map();
  let offset = PREFIX.length;
  while (offset < line.length) {
    const equal = line.indexOf('=', offset);
    if (equal === -1) return false;
    const key = line.slice(offset, equal);
    if (!KEY.test(key) || fields.has(key)) return false;
    const parsed = readValue(line, equal + 1);
    if (!parsed) return false;
    fields.set(key, parsed.value);
    if (parsed.end === line.length) break;
    offset = parsed.end + 1;
    if (offset === line.length || line[offset] === ' ') return false;
  }
  return fields.get('threadId') === expectedThreadId
    && fields.get('conversationId') === expectedThreadId
    && fields.get('vmEvent') === 'thread_resumed'
    && fields.get('assignedStreamRole') === 'owner'
    && fields.get('markedStreaming') === true;
}

/**
 * Optional bounded stdout framing for a single owned child generation.
 * Generation identity is supplied by the supervisor, never read from log text.
 * expect() must precede the GUI navigation. close() permanently invalidates it.
 * No historical files, stderr, incomplete lines, or cross-generation fragments
 * are accepted. The caller still owns timeouts and disconnect invalidation.
 */
export function createGuiReadiness({ generation, onReady }) {
  if (generation == null || typeof onReady !== 'function') throw new TypeError('generation and onReady are required');
  let decoder = new StringDecoder('utf8');
  let buffer = '';
  let discarding = false;
  let expectedThreadId;
  let matched = false;
  let closed = false;
  return {
    expect(threadId) {
      if (closed) throw new Error('GUI readiness observer is closed');
      if (typeof threadId !== 'string' || !UUID.test(threadId)) throw new TypeError('Expected a thread UUID');
      // An earlier partial line cannot become a new expectation's evidence.
      if (buffer.length || decoder.lastNeed) discarding = true;
      buffer = '';
      decoder = new StringDecoder('utf8');
      expectedThreadId = threadId;
      matched = false;
    },
    push(chunk, { channel, generation: sourceGeneration } = {}) {
      if (closed || channel !== 'stdout' || sourceGeneration !== generation) return;
      if (typeof chunk !== 'string' && !Buffer.isBuffer(chunk) && !(chunk instanceof Uint8Array)) throw new TypeError('Expected a stdout chunk');
      const text = decoder.write(Buffer.from(chunk));
      let offset = 0;
      while (offset < text.length) {
        const newline = text.indexOf('\n', offset);
        const end = newline === -1 ? text.length : newline;
        const part = text.slice(offset, end);
        if (!discarding) {
          if (Buffer.byteLength(buffer) + Buffer.byteLength(part) > MAX_GUI_READY_LINE_BYTES) {
            buffer = '';
            discarding = true;
          } else buffer += part;
        }
        if (newline === -1) break;
        const ready = !discarding && !matched && expectedThreadId != null && parseGuiReady(buffer, expectedThreadId);
        buffer = '';
        discarding = false;
        if (ready) {
          matched = true;
          onReady(expectedThreadId);
          if (closed) return;
        }
        offset = newline + 1;
      }
    },
    close() {
      closed = true;
      expectedThreadId = undefined;
      buffer = '';
      decoder = new StringDecoder('utf8');
    },
  };
}
