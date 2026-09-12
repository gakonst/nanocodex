// Independently authored EventEmitter subset and one owned async once helper.
// Listener state belongs to this JS realm; callbacks use the active execution.
// No native provider handle, task authority or Node process is captured here.
// The remaining async helpers and full Node namespace are not advertised.
const once = globalThis.__skyreTimers.once;
const defaultMaxListeners = 10;
const errorMonitor = Symbol('events.errorMonitor');
let fallbackMaximum = defaultMaxListeners;

function inspected(value) {
  if (typeof value === 'string') return "'" + value.replace(/\\/g, '\\\\').replace(/'/g, "\\'") + "'";
  return String(value);
}
function received(value) {
  if (value === null || value === undefined) return String(value);
  if (typeof value === 'object') return 'an instance of Object';
  return 'type ' + typeof value + ' (' + inspected(value) + ')';
}
function invalidType(name, expected, value) {
  const error = new TypeError('The "' + name + '" argument must be of type ' + expected + '. Received ' + received(value));
  error.code = 'ERR_INVALID_ARG_TYPE';
  throw error;
}
function validListener(listener) {
  if (typeof listener !== 'function') invalidType('listener', 'function', listener);
}
function validMaximum(value, name) {
  if (typeof value !== 'number') invalidType(name, 'number', value);
  if (Number.isNaN(value) || value < 0) {
    const error = new RangeError('The value of "' + name + '" is out of range. It must be >= 0. Received ' + String(value));
    error.code = 'ERR_OUT_OF_RANGE';
    throw error;
  }
}


// The immutable timer owner supplies genuine target observations and the common
// default. Abort refinement, subscriptions and native handles stay private.
const signalObservers=globalThis.__skyreTimers?.eventTargetObservers;
const sharedMaximum=()=>signalObservers?.defaultMaximum?.()??fallbackMaximum;
function setSharedMaximum(value){if(signalObservers?.setDefaultMaximum)signalObservers.setDefaultMaximum(value);else fallbackMaximum=value;}
function emitterArgument(name, value) {
  let description;
  if (value !== null && typeof value === 'object') {
    const type = Array.isArray(value) ? 'Array' : value.constructor?.name;
    description = 'an instance of ' + (typeof type === 'string' && type !== '' ? type : 'Object');
  } else description = received(value);
  const error = new TypeError('The "' + name + '" argument must be an instance of EventEmitter or EventTarget. Received ' + description);
  error.code = 'ERR_INVALID_ARG_TYPE';
  throw error;
}
function readableMethod(emitter, key) {
  if (emitter === null || emitter === undefined) {
    throw new TypeError("Cannot read properties of " + emitter + " (reading '" + key + "')");
  }
  return emitter[key];
}
function listenerCount(emitter, type) {
  // Check and invocation intentionally make two observable method reads.
  if (typeof readableMethod(emitter, 'listenerCount') === 'function') return emitter.listenerCount(type);
  if(signalObservers?.has(emitter))return signalObservers.count(emitter,type);
  return emitterArgument('emitter', emitter);
}
function getEventListeners(emitter, type) {
  if (typeof readableMethod(emitter, 'listeners') === 'function') return emitter.listeners(type);
  if(signalObservers?.has(emitter))return signalObservers.listeners(emitter,type);
  return emitterArgument('emitter', emitter);
}
function getMaxListeners(emitter) {
  // The callable is an admission shape, not a callback to invoke. Native maximum
  // accounting remains in the same realm owner used by the instance methods.
  if (emitter !== null && emitter !== undefined && typeof emitter.getMaxListeners === 'function') {
    return emitter._maxListeners === undefined ? sharedMaximum() : emitter._maxListeners;
  }
  if(signalObservers?.has(emitter))return signalObservers.maximum(emitter);
  return emitterArgument('emitter', emitter);
}
const setMaxListeners = function(value = sharedMaximum(), ...emitters) {
  validMaximum(value, 'setMaxListeners');
  if (emitters.length === 0) setSharedMaximum(value);
  else for (const emitter of emitters) {
    if(signalObservers?.has(emitter)){signalObservers.setMaximum(emitter,value);continue;}
    if (typeof readableMethod(emitter, 'setMaxListeners') !== 'function') emitterArgument('eventTargets', emitter);
    // Earlier mutations remain if a later target rejects; callback results do
    // not become this configuration operation's return value.
    emitter.setMaxListeners(value);
  }
};
Object.defineProperty(setMaxListeners, 'name', {value:''});

function EventEmitter(options) {
  if (this === undefined || this === null) {
    throw new TypeError("Cannot read properties of " + this + " (reading '_events')");
  }
  // Promise rejection scheduling is outside this synchronous implementation.
  // Do not silently accept a request for that unavailable behavior.
  if (options != null && options.captureRejections !== undefined && options.captureRejections !== false) {
    throw new TypeError('EventEmitter captureRejections is not supported');
  }
  const inherited = Object.getPrototypeOf(this);
  if (this._events === undefined || (inherited !== null && this._events === inherited._events)) {
    this._events = Object.create(null);
    this._eventsCount = 0;
  }
  this._maxListeners = this._maxListeners;
}
EventEmitter.once = once;
EventEmitter.getEventListeners = getEventListeners;
EventEmitter.getMaxListeners = getMaxListeners;
EventEmitter.listenerCount = listenerCount;
EventEmitter.EventEmitter = EventEmitter;
EventEmitter.errorMonitor = errorMonitor;
Object.defineProperty(EventEmitter, 'defaultMaxListeners', {
  enumerable: true,
  get() { return sharedMaximum(); },
  set(value) { validMaximum(value, 'defaultMaxListeners'); setSharedMaximum(value); },
});
EventEmitter.setMaxListeners = setMaxListeners;
const prototype = EventEmitter.prototype;
prototype._events = undefined;
prototype._eventsCount = 0;
prototype._maxListeners = undefined;
prototype.setMaxListeners = function setMaxListeners(value) {
  validMaximum(value, 'setMaxListeners');
  this._maxListeners = value;
  return this;
};
prototype.getMaxListeners = function getMaxListeners() {
  return this._maxListeners === undefined ? sharedMaximum() : this._maxListeners;
};
prototype.emit = function emit(type, ...args) {
  const events = this._events;
  // Monitors observe the same synchronous emission before error handling. The
  // table belongs to this emission even if a monitor replaces this._events.
  // Dispatch through the instance method so ordinary overrides remain visible.
  if (type === 'error' && events !== undefined && events[errorMonitor] !== undefined) {
    this.emit(errorMonitor, ...args);
  }
  const stored = events === undefined ? undefined : events[type];
  if (stored === undefined) {
    if (type === 'error') {
      const value = args[0];
      if (value instanceof Error) throw value;
      const error = new Error('Unhandled error. (' + inspected(value) + ')');
      error.code = 'ERR_UNHANDLED_ERROR';
      error.context = value;
      throw error;
    }
    return false;
  }
  // A copy fixes this emission's order while nested emissions read the current
  // registry. Exceptions propagate synchronously and stop this dispatch.
  const listeners = typeof stored === 'function' ? [stored] : stored.slice();
  for (const listener of listeners) Reflect.apply(listener, this, args);
  return true;
};
function add(emitter, type, listener, prepend) {
  validListener(listener);
  if (emitter._events === undefined) {
    emitter._events = Object.create(null);
    emitter._eventsCount = 0;
  }
  if (emitter._events.newListener !== undefined) {
    emitter.emit('newListener', type, listener.listener ?? listener);
  }
  // A newListener callback may replace the table or insert another listener.
  const events = emitter._events;
  const previous = events[type];
  if (previous === undefined) {
    events[type] = listener;
    emitter._eventsCount++;
  } else if (typeof previous === 'function') {
    events[type] = prepend ? [listener, previous] : [previous, listener];
  } else if (prepend) previous.unshift(listener);
  else previous.push(listener);
  return emitter;
}
prototype.addListener = function addListener(type, listener) {
  return add(this, type, listener, false);
};
prototype.on = prototype.addListener;
prototype.prependListener = function prependListener(type, listener) {
  return add(this, type, listener, true);
};
function onceListener(emitter, type, listener) {
  let fired = false;
  function onceWrapper(...args) {
    if (fired) return undefined;
    fired = true;
    emitter.removeListener(type, onceWrapper);
    return Reflect.apply(listener, emitter, args);
  }
  onceWrapper.listener = listener;
  return onceWrapper;
}
prototype.once = function once(type, listener) {
  validListener(listener);
  return this.on(type, onceListener(this, type, listener));
};
prototype.prependOnceListener = function prependOnceListener(type, listener) {
  validListener(listener);
  return this.prependListener(type, onceListener(this, type, listener));
};
prototype.removeListener = function removeListener(type, listener) {
  validListener(listener);
  const events = this._events;
  if (events === undefined) return this;
  const previous = events[type];
  if (previous === undefined) return this;
  let removed;
  if (typeof previous === 'function') {
    if (previous !== listener && previous.listener !== listener) return this;
    removed = previous;
    if (--this._eventsCount === 0) this._events = Object.create(null);
    else delete events[type];
  } else {
    for (let index = previous.length - 1; index >= 0; index--) {
      if (previous[index] === listener || previous[index].listener === listener) {
        removed = previous[index];
        previous.splice(index, 1);
        if (previous.length === 1) events[type] = previous[0];
        break;
      }
    }
    if (removed === undefined) return this;
  }
  if (this._events.removeListener !== undefined) {
    this.emit('removeListener', type, removed.listener ?? removed);
  }
  return this;
};
prototype.off = prototype.removeListener;
prototype.removeAllListeners = function removeAllListeners(type) {
  const events = this._events;
  if (events === undefined) return this;
  if (arguments.length === 0) {
    if (events.removeListener !== undefined) {
      for (const key of Reflect.ownKeys(events)) {
        if (key !== 'removeListener') this.removeAllListeners(key);
      }
      this.removeAllListeners('removeListener');
    }
    this._events = Object.create(null);
    this._eventsCount = 0;
    return this;
  }
  const previous = events[type];
  if (previous === undefined) return this;
  if (events.removeListener !== undefined) {
    const listeners = typeof previous === 'function' ? [previous] : previous.slice();
    for (let index = listeners.length - 1; index >= 0; index--) {
      this.removeListener(type, listeners[index]);
    }
  } else if (--this._eventsCount === 0) this._events = Object.create(null);
  else delete events[type];
  return this;
};
prototype.listeners = function listeners(type) {
  return this.rawListeners(type).map(listener => listener.listener ?? listener);
};
prototype.rawListeners = function rawListeners(type) {
  const stored = this._events === undefined ? undefined : this._events[type];
  return stored === undefined ? [] : typeof stored === 'function' ? [stored] : stored.slice();
};
prototype.listenerCount = function listenerCount(type, listener) {
  const stored = this._events === undefined ? undefined : this._events[type];
  if (stored === undefined) return 0;
  const listeners = typeof stored === 'function' ? [stored] : stored;
  if (typeof listener !== 'function') return listeners.length;
  let count = 0;
  for (const entry of listeners) if (entry === listener || entry.listener === listener) count++;
  return count;
};
prototype.eventNames = function eventNames() {
  return this._eventsCount > 0 ? Reflect.ownKeys(this._events) : [];
};

export {EventEmitter, defaultMaxListeners, errorMonitor, listenerCount, getEventListeners, getMaxListeners, once, setMaxListeners};
export default EventEmitter;
