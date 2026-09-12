// Independently implemented output and URL adapters for the installed CUA API.
(() => {
  // Host primitives are captured before any submitted code runs. Only the
  // trusted adapter initializer can consume the private bridge below.
  const rawHostRpc = __skyre_rpc,
    responseMeta = __skyre_response_meta, cellActive = __skyre_cell_active,
    cellId = __skyre_cell_id,
    registerOperation = __skyre_operation_register, finishOperation = __skyre_operation_finish,
    rawDerive = __skyre_operation_derive, rawTimerSchedule = __skyre_timer_schedule;
  const deriveOperation = (source,target) => { rawDerive(source,target); return target; };
  const ownedYield = () => new promiseConstructor(resolve => rawTimerSchedule(resolve,0));
  // Suspension is authority to exclude a trusted synchronous host operation,
  // never authority to run model-mutated JavaScript with its deadline disabled.
  let suspensionDepth = 0, suspensionCell;
  const hostRpc = (method, request) => {
    const cell = cellId();
    const suspend = cellActive() && suspensionDepth > 0 && suspensionCell === cell;
    // Queueing captures authority; only the Rust dispatcher suspends its actual
    // blocking provider call. No user JavaScript executes in a suspended phase.
    ensureOperationCapacity();
    const raw=rawHostRpc(method,request,suspend);
    return deriveOperation(raw,apply(promiseResolve,promiseConstructor,[trackOperation(raw)]));
  };
  const parseJson = JSON.parse, stringifyJson = JSON.stringify,
    descriptors = Object.getOwnPropertyDescriptors, createObject = Object.create,
    defineProperty = Object.defineProperty, freezeObject = Object.freeze,
    isArray = Array.isArray, ownKeys = Reflect.ownKeys, apply = Reflect.apply,
    setPrototypeOf = Object.setPrototypeOf;
  // Private records must not execute an inherited/user-installed toJSON hook.
  // Own accessors are rejected; no model callback receives the private bridge.
  const privateRecord = value => {
    const parents = createObject(null);
    let parentCount = 0;
    const copy = (value, depth) => {
      if (value === null || typeof value !== 'object') return value;
      if (depth > 128) throw new TypeError('Private host record is cyclic or exceeds the nesting limit');
      for (let i = 0; i < parentCount; i++) if (parents[i] === value) throw new TypeError('Private host record is cyclic or exceeds the nesting limit');
      parents[parentCount++] = value;
      const fields = descriptors(value), result = isArray(value) ? [] : createObject(null);
      if (isArray(value)) setPrototypeOf(result, null);
      const keys = ownKeys(fields);
      for (let i = 0; i < keys.length; i++) {
        const key = keys[i], field = fields[key];
        if (typeof key !== 'string' || !field.enumerable) continue;
        if (!('value' in field)) throw new TypeError('Private host record must contain data properties');
        defineProperty(result, key, {value:copy(field.value, depth + 1),enumerable:true,writable:true,configurable:true});
      }
      delete parents[--parentCount];
      return result;
    };
    return copy(value, 0);
  };
  const privateJson = value => stringifyJson(privateRecord(value));
  const parsePrivate = text => privateRecord(parseJson(text));
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  const encode = bytes => {
    let text = '';
    for (let i = 0; i < bytes.length; i += 3) {
      const a = bytes[i], b = bytes[i + 1], c = bytes[i + 2];
      text += alphabet[a >> 2] + alphabet[((a & 3) << 4) | ((b ?? 0) >> 4)]
        + (b === undefined ? '=' : alphabet[((b & 15) << 2) | ((c ?? 0) >> 6)])
        + (c === undefined ? '=' : alphabet[c & 63]);
    }
    return text;
  };
  const asBytes = value => value instanceof Uint8Array ? value
    : value instanceof ArrayBuffer ? new Uint8Array(value)
    : ArrayBuffer.isView(value) ? new Uint8Array(value.buffer, value.byteOffset, value.byteLength) : null;
  function mime(bytes) {
    if ([137, 80, 78, 71, 13, 10, 26, 10].every((b, i) => bytes[i] === b)) return 'image/png';
    if (bytes[0] === 255 && bytes[1] === 216 && bytes[2] === 255) return 'image/jpeg';
    if (bytes.length >= 12 && bytes[0] === 82 && bytes[1] === 73 && bytes[2] === 70 && bytes[3] === 70
        && bytes[8] === 87 && bytes[9] === 69 && bytes[10] === 66 && bytes[11] === 80) return 'image/webp';
    throw new Error('nodeRepl.emitImage could not infer image MIME type from bytes; expected PNG, JPEG, or WebP data');
  }
  function packet(bytes, mimeType) {
    if (!bytes.byteLength) throw new Error('nodeRepl.emitImage expected non-empty bytes');
    if (typeof mimeType !== 'string' || !mimeType) throw new Error('nodeRepl.emitImage expected a non-empty mimeType');
    if (bytes.byteLength > 3 * 1024 * 1024) throw new Error('Image exceeds the bounded 3 MiB image budget');
    return {data: encode(bytes), mime_type: mimeType};
  }
  const unwrap = (value, parser = parseJson) => {
    const response = parser(value);
    if (response.error) { const error = new Error(response.error.message); error.code = response.error.code; throw error; }
    return response.result;
  };
  async function imagePacket(value) {
    value = await value;
    if (typeof value === 'string') {
      if (!value) throw new Error('nodeRepl.emitImage expected a non-empty image_url');
      if (/^file:/i.test(value)) return unwrap(__skyre_read_image_file(value));
      if (!/^data:/i.test(value)) throw new Error('nodeRepl.emitImage only accepts data or file URLs');
      return unwrap(__skyre_image_data_url(value));
    }
    const bytes = asBytes(value);
    if (bytes) return packet(bytes, mime(bytes));
    if (value && typeof value === 'object' && !Array.isArray(value) && 'bytes' in value) {
      if (Object.keys(value).some(key => key !== 'bytes' && key !== 'mimeType'))
        throw new Error('nodeRepl.emitImage received an unsupported value');
      const data = asBytes(value.bytes);
      if (!data) throw new Error('nodeRepl.emitImage expected bytes to be Buffer, Uint8Array, ArrayBuffer, or ArrayBufferView');
      return packet(data, value.mimeType);
    }
    throw new Error('nodeRepl.emitImage received an unsupported value');
  }
  const quote = text => {
    const delimiter = !text.includes("'") ? "'" : !text.includes('"') ? '"'
      : !text.includes('`') && !text.includes('${') ? '`' : "'";
    let out = delimiter;
    for (let i = 0; i < text.length; i++) {
      const c = text[i], n = text.charCodeAt(i);
      if (c === delimiter || c === '\\') out += '\\' + c;
      else if (c === '\n') out += '\\n';
      else if (c === '\r') out += '\\r';
      else if (c === '\t') out += '\\t';
      else if (c === '\b') out += '\\b';
      else if (c === '\f') out += '\\f';
      else if (c === '\v') out += '\\v';
      else if (n < 32 || n === 127) out += '\\x' + n.toString(16).padStart(2, '0');
      else if (n >= 0xd800 && n <= 0xdbff && i + 1 < text.length && text.charCodeAt(i + 1) >= 0xdc00 && text.charCodeAt(i + 1) <= 0xdfff) out += c + text[++i];
      else if (n >= 0xd800 && n <= 0xdfff) out += '\\u' + n.toString(16).padStart(4, '0');
      else out += c;
    }
    return out + delimiter;
  };
  function inspect(value) {
    const active = new Map(), circular = new Map(); let next = 1;
    const nesting = (v, remaining, seen = new Set()) => {
      if (!v || typeof v !== 'object' || remaining < 0 || seen.has(v)) return 0;
      seen.add(v);
      const children = Reflect.ownKeys(v).map(k => Object.getOwnPropertyDescriptor(v, k)).filter(d => d.enumerable && 'value' in d);
      const depth = 1 + Math.max(0, ...children.map(d => nesting(d.value, remaining - 1, seen)));
      seen.delete(v); return depth;
    };
    const render = (v, depth, indent) => {
      if (v === null) return 'null';
      switch (typeof v) {
        case 'undefined': return 'undefined';
        case 'string': return quote(v);
        case 'number': return Object.is(v, -0) ? '-0' : String(v);
        case 'bigint': return v.toString() + 'n';
        case 'boolean': case 'symbol': return String(v);
      }
      if (active.has(v)) {
        if (!circular.has(v)) circular.set(v, next++);
        return '[Circular *' + circular.get(v) + ']';
      }
      if (depth < 0) return Array.isArray(v) ? '[Array]' : '[' + (v.constructor?.name || 'Object') + ']';
      active.set(v, true);
      let result;
      const format = (prefix, open, close, parts) => {
        if (!parts.length) return prefix + open + close;
        const one = prefix + open + ' ' + parts.join(', ') + ' ' + close;
        if (one.length + indent <= 80 && !one.includes('\n') && nesting(v, depth) <= 3) return one;
        return prefix + open + '\n' + parts.map(s => ' '.repeat(indent + 2) + s).join(',\n') + '\n' + ' '.repeat(indent) + close;
      };
      const field = key => {
        const descriptor = Object.getOwnPropertyDescriptor(v, key);
        const name = typeof key === 'symbol' ? '[' + String(key) + ']' : /^[A-Za-z_$][\w$]*$/.test(key) ? key : quote(key);
        const content = descriptor.get || descriptor.set ? descriptor.get && descriptor.set ? '[Getter/Setter]' : descriptor.get ? '[Getter]' : '[Setter]' : render(descriptor.value, depth - 1, indent + 2);
        return name + ': ' + content;
      };
      const properties = () => Reflect.ownKeys(v).filter(k => Object.getOwnPropertyDescriptor(v, k)?.enumerable).map(field);
      if (Array.isArray(v)) {
        const parts = [];
        for (let i = 0; i < Math.min(v.length, 100); i++) {
          if (!(i in v)) { let end = i + 1; while (end < v.length && !(end in v)) end++; const count = end - i; parts.push('<' + count + ' empty item' + (count === 1 ? '' : 's') + '>'); i = end - 1; }
          else parts.push(render(v[i], depth - 1, indent + 2));
        }
        if (v.length > 100) parts.push('... ' + (v.length - 100) + ' more items');
        for (const key of Reflect.ownKeys(v)) if ((typeof key === 'symbol' || !/^(0|[1-9][0-9]*)$/.test(key)) && key !== 'length' && Object.getOwnPropertyDescriptor(v, key)?.enumerable) parts.push(field(key));
        result = format('', '[', ']', parts);
      } else if (globalThis.Buffer?.isBuffer(v)) {
        const hex=Array.from(v.subarray(0,50),byte=>byte.toString(16).padStart(2,'0')).join(' ');
        result='<Buffer '+hex+(v.length>50?' ... '+(v.length-50)+' more byte'+(v.length===51?'':'s'):'')+'>';
      } else if (ArrayBuffer.isView(v) && !(v instanceof DataView)) {
        const parts = Array.from(v.slice(0, 100), n => render(n, depth - 1, indent + 2));
        if (v.length > 100) parts.push('... ' + (v.length - 100) + ' more items');
        result = format(v.constructor.name + '(' + v.length + ') ', '[', ']', parts);
      } else if (v instanceof Map) result = format('Map(' + v.size + ') ', '{', '}', Array.from(v, ([k, item]) => render(k, depth - 1, indent + 2) + ' => ' + render(item, depth - 1, indent + 2)));
      else if (v instanceof Set) result = format('Set(' + v.size + ') ', '{', '}', Array.from(v, item => render(item, depth - 1, indent + 2)));
      else if (v instanceof Date) result = Number.isNaN(v.getTime()) ? 'Invalid Date' : v.toISOString();
      else if (v instanceof RegExp) result = String(v);
      else if (v instanceof WeakMap || v instanceof WeakSet) result = v.constructor.name + ' { <items unknown> }';
      else if (v instanceof Promise) result = 'Promise { <pending> }';
      else if (v instanceof Error) result = v.stack?.includes(v.message) ? v.stack : v.name + ': ' + v.message;
      else if (typeof v === 'function') {
        const kind = Object.getPrototypeOf(v)?.constructor?.name || 'Function';
        result = '[' + kind + (v.name ? ': ' + v.name : ' (anonymous)') + ']';
        const parts = properties(); if (parts.length) result += ' ' + format('', '{', '}', parts);
      } else result = format(Object.getPrototypeOf(v) === null ? '[Object: null prototype] ' : v.constructor?.name && v.constructor.name !== 'Object' ? v.constructor.name + ' ' : '', '{', '}', properties());
      active.delete(v);
      return (circular.has(v) ? '<ref *' + circular.get(v) + '> ' : '') + result;
    };
    return typeof value === 'string' ? value : render(value, 4, 0);
  }
  const promiseConstructor = Promise, promiseResolve = Promise.resolve, promiseReject = Promise.reject,
    promiseThen = Promise.prototype.then, promiseFinally = Promise.prototype.finally;
  const rejectedThenable = error => ({
    then(ok,fail){return apply(promiseThen,apply(promiseReject,promiseConstructor,[error]),[ok,fail]);},
    catch(fail){return apply(promiseThen,apply(promiseReject,promiseConstructor,[error]),[undefined,fail]);},
    finally(finish){return apply(promiseFinally,apply(promiseReject,promiseConstructor,[error]),[finish]);},
  });
  const pending = createObject(null);
  let nextOperation = 0;
  const ensureOperationCapacity=()=>{if(ownKeys(pending).length>=1024)throw new Error('Too many pending runtime operations');};
  const trackOperation = value => {
    const operation = apply(promiseResolve, promiseConstructor, [value]);
    const observation = {observed:false};
    const id = ++nextOperation;
    ensureOperationCapacity();
    const nativeId = registerOperation(operation);
    pending[id] = apply(promiseThen, operation, [()=>{finishOperation(nativeId);return {ok:true,observation};},error=>{finishOperation(nativeId);return {ok:false,error,observation};}]);
    return {
      then(ok,fail){observation.observed=true;return apply(promiseThen,operation,[ok,fail]);},
      catch(fail){observation.observed=true;return apply(promiseThen,operation,[undefined,fail]);},
      finally(finish){observation.observed=true;return apply(promiseFinally,operation,[finish]);},
    };
  };
  const frozen = value => { if (value && typeof value === 'object' && !Object.isFrozen(value)) { Object.freeze(value); for (const item of Object.values(value)) frozen(item); } return value; };
  const config = frozen(parseJson(__skyre_host_options));
  // Parse once, before submitted code can replace any intrinsic. The original
  // launcher uses ECMAScript trim, insertion-order deduplication and exact case.
  const surfaceNames = new Set((config.cuaEnabledSurfaces ?? 'browser,computer')
    .split(',').map(surface=>surface.trim()).filter(Boolean));
  let surfaceError = null;
  for (const surface of surfaceNames) {
    if (surface !== 'browser' && surface !== 'computer') {
      surfaceError = `Unknown CUA_REPL_ENABLED_SURFACES value: ${surface}`;
      break;
    }
  }
  if (surfaceNames.size === 0) surfaceError = 'CUA_REPL_ENABLED_SURFACES must enable browser or computer';
  const setupSurfaces = freezeObject({browser:surfaceNames.has('browser'),computer:surfaceNames.has('computer'),error:surfaceError});
  const readHostMetadata=typeof __skyre_request_meta==='function'?__skyre_request_meta:()=>JSON.stringify(config.requestMeta);
  let metadataSource,metadataValue;
  const requestMetadata=()=>{const source=readHostMetadata();if(source!==metadataSource){metadataValue=frozen(parseJson(source));metadataSource=source;}return metadataValue;};
  const suspended = async operation => {
    if (typeof operation !== 'function') throw new TypeError('nodeRepl.withSuspendedTimeout expected a function');
    const cell = cellId();
    if (suspensionCell !== cell) { suspensionCell = cell; suspensionDepth = 0; }
    suspensionDepth++;
    try { return await operation(); } finally { if (suspensionCell === cell) suspensionDepth--; }
  };
  const bridge = {
    cwd: config.cwd, env: freezeObject({}), homeDir: config.homeDir, tmpDir: config.tmpDir,
    get requestMeta() { return requestMetadata(); },
    write(value, itemId) {
      if (itemId !== undefined && (typeof itemId !== 'string' || !itemId.length)) throw new TypeError('nodeRepl.write expected a nonempty string content item ID');
      __skyre_write(JSON.stringify(inspect(value)), itemId === undefined ? 'output' : itemId, itemId === undefined ? 'write' : 'named');
    },
    emitImage(value) {
      if (!cellActive()) return rejectedThenable(new Error('node_repl exec context not found'));
      const cell = __skyre_cell_id();
      const operation = imagePacket(value).then(image => {
        if (!__skyre_cell_active() || __skyre_cell_id() !== cell) throw new Error('node_repl exec context not found');
        __skyre_write(JSON.stringify(image), 'image', 'image');
      });
      return trackOperation(operation);
    },
    rpc(service, request) {
      if (!cellActive()) return rejectedThenable(new Error('node_repl exec context not found'));
      if (typeof service !== 'string' || !service.trim()) return rejectedThenable(new Error('nodeRepl.rpc expected a nonempty service identifier'));
      let serialized;
      try { serialized = JSON.stringify(request); } catch { return rejectedThenable(new Error('nodeRepl.rpc expected a JSON-serializable request')); }
      if (serialized === undefined) return rejectedThenable(new Error('nodeRepl.rpc expected a JSON-serializable request'));
      try {
        ensureOperationCapacity();
        return trackOperation(apply(promiseThen,apply(promiseResolve,promiseConstructor,[rawHostRpc(service+'.rpc',serialized,false)]),[unwrap]));
      } catch(error) { return rejectedThenable(error); }
    }
  };
  freezeObject(bridge);
  const privateBridge = freezeObject(createObject(bridge, {
    env: {value:config.env, enumerable:true},
    withSuspendedTimeout: {value:suspended, enumerable:true},
    createElicitation: {enumerable:true, value(request) {
      if (!request || typeof request !== 'object' || isArray(request)) throw new TypeError('nodeRepl.createElicitation expected an object');
      const serialized = privateJson(request);
      return suspended(async () => unwrap(await hostRpc('host.elicitation', serialized), parsePrivate));
    }},
    setResponseMeta: {enumerable:true, value(value) {
      if (!value || typeof value !== 'object' || isArray(value)) throw new TypeError('Response metadata must be an object');
      responseMeta(privateJson(value));
    }},
  }));
  defineProperty(globalThis, 'nodeRepl', {value:bridge, writable:false, configurable:false, enumerable:false});
  let privateAvailable = true;
  defineProperty(globalThis, '__skyreTakePrivateBridge', {configurable:true, value() {
    if (!privateAvailable) throw new Error('Private runtime bridge was already consumed');
    privateAvailable = false;
    return freezeObject({nodeRepl:privateBridge,rpc:hostRpc,stringify:privateJson,parse:parseJson,parsePrivate,apply,trackOperation,deriveOperation,ownedYield,setupSurfaces});
  }});
  for (const name of ['__skyre_rpc','__skyre_suspend_timeout','__skyre_response_meta','__skyre_operation_register','__skyre_operation_finish','__skyre_operation_derive']) {
    if (!Reflect.deleteProperty(globalThis, name)) throw new Error('Cannot isolate private host primitive ' + name);
  }
  Object.defineProperty(globalThis, '__skyreDrainOutput', {value: async () => {
    for (;;) {
      const keys=ownKeys(pending);
      if (!keys.length) return;
      let failure;
      for(let i=0;i<keys.length;i++){
        const operation=pending[keys[i]];delete pending[keys[i]];
        const result=await operation;
        if(!result.ok&&!result.observation.observed&&failure===undefined)failure=result;
      }
      if(failure!==undefined)throw failure.error;
    }
  }});
  Object.defineProperty(globalThis, '__skyreResetOutput', {value: () => {const keys=ownKeys(pending);for(let i=0;i<keys.length;i++)delete pending[keys[i]];}});
  const urlData = new WeakMap();
  class URL {
    constructor(input, base) {
      if (!arguments.length) throw new TypeError('The URL constructor requires an input');
      urlData.set(this, unwrap(__skyre_url_parse(JSON.stringify({input: String(input), ...(base === undefined ? {} : {base: String(base)})}))));
    }
    static canParse(input, base) { if (!arguments.length) return false; try { new URL(input, base); return true; } catch { return false; } }
    static parse(input, base) { try { return new URL(input, base); } catch { return null; } }
    toString() { return this.href; }
    toJSON() { return this.href; }
  }
  for (const property of ['href','origin','protocol','username','password','host','hostname','port','pathname','search','hash']) {
    const descriptor = {enumerable: true, configurable: true, get() { const data = urlData.get(this); if (!data) throw new TypeError('Invalid URL receiver'); return data[property]; }};
    if (property !== 'origin') descriptor.set = function(value) {
      const data = urlData.get(this); if (!data) throw new TypeError('Invalid URL receiver');
      urlData.set(this, unwrap(__skyre_url_parse(JSON.stringify({input: data.href, set: {property, value: String(value)}}))));
    };
    Object.defineProperty(URL.prototype, property, descriptor);
  }
  globalThis.URL = URL;
  globalThis.TextEncoder = class TextEncoder {
    get encoding() { return 'utf-8'; }
    encode(input = '') {
      const bytes = [];
      for (const character of String(input)) {
        let point = character.codePointAt(0);
        if (point >= 0xd800 && point <= 0xdfff) point = 0xfffd;
        if (point < 0x80) bytes.push(point);
        else if (point < 0x800) bytes.push(0xc0 | point >> 6, 0x80 | point & 63);
        else if (point < 0x10000) bytes.push(0xe0 | point >> 12, 0x80 | point >> 6 & 63, 0x80 | point & 63);
        else bytes.push(0xf0 | point >> 18, 0x80 | point >> 12 & 63, 0x80 | point >> 6 & 63, 0x80 | point & 63);
      }
      return new Uint8Array(bytes);
    }
    encodeInto(input, destination) {
      if (!(destination instanceof Uint8Array)) throw new TypeError('The destination must be a Uint8Array');
      let read = 0, written = 0;
      for (const character of String(input)) {
        const bytes = this.encode(character);
        if (written + bytes.length > destination.length) break;
        destination.set(bytes, written); written += bytes.length; read += character.length;
      }
      return {read, written};
    }
  };
  globalThis.console = Object.freeze(Object.fromEntries(['log','info','warn','error','debug'].map(name => [name, (...values) => __skyre_write(JSON.stringify(values.map(inspect).join(' ') + '\n'), 'output', 'line')])));
  const bindings = new Map(), probes = new Map();
  let bindingCell = 0;
  const checkBindingCell = cell => {if (cell !== bindingCell || cell !== __skyre_cell_id() || !__skyre_cell_active()) throw new Error('Evaluation context expired');};
  Object.defineProperty(globalThis,'__skyreKernelInternals',{value:Object.freeze({
    metadata:()=>[...bindings].map(([name,binding])=>[name,binding.kind]),
    read:name=>bindings.get(name).read(),
    start(cell){checkBindingCellStart(cell);bindingCell=cell;probes.clear();},
    probe(name,kind,read,cell){checkBindingCell(cell);probes.set(name,{kind,read});},
    commit(name,kind,read,cell){checkBindingCell(cell);bindings.set(name,{kind,read});},
    finish(success,cell){
      checkBindingCell(cell);
      for(const [name,binding] of probes){
        if(success){bindings.set(name,binding);continue;}
        if(binding.kind==='var'||binding.kind==='function')continue;
        try{binding.read();bindings.set(name,binding);}catch{}
      }
      probes.clear();
    },
    initialize:()=>globalThis.__skyreInitialize(setupSurfaces),
    resetOutput:()=>globalThis.__skyreResetOutput(),
    drainOutput:()=>globalThis.__skyreDrainOutput(),
    warn:globalThis.console.warn,
  }),configurable:false,writable:false});
  function checkBindingCellStart(cell){if(cell!==__skyre_cell_id()||!__skyre_cell_active())throw new Error('Evaluation context expired');}
})();
