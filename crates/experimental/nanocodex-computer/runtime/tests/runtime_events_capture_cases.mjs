// Authored synchronous cases: only in-memory events and caught exceptions.
// The adjacent pinned Node binary is the reference, not an original kernel run.
import * as prefixed from 'node:events';
import * as bare from 'events';
const E = prefixed.EventEmitter, monitor = prefixed.errorMonitor;
const cases = [];
const label = value => typeof value === 'symbol' ? {symbol:value.description,global:Symbol.keyFor(value)??null} : value;
const caught = (fn, expected) => {try{return {returned:fn()};}catch(error){return {threw:true,same:error===expected,name:error.name,code:error.code??null,message:error.message};}};
function record(name, fn) {try{cases.push({name,value:fn()});}catch(error){cases.push({name,unexpected:{name:error.name,message:error.message}});}}
function descriptor(object,key) {const d=Object.getOwnPropertyDescriptor(object,key);return d?{enumerable:d.enumerable,configurable:d.configurable,writable:d.writable,value:label(d.value)}:null;}
record('monitor-export-identity-and-descriptors',()=>({type:typeof monitor,identity:monitor===E.errorMonitor&&monitor===bare.errorMonitor&&bare===prefixed,description:label(monitor),namespace:descriptor(prefixed,'errorMonitor'),constructor:descriptor(E,'errorMonitor')}));
for(const order of ['monitor-first','error-first'])record('monitor-precedes-handler-'+order,()=>{
  const e=new E(),error=new Error('owned'),arg={},seen=[];
  function m(...args){seen.push(['monitor',this===e,args[0]===error,args[1]===arg,args.length]);return false;}
  function h(...args){seen.push(['error',this===e,args[0]===error,args[1]===arg,args.length]);}
  if(order==='monitor-first')e.on(monitor,m).on('error',h);else e.on('error',h).on(monitor,m);
  return {result:e.emit('error',error,arg),seen};
});
for(const data of ['error-object',undefined,null,'owned data',17])record('monitor-does-not-handle-'+String(data),()=>{
  const e=new E(),owned=data==='error-object'?new Error('owned unhandled'):data,seen=[];
  e.on(monitor,function(value){seen.push([this===e,value===owned]);return true;});
  const result=caught(()=>e.emit('error',owned),owned);return {result,seen,errorListeners:e.listenerCount('error')};
});
for(const method of ['on','once','prependListener','prependOnceListener'])record('monitor-registration-'+method,()=>{
  const e=new E(),seen=[];function first(){seen.push('first')}function added(){seen.push('added')}
  e.on(monitor,first)[method](monitor,added).on('error',()=>seen.push('error'));
  const before=e.listeners(monitor).map(fn=>fn.name),raw=e.rawListeners(monitor);
  e.emit('error',new Error('one'));e.emit('error',new Error('two'));
  return {before,wrapped:raw.some(fn=>fn.listener===added),seen,remaining:e.listenerCount(monitor)};
});
record('monitor-throw-stops-handler-and-later-monitors',()=>{
  const e=new E(),failure=new Error('monitor failure'),seen=[];
  e.on(monitor,()=>{seen.push('monitor');throw failure}).on(monitor,()=>seen.push('later')).on('error',()=>seen.push('handler'));
  return {result:caught(()=>e.emit('error',new Error('event error')),failure),seen};
});
record('handler-throw-follows-monitor',()=>{
  const e=new E(),failure=new Error('handler failure'),seen=[];
  e.on(monitor,()=>seen.push('monitor')).on('error',()=>{seen.push('handler');throw failure});
  return {result:caught(()=>e.emit('error',new Error('event error')),failure),seen};
});
record('monitor-adds-error-handler-before-error-decision',()=>{
  const e=new E(),seen=[];e.on(monitor,()=>{seen.push('monitor');e.on('error',()=>seen.push('added'))});
  return {result:e.emit('error',new Error('owned')),seen};
});
record('monitor-removes-error-handler-before-error-decision',()=>{
  const e=new E(),owned=new Error('removed handler'),seen=[];function h(){seen.push('handler')}
  e.on(monitor,()=>{seen.push('monitor');e.off('error',h)}).on('error',h);
  return {result:caught(()=>e.emit('error',owned),owned),seen,remaining:e.listenerCount('error')};
});
for(const handled of [true,false])record('monitor-replaces-table-'+handled,()=>{
  const e=new E(),owned=new Error('table replacement'),seen=[];
  e.on(monitor,()=>{seen.push('monitor');e.removeAllListeners();e.on('error',()=>seen.push('new-handler'))});
  if(handled)e.on('error',()=>seen.push('old-handler'));
  return {result:caught(()=>e.emit('error',owned),owned),seen,remaining:e.listenerCount('error')};
});
record('monitor-listener-snapshot-and-nested-error-order',()=>{
  const e=new E(),seen=[];let nested=false;
  function second(error){seen.push('second:'+error.message)}
  function late(error){seen.push('late:'+error.message)}
  e.on(monitor,error=>{seen.push('first:'+error.message);if(!nested){nested=true;e.off(monitor,second);e.on(monitor,late);e.emit('error',new Error('inner'))}}).on(monitor,second).on('error',error=>seen.push('handler:'+error.message));
  return {result:e.emit('error',new Error('outer')),seen};
});
record('monitor-once-removed-before-recursive-error',()=>{
  const e=new E(),seen=[];e.once(monitor,error=>{seen.push('monitor:'+error.message);e.emit('error',new Error('inner'))}).on('error',error=>seen.push('handler:'+error.message));
  return {result:e.emit('error',new Error('outer')),seen,remaining:e.listenerCount(monitor)};
});
record('direct-monitor-emission-is-ordinary-event',()=>{
  const e=new E(),seen=[],owned=new Error('direct');e.on('error',()=>seen.push('error'));
  const absent=e.emit(monitor,owned);e.on(monitor,error=>seen.push(error===owned?'monitor':'bad'));
  return {absent,present:e.emit(monitor,owned),seen};
});
record('monitor-lookalike-symbols-and-strings-are-independent',()=>{
  const e=new E(),seen=[];
  for(const key of [Symbol('events.errorMonitor'),Symbol.for('events.errorMonitor'),'events.errorMonitor'])e.on(key,()=>seen.push('lookalike'));
  e.on(monitor,()=>seen.push('monitor')).on('error',()=>seen.push('error'));
  return {result:e.emit('error',new Error('owned')),seen,distinct:e.eventNames().length};
});
record('monitor-meta-events-and-original-once-listener',()=>{
  const e=new E(),seen=[];function listener(){}
  e.on('newListener',(key,fn)=>{if(key===monitor)seen.push(['new',fn===listener,e.listenerCount(key)])});
  e.on('removeListener',(key,fn)=>{if(key===monitor)seen.push(['remove',fn===listener,e.listenerCount(key)])});
  e.once(monitor,listener).on('error',()=>{});e.emit('error',new Error('owned'));
  return {seen,remaining:e.listenerCount(monitor)};
});
record('monitor-dispatch-uses-overridden-emit',()=>{
  const e=new E(),seen=[],base=e.emit;e.on(monitor,()=>seen.push('monitor')).on('error',()=>seen.push('error'));
  e.emit=function(type,...args){seen.push(type===monitor?'emit-monitor':'emit-'+type);return Reflect.apply(base,this,[type,...args]);};
  return {result:e.emit('error',new Error('owned')),seen};
});
record('overridden-monitor-dispatch-can-stop-error-emission',()=>{
  const e=new E(),seen=[],failure=new Error('override failure'),base=e.emit;
  e.on(monitor,()=>seen.push('monitor')).on('error',()=>seen.push('error'));
  e.emit=function(type,...args){if(type===monitor)throw failure;return Reflect.apply(base,this,[type,...args]);};
  return {result:caught(()=>e.emit('error',new Error('owned')),failure),seen};
});
record('static-monitor-mutation-does-not-rebind-module-symbol',()=>{
  const e=new E(),original=E.errorMonitor,replacement=Symbol('replacement'),seen=[];
  try{E.errorMonitor=replacement;e.on(original,()=>seen.push('original')).on(replacement,()=>seen.push('replacement')).on('error',()=>seen.push('error'));return {result:e.emit('error',new Error('owned')),seen,namedUnchanged:prefixed.errorMonitor===monitor};}finally{E.errorMonitor=original}
});
record('separate-emitter-monitor-registries',()=>{
  const a=new E(),b=new E(),owned=new Error('other emitter'),seen=[];a.on(monitor,()=>seen.push('a'));
  return {result:caught(()=>b.emit('error',owned),owned),seen,counts:[a.listenerCount(monitor),b.listenerCount(monitor)]};
});
if(new Set(cases.map(row=>row.name)).size!==cases.length)throw new Error('duplicate case name');
console.log(JSON.stringify({scope:'finite synchronous errorMonitor; adjacent Node builtin reference',cases}));
