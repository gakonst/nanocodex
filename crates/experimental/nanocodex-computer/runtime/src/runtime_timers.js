// Independent Node timer/abort surface over bounded Rust-owned task queues.
(() => {
  'use strict';
  const schedule = globalThis.__skyre_timer_schedule;
  const cancel = globalThis.__skyre_timer_clear;
  const refreshNative = globalThis.__skyre_timer_refresh;
  const enqueueMicrotask = globalThis.__skyre_microtask;
  const apply = Reflect.apply, bind = Function.prototype.bind, P = Promise, resolve = P.resolve.bind(P), finallyPromise = P.prototype.finally;
  const handles = new WeakMap(), numeric = new Map();
  const refed = Symbol('refed'), hasPrimitive = Symbol('kHasPrimitive');
  let nextResource = 0;
  const newResource = () => {if(nextResource>=Number.MAX_SAFE_INTEGER)throw new Error('Timer ID exhausted');return ++nextResource;};
  const dispose = Symbol.dispose || Symbol('Symbol.dispose');
  if (!Symbol.dispose) Object.defineProperty(Symbol, 'dispose', { value: dispose });
  const received = value => {
    if (value === null) return 'null';
    if (value === undefined) return 'undefined';
    if (typeof value === 'string') return "type string ('" + value + "')";
    if (typeof value === 'number' || typeof value === 'boolean') return 'type ' + typeof value + ' (' + value + ')';
    if (typeof value === 'bigint') return 'type bigint (' + value + 'n)';
    if (typeof value === 'symbol') return 'type symbol (' + String(value) + ')';
    if (typeof value === 'function') return 'function ' + value.name;
    if(Object.getPrototypeOf(value)===null)return '[Object: null prototype] {}';
    return 'an instance of ' + (value.constructor?.name || 'Object');
  };
  const invalid = (name, expected, value) => {
    const error = new TypeError('The "' + name + '" ' + (name.includes('.') ? 'property' : 'argument') + ' must be ' + expected + '. Received ' + received(value));
    error.code = 'ERR_INVALID_ARG_TYPE'; return error;
  };
  const callback = value => { if (typeof value !== 'function') throw invalid('callback', 'of type function', value); };
  const primitive = value => {
    const simple = value => value===null || typeof value!=='object' && typeof value!=='function';
    if(simple(value))return value;
    const exotic=value[Symbol.toPrimitive];
    if(exotic!=null){
      if(typeof exotic!=='function'){
        const kind=typeof exotic;
        const detail=kind==='string'?' '+JSON.stringify(exotic):kind==='number'||kind==='boolean'?' '+String(exotic):'';
        throw new TypeError(kind+detail+' is not a function');
      }
      const result=apply(exotic,value,['number']);if(simple(result))return result;
    } else {
      for(const name of ['valueOf','toString']){const method=value[name];if(typeof method==='function'){const result=apply(method,value,[]);if(simple(result))return result;}}
    }
    throw new TypeError('Cannot convert object to primitive value');
  };
  const delayValue = value => {
    const number=primitive(value);
    if(typeof number==='bigint')throw new TypeError('Cannot mix BigInt and other types, use explicit conversions');
    if(typeof number==='symbol')throw new TypeError('Cannot convert a Symbol value to a number');
    let delay=number===undefined?1:number*1;
    if(!(delay>=1&&delay<=2147483647))delay=1;return delay;
  };
  const unregister = timer => { const state=handles.get(timer); if(state){cancel(state.nativeId);numeric.delete(String(state.id));state.pending=false;} };
  function arm(timer, immediate = false) {
    const state = handles.get(timer);
    if(timer._destroyed)state.id=newResource();
    state.pending = true; timer._destroyed = false;
    state.nativeId = schedule(() => {
      state.pending = false;
      if (immediate) {
        timer._destroyed = true; timer[refed] = null;
        const fn=timer._onImmediate;timer._onImmediate=null;
        if(fn) apply(fn,timer,timer._argv || []);
        return;
      }
      const fn=timer._onTimeout;
      if(!fn){timer._destroyed=true;return;}
      if(timer._repeat) arm(timer);
      try { apply(fn,timer,timer._timerArgs || []); }
      finally { if(!state.pending){timer._destroyed=true;numeric.delete(String(state.id));} }
    }, immediate ? 0 : Math.trunc(timer._idleTimeout), immediate ? 1 : 0);
  }
  class Timeout {
    constructor(fn, delay, args, repeating, isRefed) {
      const after=delayValue(delay);
      this._idleTimeout=after;this._idlePrev=this;this._idleNext=this;this._idleStart=null;
      this._onTimeout=fn;this._timerArgs=args;this._repeat=repeating?after:null;this._destroyed=false;this[refed]=isRefed;this[hasPrimitive]=false;
      handles.set(this,{id:newResource(),nativeId:0,pending:false});arm(this);
    }
    [Symbol.for('nodejs.util.inspect.custom')](_depth,options) {
      const inspect=arguments[2];
      if(typeof inspect==='function')return inspect(this,{...options,depth:0,customInspect:false});
      return 'Timeout { _idleTimeout: '+this._idleTimeout+', _repeat: '+this._repeat+', _destroyed: '+this._destroyed+' }';
    }
    refresh() {
      const state=handles.get(this);
      if(state && this._idleTimeout>=0 && this._onTimeout){
        if(!state.pending || !refreshNative(state.nativeId,Math.trunc(this._idleTimeout)))arm(this);
      }
      return this;
    }
    unref(){this[refed]=false;return this;}
    ref(){this[refed]=true;return this;}
    hasRef(){return this[refed];}
  }
  Timeout.prototype.close=function(){clearTimeout(this);return this;};
  Timeout.prototype[dispose]=function(){clearTimeout(this);};
  Timeout.prototype[Symbol.toPrimitive]=function(){const state=handles.get(this);if(!state)return undefined;if(!this[hasPrimitive]){this[hasPrimitive]=true;numeric.set(String(state.id),this);}return state.id;};
  class Immediate {
    constructor(fn,args){this._idleNext=null;this._idlePrev=null;this._onImmediate=fn;this._argv=args;this._destroyed=false;this[refed]=true;handles.set(this,{id:newResource(),nativeId:0,pending:false});arm(this,true);}
    ref(){if(this[refed]===false)this[refed]=true;return this;}
    unref(){if(this[refed]===true)this[refed]=false;return this;}
    hasRef(){return !!this[refed];}
  }
  Immediate.prototype[dispose]=function(){clearImmediate(this);};
  function setTimeout(fn,after,...args){callback(fn);return new Timeout(fn,after,args.length?args:undefined,false,true);}
  function setInterval(fn,after,...args){callback(fn);return new Timeout(fn,after,args.length?args:undefined,true,true);}
  function setImmediate(fn,...args){callback(fn);return new Immediate(fn,args.length?args:undefined);}
  function clearTimeout(timer){
    if(typeof timer==='number'||typeof timer==='string')timer=numeric.get(String(timer));
    if(timer?._onTimeout){timer._onTimeout=null;unregister(timer);timer._destroyed=true;timer._idleTimeout=-1;}
  }
  function clearInterval(timer){clearTimeout(timer);}
  function clearImmediate(timer){if(timer?._onImmediate&&!timer._destroyed){unregister(timer);timer._onImmediate=null;timer._destroyed=true;timer[refed]=null;}}
  function queueMicrotask(fn){callback(fn);enqueueMicrotask(fn);}

  // Real targets and events own separate state. AbortSignal adds only its
  // private abort/refinement data; ordinary inherited construction stays real.
  const signals=new WeakMap(),controllers=new WeakMap(),targets=new WeakMap(),events=new WeakMap();
  const weakGet=WeakMap.prototype.get,weakHas=WeakMap.prototype.has,weakSet=WeakMap.prototype.set;
  const OwnerMap=Map,OwnerSet=Set,mapGet=Map.prototype.get,mapSet=Map.prototype.set,mapDelete=Map.prototype.delete;
  const getMap=(map,key)=>apply(mapGet,map,[key]),setMap=(map,key,value)=>apply(mapSet,map,[key,value]),deleteMap=(map,key)=>apply(mapDelete,map,[key]);
  const eventNow=globalThis.__skyre_event_now;
  let commonMaximum=10;
  const missing=(...names)=>{const error=new TypeError('The '+names.map(name=>'"'+name+'"').join(' and ')+' argument'+(names.length===1?'':'s')+' must be specified');error.code='ERR_MISSING_ARGS';return error;};
  const wrongThis=name=>{const error=new TypeError('Value of "this" must be of type '+name);error.code='ERR_INVALID_THIS';return error;};
  const textType=value=>{if(typeof value==='symbol')throw new TypeError('Cannot convert a Symbol value to a string');return String(value);};
  const targetType=value=>{if(typeof value==='symbol'){const error=new TypeError('Value is a Symbol and cannot be converted to a string.');error.code='ERR_INVALID_ARG_TYPE';throw error;}return String(value);};
  class DOMException extends Error {
    constructor(message='',name='Error'){super(String(message));Object.defineProperty(this,'name',{value:String(name),configurable:true});}
    get code(){return this.name==='AbortError'?20:this.name==='TimeoutError'?23:0;}
    get [Symbol.toStringTag](){return 'DOMException';}
  }
  const signalState=signal=>{const state=apply(weakGet,signals,[signal]);if(!state)throw wrongThis('AbortSignal');return state;};
  const targetState=target=>{const state=apply(weakGet,targets,[target]);if(!state)throw wrongThis('EventTarget');return state;};
  const eventState=event=>{const state=apply(weakGet,events,[event]);if(!state)throw wrongThis('Event');return state;};
  const newTargetState=maximum=>({types:new OwnerMap(),counts:new OwnerMap(),maximum,warned:false,allocated:0,dispatches:0,retired:[]});
  const validMaximum=value=>{if(typeof value!=='number'||Number.isNaN(value)||value<0)throw new TypeError('Invalid listener maximum');};
  class Event {
    constructor(type,options=undefined){
      const timeStamp=eventNow();
      if(arguments.length===0)throw missing('type');
      if(options!=null&&(typeof options!=='object'||Array.isArray(options)))throw invalid('options','of type object',options);
      const bubbles=!!options?.bubbles,cancelable=!!options?.cancelable,composed=!!options?.composed;
      apply(weakSet,events,[this,{type:textType(type),bubbles,cancelable,composed,prevented:false,stopped:false,immediate:false,target:null,visible:false,inFlight:false,passive:false,trusted:false,timeStamp}]);
    }
    get type(){return eventState(this).type;}
    get target(){return eventState(this).target;}
    get currentTarget(){const state=eventState(this);return state.visible?state.target:null;}
    get srcElement(){return eventState(this).target;}
    get eventPhase(){return eventState(this).visible?2:0;}
    get bubbles(){return eventState(this).bubbles;}
    get cancelable(){return eventState(this).cancelable;}
    get composed(){return eventState(this).composed;}
    get defaultPrevented(){const state=eventState(this);return state.cancelable&&state.prevented;}
    get timeStamp(){return eventState(this).timeStamp;}
    get isTrusted(){return eventState(this).trusted;}
    get returnValue(){const state=eventState(this);return !state.cancelable||!state.prevented;}
    set returnValue(value){const state=eventState(this);if(!value&&state.cancelable&&!state.passive)state.prevented=true;}
    get cancelBubble(){return eventState(this).stopped;}
    set cancelBubble(value){const state=eventState(this);if(value)state.stopped=true;}
    preventDefault(){const state=eventState(this);if(state.cancelable&&!state.passive)state.prevented=true;}
    stopPropagation(){eventState(this).stopped=true;}
    stopImmediatePropagation(){const state=eventState(this);state.stopped=true;state.immediate=true;}
    composedPath(){const state=eventState(this);return state.visible?[state.target]:[];}
    initEvent(type,bubbles=false,cancelable=false){
      if(arguments.length===0)throw missing('type');
      const state=eventState(this);if(state.inFlight)return;
      state.type=textType(type);state.bubbles=!!bubbles;state.cancelable=!!cancelable;
    }
  }
  for(const key of Object.getOwnPropertyNames(Event.prototype))if(key!=='constructor')Object.defineProperty(Event.prototype,key,{enumerable:true,configurable:key!=='isTrusted'});
  Object.defineProperty(Event.prototype,Symbol.toStringTag,{value:'Event',configurable:true});
  for(const [name,value]of [['NONE',0],['CAPTURING_PHASE',1],['AT_TARGET',2],['BUBBLING_PHASE',3]]){
    Object.defineProperty(Event,name,{value,enumerable:true});Object.defineProperty(Event.prototype,name,{value,enumerable:true});
  }
  function releaseRemoved(state){
    if(state.dispatches!==0)return;
    for(let i=0;i<state.retired.length;i++){const node=state.retired[i];node.previous=undefined;node.next=undefined;state.allocated--;}
    state.retired.length=0;
  }
  function unlink(state,node){
    if(node.removed)return;
    node.removed=true;
    if(node.previous)node.previous.next=node.next;else node.root.first=node.next;
    if(node.next)node.next.previous=node.previous;else node.root.last=node.previous;
    // Cached iterator successors remain valid through all nested dispatches.
    // Removed nodes remain charged to the same budget until finally release.
    state.retired[state.retired.length]=node;releaseRemoved(state);
  }
  function addTargetListener(state,type,listener,capture,once,passive=false,resist=false){
    let root=getMap(state.types,type);
    if(root)for(let node=root.first;node;node=node.next)if(!node.removed&&node.listener===listener&&node.capture===capture)return node;
    if(state.allocated>=1024)throw new Error('Abort listener budget exceeded');
    if(!root){root={first:undefined,last:undefined};setMap(state.types,type,root);}
    const node={root,type,listener,capture,once,passive,resist,removed:false,previous:root.last,next:undefined};
    if(root.last)root.last.next=node;else root.first=node;
    root.last=node;state.allocated++;
    setMap(state.counts,type,(getMap(state.counts,type)??0)+1);
    return node;
  }
  function removeNode(state,node){
    if(node.removed)return;
    unlink(state,node);
    const count=getMap(state.counts,node.type)-1;
    if(count===0){
      const root=getMap(state.types,node.type);deleteMap(state.types,node.type);deleteMap(state.counts,node.type);
      for(let next=root?.first;next;){const current=next;next=current.next;unlink(state,current);}
    }else setMap(state.counts,node.type,count);
  }
  function removeTargetListener(state,type,listener,capture){
    const root=getMap(state.types,type);
    for(let node=root?.first;node;node=node.next)if(!node.removed&&node.listener===listener&&node.capture===capture){removeNode(state,node);return;}
  }
  function validTargetListener(listener){
    if(listener==null)return false;
    if(typeof listener==='function'||typeof listener==='object')return true;
    throw invalid('listener','an instance of EventListener',listener);
  }
  function targetOptions(options){
    if(options==null)return {once:false,capture:false,passive:false,signal:undefined};
    if(typeof options==='boolean')return {once:false,capture:options,passive:false,signal:undefined};
    if(typeof options!=='object')throw invalid('options','of type object',options);
    const once=!!options.once,capture=!!options.capture,passive=!!options.passive,signal=options.signal;
    if(signal!==undefined&&!apply(weakHas,signals,[signal]))throw invalid('options.signal','an instance of AbortSignal',signal);
    return {once,capture,passive,signal};
  }
  class EventTarget {
    constructor(){apply(weakSet,targets,[this,newTargetState(commonMaximum)]);}
    addEventListener(type,listener,options=undefined){
      const state=targetState(this);if(arguments.length<2)throw missing('type','listener');
      const selected=targetOptions(options);let subscription;
      if(selected.signal){
        const signal=signalState(selected.signal);if(signal.aborted)return;
        const source=targetState(selected.signal);
        const cancel=()=>removeTargetListener(state,targetType(type),listener,selected.capture);
        const node=addTargetListener(source,'abort',cancel,false,true,false,true);
        subscription=()=>removeNode(source,node);
      }
      try{
        // Original signal-option effects precede listener/type/duplicate checks.
        // Warning emission and weak-GC semantics remain outside this owner.
        if(!validTargetListener(listener))return;
        type=targetType(type);
        addTargetListener(state,type,listener,selected.capture,selected.once,selected.passive);
      }catch(error){if(subscription)subscription();throw error;}
    }
    removeEventListener(type,listener,options=undefined){
      const state=targetState(this);if(arguments.length<2)throw missing('type','listener');
      if(!validTargetListener(listener))return;
      type=targetType(type);removeTargetListener(state,type,listener,options?.capture===true);
    }
    dispatchEvent(event){
      const state=targetState(this);if(arguments.length===0)throw missing('event');
      if(!apply(weakHas,events,[event]))throw invalid('event','an instance of Event',event);
      return dispatchTarget(this,state,event);
    }
  }
  for(const name of ['addEventListener','removeEventListener','dispatchEvent'])Object.defineProperty(EventTarget.prototype,name,{enumerable:true});
  Object.defineProperty(EventTarget.prototype,Symbol.toStringTag,{value:'EventTarget',configurable:true});
  function dispatchTarget(target,state,event){
    const data=eventState(event);
    if(data.inFlight){const error=new Error('The event "'+data.type+'" is already being dispatched');error.code='ERR_EVENT_RECURSION';throw error;}
    data.inFlight=true;data.visible=true;data.target=target;state.dispatches++;
    try{
      for(let node=getMap(state.types,data.type)?.first;node;){
        const current=node;node=current.next;
        if(current.removed||(data.immediate&&!current.resist))continue;
        if(current.once)removeNode(state,current);
        data.passive=current.passive;
        try{
          const listener=current.listener;
          if(typeof listener==='function')apply(listener,target,[event]);
          else if(typeof listener.handleEvent==='function')apply(listener.handleEvent,listener,[event]);
        }catch(error){enqueueMicrotask(()=>{throw error;});}
        finally{data.passive=false;data.visible=false;}
      }
      return !(data.cancelable&&data.prevented);
    }finally{data.inFlight=false;data.visible=false;state.dispatches--;releaseRemoved(state);}
  }
  function abort(signal,reason){
    const state=signalState(signal);if(state.aborted)return;state.aborted=true;state.reason=reason;
    const event=new Event('abort');eventState(event).trusted=true;
    dispatchTarget(signal,targetState(signal),event);
    for(const listener of [...state.internal])listener();
  }
  function newSignal(){
    const signal=Object.create(AbortSignal.prototype);apply(weakSet,targets,[signal,newTargetState(0)]);
    apply(weakSet,signals,[signal,{aborted:false,reason:undefined,internal:new OwnerSet(),onabortWrapper:undefined}]);return signal;
  }
  class AbortSignal extends EventTarget {
    constructor(){super();const e=new TypeError('Illegal constructor');e.code='ERR_ILLEGAL_CONSTRUCTOR';throw e;}
    get aborted(){return signalState(this).aborted;}
    get reason(){return signalState(this).reason;}
    get onabort(){return signalState(this).onabortWrapper?.handler??null;}
    set onabort(value){
      const state=signalState(this),owner=targetState(this),prior=state.onabortWrapper;
      if(prior){
        if(typeof prior.handler==='function'&&getMap(owner.counts,'abort')!==undefined)setMap(owner.counts,'abort',getMap(owner.counts,'abort')-1);
        prior.handler=value;
        if(typeof value==='function'){
          if(getMap(owner.counts,'abort')!==undefined)setMap(owner.counts,'abort',getMap(owner.counts,'abort')+1);
          else this.addEventListener('abort',prior);
        }
      }else{
        const wrapper=function(...args){
          const fn=wrapper.handler;
          if(typeof fn==='function')return apply(fn,this,args);
        };
        Object.defineProperty(wrapper,'name',{value:'eventHandler'});
        wrapper.handler=value;
        this.addEventListener('abort',wrapper);
        state.onabortWrapper=wrapper;
      }
    }
    throwIfAborted(){const state=signalState(this);if(state.aborted)throw state.reason;}
    static abort(reason=new DOMException('This operation was aborted','AbortError')){const signal=newSignal();abort(signal,reason);return signal;}
    static timeout(delay){
      if(typeof delay!=='number')throw invalid('delay','of type number',delay);
      if(!Number.isInteger(delay)||delay<0||delay>4294967295){const e=new RangeError('The value of "delay" is out of range. It must be '+(Number.isInteger(delay)?'>= 0 && <= 4294967295':'an integer')+'. Received '+delay);e.code='ERR_OUT_OF_RANGE';throw e;}
      const signal=newSignal();setTimeout(()=>abort(signal,new DOMException('The operation was aborted due to timeout','TimeoutError')),delay).unref();return signal;
    }
    static any(list){
      if(list==null||(typeof list!=='object'&&typeof list!=='function')||typeof list[Symbol.iterator]!=='function'){const e=new TypeError('signals cannot be converted to sequence.');e.code='ERR_INVALID_ARG_TYPE';throw e;}list=Array.from(list);
      const out=newSignal(),subscriptions=[];
      list.forEach((signal,index)=>{if(!signals.has(signal)){const e=new TypeError('signals['+index+'] is not of type AbortSignal.');e.code='ERR_INVALID_ARG_TYPE';throw e;}});
      const first=list.find(signal=>signal.aborted);if(first){abort(out,first.reason);return out;}
      for(const signal of list){const state=signalState(signal);const fn=()=>{abort(out,signal.reason);for(const [source,callback]of subscriptions)signalState(source).internal.delete(callback);};state.internal.add(fn);subscriptions.push([signal,fn]);}
      return out;
    }
    get [Symbol.toStringTag](){return 'AbortSignal';}
  }
  class AbortController {
    constructor(){controllers.set(this,newSignal());}
    get signal(){if(!controllers.has(this)){throw new TypeError('Cannot read private member #signal from an object whose class did not declare it');}return controllers.get(this);}
    abort(reason=new DOMException('This operation was aborted','AbortError')){if(!controllers.has(this))throw new TypeError('Cannot read private member #signal from an object whose class did not declare it');abort(controllers.get(this),reason);}
    get [Symbol.toStringTag](){return 'AbortController';}
  }
  Object.defineProperties(AbortSignal.prototype,{aborted:{enumerable:true},onabort:{enumerable:true}});
  Object.defineProperties(AbortController.prototype,{signal:{enumerable:true},abort:{enumerable:true}});
  for(const [prototype,value]of [[AbortSignal.prototype,'AbortSignal'],[AbortController.prototype,'AbortController']])Object.defineProperty(prototype,Symbol.toStringTag,{value,writable:false,enumerable:false,configurable:true});
  function validateOptions(delay,options,hasDelay){
    if(hasDelay&&delay!==undefined&&typeof delay!=='number')throw invalid('delay','of type number',delay);
    if(options===null||typeof options!=='object'||Array.isArray(options))throw invalid('options','of type object',options);
    if(options.signal!==undefined){const signal=options.signal;if(signal===null||typeof signal!=='object'||!('aborted'in signal))throw invalid('options.signal','an instance of AbortSignal',signal);}
    if(options.ref!==undefined){const ref=options.ref;if(typeof ref!=='boolean')throw invalid('options.ref','of type boolean',ref);}
  }
  // One wait owns only its own registrations. Real target nodes retain the
  // same private budget and stop-propagation resistance as timer cancellation.
  // There is no transport, task handle or additional native callback here.
  async function once(emitter,name,options={}){
    if(options===null||typeof options!=='object'||Array.isArray(options))throw invalid('options','of type object',options);
    const signal=options.signal;
    if(signal!==undefined&&!apply(weakHas,signals,[signal]))throw invalid('options.signal','an instance of AbortSignal',signal);
    if(signal!==undefined&&signalState(signal).aborted)throw abortError(signal);
    const method=(object,key)=>{
      if(object===null||object===undefined)throw new TypeError("Cannot read properties of "+object+" (reading '"+key+"')");
      return object[key];
    };
    return new P((fulfill,fail)=>{
      const registrations={event:undefined,error:undefined,abort:undefined};
      let settled=false;
      const release=kind=>{
        const entry=registrations[kind];
        if(entry===undefined)return;
        registrations[kind]=undefined;
        if(entry.owner)removeNode(entry.owner,entry.node);
        else entry.emitter.removeListener(entry.name,entry.listener);
      };
      const attach=(kind,object,type,listener)=>{
        if(typeof method(object,'on')==='function'){
          object.once(type,listener);
          registrations[kind]={emitter:object,name:type,listener};
          return;
        }
        const owner=apply(weakGet,targets,[object]);
        if(!owner)throw invalid('emitter','an instance of EventEmitter',object);
        const node=addTargetListener(owner,targetType(type),listener,false,true,false,true);
        registrations[kind]={owner,node};
      };
      function resolver(...args){
        if(settled)return;settled=true;
        release('error');release('abort');
        registrations.event=undefined;
        fulfill(args);
      }
      function errorListener(error){
        if(settled)return;settled=true;
        release('event');release('abort');
        registrations.error=undefined;
        fail(error);
      }
      function abortListener(){
        if(settled)return;settled=true;
        release('event');release('error');
        registrations.abort=undefined;
        fail(abortError(signal));
      }
      try{
        attach('event',emitter,name,resolver);
        if(name!=='error'&&typeof emitter.once==='function'){
          emitter.once('error',errorListener);
          registrations.error={emitter,name:'error',listener:errorListener};
        }
        if(signal!==undefined)attach('abort',signal,'abort',abortListener);
      }catch(error){
        // Native allocation failure cannot strand earlier owned listeners.
        // Custom throwing/reentrant registration and cleanup hooks remain a
        // separate, unproven compatibility domain.
        for(const kind of ['event','error','abort']){try{release(kind);}catch{}}
        if(!settled){settled=true;fail(error);}
      }
    });
  }
  function abortError(signal){const error=new Error('The operation was aborted',{cause:signal?.reason});error.name='AbortError';error.code='ABORT_ERR';return error;}
  function subscribe(signal,fn,once=false){
    if(!signal)return ()=>{};
    const state=apply(weakGet,signals,[signal]);
    if(state){
      // This is the actual owned timer-promise cancellation listener. Composite
      // subscriptions remain in the separate private Set and are never exposed.
      const owner=targetState(signal);
      const node=addTargetListener(owner,'abort',fn,false,once,false,true);
      return()=>removeNode(owner,node);
    }
    signal.addEventListener('abort',fn,{once});return()=>signal.removeEventListener('abort',fn);
  }
  function cancelScheduledTimer(stop,fail,source){
    if(this._destroyed)return;
    stop(this);
    fail(abortError(source));
  }
  Object.defineProperty(cancelScheduledTimer,'name',{value:'cancelListenerHandler'});
  function promiseTimer(delay,value,options,immediate){
    try{validateOptions(delay,options,!immediate);}catch(error){return P.reject(error);}
    const {signal,ref=true}=options;
    if(signal?.aborted)return P.reject(abortError(signal));
    let resolve,reject;const result=new P((yes,no)=>{resolve=yes;reject=no;});
    const handle=immediate?setImmediate(()=>resolve(value)):setTimeout(()=>resolve(value),delay);
    if(!ref)handle.unref();
    if(!signal)return result;
    const oncancel=apply(bind,cancelScheduledTimer,[handle,immediate?clearImmediate:clearTimeout,reject,signal]);
    let unsubscribe;
    try{unsubscribe=subscribe(signal,oncancel);}catch(error){
      if(immediate)clearImmediate(handle);else clearTimeout(handle);
      reject(error);return result;
    }
    // Match observable listener lifetime through settlement. Cleanup stays in
    // the native owner rather than relying on a model-replaced remove method.
    return apply(finallyPromise,result,[unsubscribe]);
  }
  function promiseTimeout(delay,value,options={}){return promiseTimer(delay,value,options,false);}
  function promiseImmediate(value,options={}){return promiseTimer(undefined,value,options,true);}
  async function* promiseInterval(delay,value,options={}){
    validateOptions(delay,options,true);const {signal,ref=true}=options;if(signal?.aborted)throw abortError(signal);
    let pending=0,wake;const handle=setInterval(()=>{pending++;if(wake){const yes=wake;wake=undefined;yes();}},delay);if(!ref)handle.unref();
    const onCancel=()=>{clearInterval(handle);if(wake){const yes=wake;wake=undefined;yes(P.reject(abortError(signal)));}};
    let unsubscribe;
    try{unsubscribe=subscribe(signal,onCancel,true);}catch(error){clearInterval(handle);throw error;}
    try{while(!signal?.aborted){if(pending===0)await new P(yes=>{wake=yes;});while(pending>0){yield value;pending--;}}throw abortError(signal);}
    finally{clearInterval(handle);unsubscribe();}
  }
  Object.defineProperty(promiseTimeout,'name',{value:'setTimeout'});Object.defineProperty(promiseImmediate,'name',{value:'setImmediate'});Object.defineProperty(promiseInterval,'name',{value:'setInterval'});
  const schedulerBrand=new WeakSet();
  class Scheduler {
    constructor(){const e=new TypeError('Illegal constructor');e.code='ERR_ILLEGAL_CONSTRUCTOR';throw e;}
    yield(){if(!schedulerBrand.has(this)){const e=new TypeError('Value of "this" must be of type Scheduler');e.code='ERR_INVALID_THIS';throw e;}return promiseImmediate();}
    wait(delay,options){if(!schedulerBrand.has(this)){const e=new TypeError('Value of "this" must be of type Scheduler');e.code='ERR_INVALID_THIS';throw e;}return promiseTimeout(delay,undefined,options);}
  }
  const scheduler=Object.create(Scheduler.prototype);schedulerBrand.add(scheduler);
  const promises={setTimeout:promiseTimeout,setImmediate:promiseImmediate,setInterval:promiseInterval,scheduler};
  const timers={setTimeout,clearTimeout,setImmediate,clearImmediate,setInterval,clearInterval};
  Object.defineProperty(timers,'promises',{enumerable:true,configurable:true,get:()=>promises});
  for(const [fn,target]of [[setTimeout,promiseTimeout],[setImmediate,promiseImmediate]])Object.defineProperty(fn,Symbol.for('nodejs.util.promisify.custom'),{enumerable:true,get:()=>target});
  for(const [name,value]of Object.entries({setTimeout,clearTimeout,setInterval,clearInterval,setImmediate,clearImmediate,queueMicrotask,AbortController,AbortSignal}))Object.defineProperty(globalThis,name,{value,writable:true,enumerable:true,configurable:true});
  const eventTargetObservers=Object.freeze({
    has:target=>apply(weakHas,targets,[target]),
    count:(target,type)=>getMap(targetState(target).counts,type)??0,
    listeners(target,type){
      const root=getMap(targetState(target).types,type),result=[];
      for(let node=root?.first;node;node=node.next)if(!node.removed)result[result.length]=node.listener;
      return result;
    },
    maximum:target=>targetState(target).maximum,
    setMaximum(target,value){validMaximum(value);const state=targetState(target);state.maximum=value;state.warned=false;},
    defaultMaximum:()=>commonMaximum,
    setDefaultMaximum(value){validMaximum(value);commonMaximum=value;}
  });
  const timerExports={timers,promises};
  Object.defineProperty(timerExports,'eventTargetObservers',{value:eventTargetObservers});
  Object.defineProperty(timerExports,'once',{value:once});
  Object.defineProperty(globalThis,'__skyreTimers',{value:timerExports});
  for(const key of ['__skyre_timer_schedule','__skyre_timer_clear','__skyre_timer_refresh','__skyre_microtask','__skyre_event_now'])delete globalThis[key];
})();
