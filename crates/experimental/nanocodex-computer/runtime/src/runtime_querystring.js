// Independently authored query-string codec. Pure JavaScript state and bounded
// existing Buffer codecs; no Node implementation or host I/O is loaded.
const {Buffer} = globalThis.__skyreModules;
const limit = 8 * 1024 * 1024;
const failure = (name, message, code) => {
  const error = name === 'RangeError' ? new RangeError(message) : new TypeError(message);
  if (code !== undefined) error.code = code;
  throw error;
};
const bounded = length => {
  if (length > limit) failure('RangeError', 'Querystring input exceeds 8 MiB', 'ERR_OUT_OF_RANGE');
};
const arrayToString = Array.prototype.toString, arrayJoin = Array.prototype.join;
const ordinaryArray = value => Array.isArray(value) && value[Symbol.toPrimitive] === undefined && value.toString === arrayToString && value.join === arrayJoin;
const arrayText = (value, active = new Set()) => {
  if (active.has(value)) return '';
  active.add(value);
  let text = '';
  for (let i = 0; i < value.length; i++) {
    if (i) text += ',';
    const item = value[i];
    if (item !== null && item !== undefined) text += ordinaryArray(item) ? arrayText(item, active) : concatenated(item);
  }
  active.delete(value);
  return text;
};
const propertyKey = value => ordinaryArray(value) ? arrayText(value) : value;
const concatenated = value => {
  if (typeof value === 'symbol') failure('TypeError', 'Cannot convert a Symbol value to a string');
  return ordinaryArray(value) ? arrayText(value) : '' + value;
};
const hex = code => code >= 48 && code <= 57 ? code - 48 : code >= 65 && code <= 70 ? code - 55 : code >= 97 && code <= 102 ? code - 87 : -1;
function unescapeBuffer(str, decodeSpaces) {
  if (str === null || str === undefined) failure('TypeError', "Cannot read properties of " + str + " (reading 'length')");
  const length = str.length;
  if (typeof length !== 'number') failure('TypeError', 'The "size" argument must be of type number. Received ' + String(length), 'ERR_INVALID_ARG_TYPE');
  if (length < 0 || !Number.isFinite(length)) failure('RangeError', 'Invalid querystring byte length', 'ERR_OUT_OF_RANGE');
  bounded(length);
  const bytes = [];
  for (let index = 0; index < length; index++) {
    let code = str.charCodeAt(index);
    if (code === 43 && decodeSpaces) code = 32;
    else if (code === 37 && index + 2 < length) {
      const first = hex(str.charCodeAt(index + 1)), second = hex(str.charCodeAt(index + 2));
      if (first >= 0 && second >= 0) { code = first * 16 + second; index += 2; }
    }
    bytes.push(code & 255);
  }
  return Buffer.from(bytes);
}
function qsUnescape(str, decodeSpaces) {
  if (typeof str === 'string') bounded(str.length);
  try { return decodeURIComponent(str); }
  catch { return unescapeBuffer(str, decodeSpaces).toString(); }
}
function qsEscape(str) {
  if (typeof str !== 'string') str = typeof str === 'object' ? String(str) : concatenated(str);
  bounded(str.length);
  // The captured codec combines any surrogate code unit with the following
  // unit's low ten bits, and rejects only a terminal surrogate. Keep this
  // legacy behavior local to querystring instead of changing the URI global.
  let text = '';
  for (let i = 0; i < str.length; i++) {
    const code = str.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdfff) {
      if (i + 1 === str.length) {
        const error = new URIError('URI malformed'); error.code = 'ERR_INVALID_URI'; throw error;
      }
      text += String.fromCodePoint(0x10000 + ((code & 1023) << 10) + (str.charCodeAt(++i) & 1023));
    } else text += str[i];
  }
  return encodeURIComponent(text);
}
const primitive = value => {
  if (typeof value === 'string' || typeof value === 'boolean' || typeof value === 'bigint') return '' + value;
  if (typeof value === 'number' && Number.isFinite(value)) return '' + value;
  return '';
};
function stringify(obj, sep, eq, options) {
  sep = sep || '&'; eq = eq || '=';
  let encode = api.escape;
  if (options && typeof options.encodeURIComponent === 'function') encode = options.encodeURIComponent;
  if (obj === null || typeof obj !== 'object') return '';
  const keys = Object.keys(obj);
  let output = '', emitted = false;
  for (const key of keys) {
    const value = obj[key];
    const prefix = concatenated(encode(key)) + concatenated(eq);
    const values = Array.isArray(value) ? value : [value];
    for (const item of values) {
      if (emitted) output += concatenated(sep);
      output += prefix + concatenated(encode(primitive(item)));
      emitted = true;
      bounded(output.length);
    }
  }
  return output;
}
function parse(qs, sep, eq, options) {
  const output = Object.create(null);
  if (typeof qs !== 'string' || qs.length === 0) return output;
  bounded(qs.length);
  sep = sep ? String(sep) : '&'; eq = eq ? String(eq) : '=';
  let remaining = 1000;
  if (options && typeof options.maxKeys === 'number') remaining = options.maxKeys > 0 ? options.maxKeys : -1;
  let decode = api.unescape;
  if (options && typeof options.decodeURIComponent === 'function') decode = options.decodeURIComponent;
  const custom = decode !== qsUnescape;
  const decoded = text => {
    if (text.length === 0) return text;
    text = text.replace(/\+/g, custom ? '%20' : ' ');
    if (custom || /%[0-9a-fA-F]{2}/.test(text)) {
      try { return decode(text); }
      catch { return qsUnescape(text, true); }
    }
    return text;
  };
  // Delimiter prefixes consume their matching positions. A mismatch resets
  // progress without rescanning that character, matching the observed legacy
  // overlap behavior; separator prefixes take priority over equals prefixes.
  let start = 0, separatorProgress = 0, equalsProgress = 0, equalsAt = -1;
  const emit = end => {
    if (end > start) {
      let key = equalsAt < 0 ? qs.slice(start,end) : qs.slice(start,equalsAt);
      let value = equalsAt < 0 || sep.length === 0 ? '' : qs.slice(equalsAt + eq.length,end);
      key = decoded(key); value = decoded(value);
      key = propertyKey(key);
      const previous = output[key];
      if (previous === undefined) output[key] = value;
      else if (previous === null) failure('TypeError', "Cannot read properties of null (reading 'pop')");
      else if (previous.pop) previous[previous.length] = value;
      else output[key] = [previous,value];
    }
    return --remaining === 0;
  };
  for (let index = 0; index < qs.length; index++) {
    const character = qs[index];
    if (character === sep[separatorProgress]) {
      separatorProgress++;
      if (separatorProgress === sep.length) {
        if (emit(index + 1 - sep.length)) return output;
        start = index + 1; separatorProgress = equalsProgress = 0; equalsAt = -1;
      }
      continue;
    }
    separatorProgress = 0;
    if (equalsAt < 0) {
      if (character === eq[equalsProgress]) {
        if (++equalsProgress === eq.length) equalsAt = index + 1 - eq.length;
      } else equalsProgress = 0;
    }
  }
  if (start < qs.length) emit(qs.length);
  return output;
}
const api = {unescapeBuffer,unescape:qsUnescape,escape:qsEscape,stringify,encode:stringify,parse,decode:parse};
export {unescapeBuffer,qsUnescape as unescape,qsEscape as escape,stringify,stringify as encode,parse,parse as decode};
export default api;
