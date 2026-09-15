// Independent URLSearchParams implementation; pair storage is private and live
// URL views always read the current URL before an operation or iterator step.
(() => {
  const data = new WeakMap(), views = new WeakMap();
  const encoder = new TextEncoder();
  const urlSearch = Object.getOwnPropertyDescriptor(URL.prototype,'search');
  function error(message, code) {
    const result = new TypeError(message);
    if (code !== undefined) result.code = code;
    return result;
  }
  function string(value) {
    if (typeof value === 'symbol') throw error('Cannot convert a Symbol value to a string');
    let result = '';
    for (const character of String(value)) {
      const point = character.codePointAt(0);
      result += point >= 0xd800 && point <= 0xdfff ? '\ufffd' : character;
    }
    return result;
  }
  function required(count, names) {
    if (count < names.length) throw error('The ' + names.map(name => '"' + name + '"').join(' and ') + (names.length === 1 ? ' argument' : ' arguments') + ' must be specified', 'ERR_MISSING_ARGS');
  }
  function state(receiver) {
    const record = data.get(receiver);
    if (!record) throw error('Value of "this" must be of type URLSearchParams', 'ERR_INVALID_THIS');
    const source = record.url ? urlSearch.get.call(record.url) : undefined;
    if (record.url && record.source !== source) {
      record.source = source;
      record.pairs = JSON.parse(__skyre_form_decode(record.source.slice(1)));
    }
    return record;
  }
  function encode(value) {
    let result = '';
    for (const byte of encoder.encode(value)) {
      if (byte === 32) result += '+';
      else if (byte >= 65 && byte <= 90 || byte >= 97 && byte <= 122 || byte >= 48 && byte <= 57 || [42,45,46,95].includes(byte)) result += String.fromCharCode(byte);
      else result += '%' + byte.toString(16).toUpperCase().padStart(2,'0');
    }
    return result;
  }
  function serialize(pairs) { return pairs.map(([key,value]) => encode(key) + '=' + encode(value)).join('&'); }
  function update(record) {
    if (record.url) {
      urlSearch.set.call(record.url,serialize(record.pairs));
      record.source = urlSearch.get.call(record.url);
    }
  }
  function iterator(receiver, kind) {
    state(receiver);
    let index = 0;
    return {
      next() {
        const pair = state(receiver).pairs[index];
        if (!pair) return {value: undefined, done: true};
        index++;
        return {value: kind === 'entries' ? [...pair] : pair[kind === 'keys' ? 0 : 1], done: false};
      },
      [Symbol.iterator]() { return this; },
      [Symbol.toStringTag]: 'URLSearchParams Iterator',
    };
  }
  class URLSearchParams {
    constructor(init = undefined) {
      let pairs = [];
      if (init !== undefined) {
        if (init !== null && (typeof init === 'object' || typeof init === 'function')) {
          const iterable = init[Symbol.iterator];
          if (iterable !== undefined && iterable !== null) {
            if (typeof iterable !== 'function') throw error('Query pairs must be iterable', 'ERR_ARG_NOT_ITERABLE');
            for (const pair of init) {
              if (pair === null || typeof pair !== 'object' && typeof pair !== 'function' || typeof pair[Symbol.iterator] !== 'function') throw error('Each query pair must be an iterable [name, value] tuple', 'ERR_INVALID_TUPLE');
              const values = [...pair];
              if (values.length !== 2) throw error('Each query pair must be an iterable [name, value] tuple', 'ERR_INVALID_TUPLE');
              pairs.push(values.map(string));
            }
          } else {
            for (const key of Reflect.ownKeys(init)) {
              if (Object.getOwnPropertyDescriptor(init,key)?.enumerable) {
                const name = string(key), value = string(init[key]);
                const existing = pairs.findIndex(pair => pair[0] === name);
                if (existing >= 0) pairs[existing][1] = value;
                else pairs.push([name,value]);
              }
            }
          }
        } else {
          const source = string(init);
          pairs = JSON.parse(__skyre_form_decode(source.startsWith('?') ? source.slice(1) : source));
        }
      }
      data.set(this,{pairs});
    }
    get size() { return state(this).pairs.length; }
    append(name,value) {
      state(this); required(arguments.length,['name','value']);
      name = string(name); value = string(value);
      const record = state(this);
      record.pairs.push([name,value]); update(record);
    }
    delete(name,value = undefined) {
      const pairs = state(this).pairs; required(arguments.length,['name']);
      name = string(name); if (value !== undefined) value = string(value);
      for (let i = pairs.length - 1; i >= 0; i--) {
        if (pairs[i][0] === name && (value === undefined || pairs[i][1] === value)) pairs.splice(i,1);
      }
      update(state(this));
    }
    get(name) {
      const pairs = state(this).pairs; required(arguments.length,['name']); name = string(name);
      return pairs.find(pair => pair[0] === name)?.[1] ?? null;
    }
    getAll(name) {
      const pairs = state(this).pairs; required(arguments.length,['name']); name = string(name);
      return pairs.filter(pair => pair[0] === name).map(pair => pair[1]);
    }
    has(name,value = undefined) {
      const pairs = state(this).pairs; required(arguments.length,['name']); name = string(name);
      if (value !== undefined) value = string(value);
      return pairs.some(pair => pair[0] === name && (value === undefined || pair[1] === value));
    }
    set(name,value) {
      const pairs = state(this).pairs; required(arguments.length,['name','value']); name = string(name); value = string(value);
      const position = pairs.findIndex(pair => pair[0] === name);
      if (position < 0) pairs.push([name,value]);
      else {
        pairs[position][1] = value;
        for (let i = pairs.length - 1; i > position; i--) if (pairs[i][0] === name) pairs.splice(i,1);
      }
      update(state(this));
    }
    sort() {
      const record = state(this);
      record.pairs.sort((a,b) => a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0);
      update(record);
    }
    entries() { return iterator(this,'entries'); }
    keys() { return iterator(this,'keys'); }
    values() { return iterator(this,'values'); }
    forEach(callback,thisArg = undefined) {
      state(this);
      if (typeof callback !== 'function') {
        let received = String(callback);
        if (typeof callback === 'number' || typeof callback === 'boolean') received = 'type ' + typeof callback + ' (' + received + ')';
        throw error('The "callback" argument must be of type function. Received ' + received, 'ERR_INVALID_ARG_TYPE');
      }
      for (const [name,value] of iterator(this,'entries')) callback.call(thisArg,value,name,this);
    }
    toString() { return serialize(state(this).pairs); }
  }
  Object.defineProperty(URLSearchParams.prototype,Symbol.iterator,{value:URLSearchParams.prototype.entries,writable:true,configurable:true});
  Object.defineProperty(URLSearchParams.prototype,Symbol.toStringTag,{value:'URLSearchParams',configurable:true});
  // Node exposes the standard prototype operations as enumerable properties.
  for (const name of Object.getOwnPropertyNames(URLSearchParams.prototype)) {
    if (name !== 'constructor') Object.defineProperty(URLSearchParams.prototype,name,{enumerable:true});
  }
  Object.defineProperty(URL.prototype,'searchParams',{enumerable:true,configurable:true,get() {
    // The existing getter enforces the URL receiver brand.
    let source;
    try { source = urlSearch.get.call(this); }
    catch { throw error('Cannot read private member #searchParams from an object whose class did not declare it'); }
    let value = views.get(this);
    if (!value) {
      value = new URLSearchParams(source);
      data.get(value).url = this;
      data.get(value).source = source;
      views.set(this,value);
    }
    return value;
  }});
  globalThis.URLSearchParams = URLSearchParams;
})();
