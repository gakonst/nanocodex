// Independently authored inert synchronous EventEmitter reference cases.
// No services, providers, filesystem, network or asynchronous event helpers.
import * as prefixed from 'node:events';
import * as bare from 'events';
const E = prefixed.EventEmitter;
const cases = [];
const label = key => typeof key === 'symbol' ? {symbol: key.description, global: Symbol.keyFor(key) ?? null} : key;
const atom = value => value === undefined ? {type:'undefined'} : typeof value === 'number' && !Number.isFinite(value) ? {number:String(value)} : typeof value === 'symbol' ? label(value) : typeof value === 'function' ? {function:value.name,length:value.length} : value;
const error = e => ({name:e?.name ?? null,code:e?.code ?? null,message:e?.message ?? String(e)});
const caught = fn => {try{return {returned:atom(fn())};}catch(e){return {threw:error(e)};}};
function record(name, fn) {try {cases.push({name,value:fn()});} catch(e) {cases.push({name,unexpected:error(e)});}}
function descriptors(object) {
  return Reflect.ownKeys(object).map(key=>{
    const d=Object.getOwnPropertyDescriptor(object,key);
    return {key:label(key),enumerable:d.enumerable,configurable:d.configurable,...('value' in d?{kind:'data',writable:d.writable,type:typeof d.value,value:typeof d.value==='function'?atom(d.value):typeof d.value==='symbol'?label(d.value):['string','number','boolean','undefined'].includes(typeof d.value)?atom(d.value):null}:{kind:'accessor',get:atom(d.get),set:atom(d.set)})};
  });
}
const inventory={
  aliases: {namespace:bare===prefixed,default:bare.default===prefixed.default,named:bare.EventEmitter===E,defaultIsNamed:prefixed.default===E,constructorSelf:E.EventEmitter===E},
  namespace:descriptors(prefixed),constructor:descriptors(E),prototype:descriptors(E.prototype),
  namespacePrototypeIsNull:Object.getPrototypeOf(prefixed)===null,
  namespaceExtensible:Object.isExtensible(prefixed),defaultMaxListeners:E.defaultMaxListeners
};
record('construct-and-independent-state',()=>{
  const a=new E(),b=new E();const fn=()=>{};
  return {instance:a instanceof E,proto:Object.getPrototypeOf(a)===E.prototype,empty:a.eventNames(),registered:a.on('x',fn)===a,counts:[a.listenerCount('x'),b.listenerCount('x')],constructor:a.constructor===E};
});
record('construct-without-new',()=>caught(()=>{const a=E();return{type:typeof a,instance:a instanceof E,events:a.eventNames()};}));
record('ordinary-subclass',()=>{class Own extends E{constructor(){super();this.owned=7;}}const a=new Own();let seen;a.on('x',function(){seen=[this===a,this.owned]});a.emit('x');return{instance:a instanceof E,subclass:a instanceof Own,seen};});
record('call-constructor-on-ordinary-receiver',()=>{const receiver={};E.call(receiver);return{keys:Object.keys(receiver),hasEvents:typeof receiver._events,prototypeUnchanged:Object.getPrototypeOf(receiver)===Object.prototype};});
record('zero-listener-return',()=>{const a=new E();return[a.emit('x'),a.emit(Symbol('x')),a.eventNames()];});
for(const count of [1,2,5]) record('emit-order-this-args-'+count,()=>{
  const a=new E(),arg={},seen=[];
  for(let i=0;i<count;i++)a.on('x',function(...args){seen.push([i,this===a,args.length,args[0]===arg,args[1],args[2]===undefined]);return 100+i});
  return{result:a.emit('x',arg,'value',undefined),seen,count:a.listenerCount('x')};
});
for(const method of ['on','addListener','prependListener','once','prependOnceListener'])record('registration-return-'+method,()=>{const a=new E();function f(){};return{same:a[method]('x',f)===a,registered:a.listenerCount('x'),afterFirst:a.emit('x'),afterSecond:a.emit('x'),remaining:a.listenerCount('x')};});
record('duplicate-and-prepend-order',()=>{const a=new E(),seen=[];function f(){seen.push('f')}function g(){seen.push('g')}a.on('x',f).on('x',g).on('x',f).prependListener('x',g);a.emit('x');return{seen,listeners:a.listeners('x').map(fn=>fn.name),count:a.listenerCount('x'),selected:a.listenerCount('x',f)};});
record('once-order-and-wrapper-identity',()=>{const a=new E(),seen=[];function once(){seen.push('once')}function stable(){seen.push('stable')}function prep(){seen.push('prep')}a.on('x',stable).once('x',once).prependOnceListener('x',prep);const raw=a.rawListeners('x'),plain=a.listeners('x');const first=a.emit('x'),second=a.emit('x');return{seen,first,second,unwrapped:plain[0]===prep&&plain[1]===stable&&plain[2]===once,rawWrapper:raw[0]!==prep&&raw[0].listener===prep,stableRaw:raw[1]===stable,onceRaw:raw[2].listener===once,remaining:a.listeners('x').map(fn=>fn.name)};});
record('once-recursive-emission-removes-before-call',()=>{const a=new E(),seen=[];a.once('x',function(){seen.push(['once',a.listenerCount('x')]);seen.push(['nested',a.emit('x')]);});a.on('x',()=>seen.push(['stable']));return{result:a.emit('x'),seen,count:a.listenerCount('x')};});
for(const method of ['removeListener','off'])record('remove-one-last-duplicate-'+method,()=>{const a=new E(),seen=[];function f(){seen.push('f')}function g(){seen.push('g')}a.on('x',f).on('x',g).on('x',f);const same=a[method]('x',f)===a;a.emit('x');return{same,seen,names:a.listeners('x').map(fn=>fn.name),selected:a.listenerCount('x',f)};});
for(const onceMethod of ['once','prependOnceListener'])for(const removeBy of ['original','wrapper'])record('remove-once-'+onceMethod+'-'+removeBy,()=>{const a=new E();let called=0;function f(){called++}a[onceMethod]('x',f);const wrapper=a.rawListeners('x')[0];a.removeListener('x',removeBy==='wrapper'?wrapper:f);return{called,emitted:a.emit('x'),count:a.listenerCount('x')};});
record('remove-missing-noop',()=>{const a=new E();function f(){}function g(){}a.on('x',f);return{same:a.removeListener('missing',f)===a,other:a.removeListener('x',g)===a,count:a.listenerCount('x'),events:a.eventNames()};});
record('once-wrapper-direct-call',()=>{const a=new E(),seen=[];function f(...args){seen.push([this===a,args]);return 42}a.once('x',f);const raw=a.rawListeners('x')[0];return{first:atom(raw('direct')),second:atom(raw('twice')),seen,remaining:a.listenerCount('x'),event:a.emit('x')};});
record('mutate-remove-later-listener',()=>{const a=new E(),seen=[];function second(){seen.push('second')}a.on('x',()=>{seen.push('first');a.off('x',second)}).on('x',second);a.emit('x');a.emit('x');return{seen,count:a.listenerCount('x')};});
record('mutate-remove-self',()=>{const a=new E(),seen=[];function first(){seen.push('first');a.off('x',first)}a.on('x',first).on('x',()=>seen.push('last'));a.emit('x');a.emit('x');return{seen,count:a.listenerCount('x')};});
record('mutate-add-listener-deferred-until-next-emit',()=>{const a=new E(),seen=[];let added=false;function late(){seen.push('late')}a.on('x',()=>{seen.push('first');if(!added){added=true;a.on('x',late)}}).on('x',()=>seen.push('second'));a.emit('x');a.emit('x');return{seen,count:a.listenerCount('x')};});
record('mutate-remove-all-preserves-current-snapshot',()=>{const a=new E(),seen=[];a.on('x',()=>{seen.push('first');a.removeAllListeners('x')}).on('x',()=>seen.push('second'));const first=a.emit('x'),second=a.emit('x');return{first,second,seen,count:a.listenerCount('x')};});
record('mutate-nested-emit-sees-current-registry',()=>{const a=new E(),seen=[];let depth=0;function late(){seen.push('late'+depth)}a.on('x',()=>{seen.push('first'+depth);if(depth===0){a.on('x',late);depth++;a.emit('x');depth--}}).on('x',()=>seen.push('second'+depth));a.emit('x');return{seen,count:a.listenerCount('x')};});
record('listeners-snapshots-are-independent',()=>{const a=new E();function f(){}function g(){}a.on('x',f).once('x',g);const plain=a.listeners('x'),raw=a.rawListeners('x');plain.pop();raw.length=0;return{counts:[plain.length,raw.length,a.listenerCount('x')],plain:a.listeners('x').map(x=>x.name),rawOnce:a.rawListeners('x')[1].listener===g};});
record('event-key-order-and-symbol-identity',()=>{const a=new E(),s1=Symbol('same'),s2=Symbol('same');function f(){}for(const key of ['z','2','1','',s1,'a',s2])a.on(key,f);return{events:a.eventNames().map(label),s1:a.listenerCount(s1),s2:a.listenerCount(s2),foreign:a.listenerCount(Symbol('same'))};});
for(const key of ['','__proto__','constructor','toString','hasOwnProperty','0','01','é','😀'])record('string-event-'+JSON.stringify(key),()=>{const a=new E(),seen=[];a.on(key,value=>seen.push(value));const present=a.emit(key,7);a.off(key,a.listeners(key)[0]);return{present,seen,removed:a.emit(key,8),keys:a.eventNames()};});
for(const key of [null,undefined,12,true])record('ordinary-event-key-coercion-'+String(key),()=>{const a=new E(),seen=[];a.on(key,()=>seen.push('called'));return{result:a.emit(String(key)),seen,keys:a.eventNames()};});
record('object-key-coercion-order',()=>{const a=new E(),seen=[];const key={toString(){seen.push('key');return'owned'}};function f(){seen.push('listener')}a.on(key,f);a.emit(key);a.off(key,f);return{seen,count:a.listenerCount('owned')};});
record('newListener-before-insertion-and-nesting',()=>{const a=new E(),seen=[];let nested=false;function f(){}function extra(){}a.on('newListener',(name,fn)=>{seen.push([String(name),fn.name,a.listenerCount(name)]);if(name==='x'&&!nested){nested=true;a.on('x',extra)}});a.on('x',f);return{seen,names:a.listeners('x').map(fn=>fn.name)};});
record('meta-events-unwrap-once-original',()=>{const a=new E(),seen=[];function original(){}a.on('newListener',(name,fn)=>{if(name==='x')seen.push(['new',fn===original,a.listenerCount(name)])});a.on('removeListener',(name,fn)=>{if(name==='x')seen.push(['remove',fn===original,a.listenerCount(name)])});a.once('x',original);a.emit('x');return{seen,count:a.listenerCount('x')};});
record('removeAll-meta-order',()=>{const a=new E(),seen=[];function f(){}function g(){}function h(){}a.on('removeListener',(name,fn)=>seen.push([label(name),fn.name,a.listenerCount(name)]));a.on('x',f).on('x',g).on('y',h);const same=a.removeAllListeners()===a;return{same,seen,events:a.eventNames()};});
record('removeAll-one-key-retains-other-and-symbol',()=>{const a=new E(),s=Symbol('s');function f(){}a.on('x',f).once('x',f).on('y',f).on(s,f);return{same:a.removeAllListeners('x')===a,events:a.eventNames().map(label),counts:[a.listenerCount('x'),a.listenerCount('y'),a.listenerCount(s)]};});
record('caught-unhandled-error-preserves-object',()=>{const a=new E(),owned=new Error('owned error');try{a.emit('error',owned);return{threw:false}}catch(e){return{threw:true,same:e===owned,...error(e),count:a.listenerCount('error')}}});
for(const value of [undefined,null,'owned error data',17])record('caught-unhandled-error-value-'+String(value),()=>{const a=new E();return caught(()=>a.emit('error',value));});
record('handled-error-is-ordinary-synchronous-event',()=>{const a=new E(),owned=new Error('owned'),seen=[];a.on('error',function(value){seen.push([this===a,value===owned])});return{result:a.emit('error',owned),seen};});
record('listener-throw-stops-current-dispatch',()=>{const a=new E(),owned=new Error('owned listener'),seen=[];a.on('x',()=>{seen.push('first');throw owned}).on('x',()=>seen.push('second'));let same;try{a.emit('x')}catch(e){same=e===owned}return{same,seen,count:a.listenerCount('x')};});
record('once-listener-throw-remains-removed',()=>{const a=new E(),owned=new Error('owned once'),seen=[];a.once('x',()=>{seen.push('once');throw owned}).on('x',()=>seen.push('stable'));let same;try{a.emit('x')}catch(e){same=e===owned}const second=a.emit('x');return{same,seen,second,count:a.listenerCount('x')};});
for(const method of ['on','addListener','prependListener','once','prependOnceListener','removeListener','off'])for(const bad of [null,undefined,1,{},'listener'])record('invalid-listener-'+method+'-'+(bad===null?'null':typeof bad),()=>{const a=new E();return caught(()=>a[method]('x',bad));});
record('invalid-listener-before-key-coercion',()=>{const a=new E(),seen=[];const key={toString(){seen.push('coerced');return'x'}};const result=caught(()=>a.on(key,null));return{result,seen,events:a.eventNames()};});
record('ordinary-max-listener-state',()=>{const a=new E(),b=new E(),before=E.defaultMaxListeners;const values=[];for(const n of [0,1,3,Infinity]){values.push([n===Infinity?'Infinity':n,a.setMaxListeners(n)===a,a.getMaxListeners()===n,b.getMaxListeners()]);}return{before,values};});
for(const bad of [-1,NaN,'3',null,undefined,{}])record('invalid-setMaxListeners-'+(Number.isNaN(bad)?'NaN':bad===null?'null':typeof bad),()=>caught(()=>new E().setMaxListeners(bad)));
record('defaultMaxListeners-shared-by-alias-and-instance-overrides',()=>{const before=E.defaultMaxListeners;try{const prior=new E(),own=new E().setMaxListeners(2);E.defaultMaxListeners=4;const next=new bare.EventEmitter();return{alias:bare.default.defaultMaxListeners,prior:prior.getMaxListeners(),next:next.getMaxListeners(),own:own.getMaxListeners()};}finally{E.defaultMaxListeners=before}});
record('state-survives-ordinary-caught-error-and-removal',()=>{const a=new E(),seen=[];function stable(x){seen.push(x)}a.on('x',stable);const failure=caught(()=>a.on('x',null));const emitted=a.emit('x','after');a.off('x',stable);return{failure,emitted,seen,remaining:a.eventNames()};});
const names=cases.map(x=>x.name);
if(new Set(names).size!==names.length)throw new Error('Duplicate authored case names');
console.log(JSON.stringify({scope:'direct adjacent Node builtin provider; not original kernel import-surface proof',inventory,cases}));
