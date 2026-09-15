// Independent incremental Node string decoder. Only four pending bytes survive
// a write; decoding uses the existing bounded codecs, with no native handles.
const {Buffer} = globalThis.__skyreModules;
const nativeDecoder = Symbol('kNativeDecoder');
const encodings = ['ascii', 'utf8', 'base64', 'utf16le', 'latin1', 'hex', null, 'base64url'];
const fail = (code, message) => { const error = new TypeError(message); error.code = code; throw error; };
const encodingName = value => {
  if (value === undefined || value === null || value === '') return 'utf8';
  if (typeof value !== 'string') return undefined;
  const name = value.toLowerCase();
  if (name === 'utf-8') return 'utf8';
  if (['utf-16le', 'ucs2', 'ucs-2'].includes(name)) return 'utf16le';
  if (name === 'binary') return 'latin1';
  return encodings.includes(name) ? name : undefined;
};
const received = value => {
  if (value === null || value === undefined) return String(value);
  if (typeof value === 'object') return 'an instance of ' + (value.constructor?.name || 'Object');
  if (typeof value === 'function') return 'function ' + (value.name || '');
  return 'type ' + typeof value + ' (' + String(value) + ')';
};
function StringDecoder(encoding) {
  const normalized = encodingName(encoding);
  if (normalized === undefined) fail('ERR_UNKNOWN_ENCODING', 'Unknown encoding: ' + String(encoding));
  this.encoding = normalized;
  const state = Buffer.alloc(7);
  state[6] = encodings.indexOf(normalized);
  this[nativeDecoder] = state;
}
const checkedState = decoder => {
  const state = decoder?.[nativeDecoder];
  if (!state) fail('ERR_INVALID_THIS', 'Value of "this" must be of type StringDecoder');
  // The original native backing buffer is observable. Reject corrupt lengths
  // instead of translating them into an out-of-bounds native decoder access.
  if (!Buffer.isBuffer(state) || state.length !== 7 || state[4] + state[5] > 4 || !encodings[state[6]]) {
    fail('ERR_INVALID_STATE', 'Invalid StringDecoder state');
  }
  return state;
};
const decode = (bytes, encoding) => {
  if (encoding === 'utf16le') {
    // Buffer's Rust UTF16 codec is lossy; StringDecoder must preserve individual
    // surrogate code units, including a high surrogate flushed at stream end.
    let result = '';
    for (let i = 0; i + 1 < bytes.length; i += 2) result += String.fromCharCode(bytes[i] | bytes[i + 1] << 8);
    return result;
  }
  return Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength).toString(encoding);
};
const flush = state => {
  const result = decode(state.subarray(0, state[5]), encodings[state[6]]);
  state[4] = state[5] = 0;
  return result;
};
const width = byte => byte >= 0xc0 && byte < 0xe0 ? 2 : byte >= 0xe0 && byte < 0xf0 ? 3 : byte >= 0xf0 && byte < 0xf8 ? 4 : 0;
StringDecoder.prototype.write = function write(buf) {
  if (typeof buf === 'string') return buf;
  if (!ArrayBuffer.isView(buf)) fail('ERR_INVALID_ARG_TYPE', 'The "buf" argument must be an instance of Buffer, TypedArray, or DataView. Received ' + received(buf));
  const state = checkedState(this);
  const bytes = new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength);
  if (bytes.length > 8 * 1024 * 1024) fail('ERR_OUT_OF_RANGE', 'StringDecoder input exceeds 8 MiB');
  const encoding = encodings[state[6]];
  let result = '', start = 0;
  if (state[4]) {
    while (state[4] && start < bytes.length) {
      if (encoding === 'utf8' && (bytes[start] & 0xc0) !== 0x80) break;
      state[state[5]++] = bytes[start++];
      state[4]--;
    }
    if (state[4] && start === bytes.length) return '';
    result = flush(state);
  }
  let end = bytes.length, total = 0;
  if (encoding === 'utf8') {
    // Hold only a trailing structural prefix. Overlong/surrogate/out-of-range
    // prefixes are decoded by the codec when completed, interrupted or flushed.
    let lead = end - 1;
    while (lead >= start && end - lead < 4 && (bytes[lead] & 0xc0) === 0x80) lead--;
    if (lead >= start && width(bytes[lead]) > end - lead) { total = width(bytes[lead]); end = lead; }
  } else if (encoding === 'utf16le') {
    if ((end - start) % 2) { total = 2; end--; }
    else if (end > start && bytes[end - 1] >= 0xd8 && bytes[end - 1] <= 0xdb) { total = 4; end -= 2; }
  } else if (encoding === 'base64' || encoding === 'base64url') {
    const pending = (end - start) % 3;
    if (pending) { total = 3; end -= pending; }
  }
  if (total) {
    state[5] = bytes.length - end;
    state[4] = total - state[5];
    state.set(bytes.subarray(end), 0);
  }
  return result + decode(bytes.subarray(start, end), encoding);
};
StringDecoder.prototype.end = function end(buf) {
  const result = buf === undefined ? '' : this.write(buf);
  const state = checkedState(this);
  return state[5] ? result + flush(state) : result;
};
StringDecoder.prototype.text = function text(buf, offset) {
  const state = checkedState(this);
  state[4] = state[5] = 0;
  return this.write(buf.slice(offset));
};
Object.defineProperties(StringDecoder.prototype, {
  lastChar: {enumerable: true, configurable: true, get() { return checkedState(this).subarray(0, 4); }},
  lastNeed: {enumerable: true, configurable: true, get() { return checkedState(this)[4]; }},
  lastTotal: {enumerable: true, configurable: true, get() { const state = checkedState(this); return state[4] + state[5]; }},
});
export {StringDecoder};
export default {StringDecoder};
