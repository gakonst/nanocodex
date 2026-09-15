// Independently authored synchronous helper cases. No services or asynchronous
// helpers execute. EventTarget observations are labeled outside the native slice.
import * as prefixed from 'node:events';
import * as bare from 'events';
const E=prefixed.EventEmitter;
const helperNames=['listenerCount','getEventListeners','getMaxListeners','setMaxListeners'];
const cases=[];
const atom=x=>x===undefined?{undefined:true}:typeof x==='number'&&!Number.isFinite(x)?{number:String(x)}:typeof x==='number'&&Object.is(x,-0)?{number:'-0'}:typeof x==='symbol'?{symbol:x.description}:typeof x==='function'?{function:x.name,length:x.length}:x;
const err=e=>({name:e?.name,code:e?.code??null,message:e?.message??String(e)});
const caught=f=>{try{return {returned:atom(f())}}catch(e){return {threw:err(e)}}};
function record(id,fn){try{cases.push({id,value:fn()})}catch(e){cases.push({id,unexpected:err(e)})}}
const describe=o=>Reflect.ownKeys(o).map(k=>{const d=Object.getOwnPropertyDescriptor(o,k);return{key:atom(k),enumerable:d.enumerable,configurable:d.configurable,...('value'in d?{writable:d.writable,type:typeof d.value,value:typeof d.value==='object'?null:atom(d.value)}:{get:atom(d.get),set:atom(d.set)})}});
const inventory={aliases:bare===prefixed,helpers:helperNames.map(name=>({name,namedStatic:prefixed[name]===E[name],aliases:bare[name]===prefixed[name],descriptor:describe(E).find(d=>d.key===name),functionDescriptors:describe(prefixed[name])}))};
for(const count of [0,1,3])record('ordinary-count-and-unwrapped-copy-'+count,()=>{
 const e=new E();function f(){}function once(){}for(let i=0;i<count;i++)e.on('x',f);e.once('once',once);
 const a=prefixed.getEventListeners(e,'x'),b=prefixed.getEventListeners(e,'x');a.length=0;
 return {count:prefixed.listenerCount(e,'x'),ignoredThird:prefixed.listenerCount(e,'x',once),copies:a!==b,remaining:e.listenerCount('x'),names:b.map(f=>f.name),once:prefixed.getEventListeners(e,'once')[0]===once};
});
record('symbol-key-and-key-coercion',()=>{const e=new E(),s=Symbol('s'),seen=[];function f(){}e.on(s,f).on('owned',f);const key={toString(){seen.push('key');return'owned'}};return{symbol:prefixed.listenerCount(e,s),foreign:prefixed.getEventListeners(e,Symbol('s')).length,count:prefixed.listenerCount(e,key),listeners:prefixed.getEventListeners(e,key).map(f=>f.name),seen}});
for(const method of ['listenerCount','listeners','getMaxListeners'])record('ordinary-instance-hook-'+method,()=>{
 const seen=[],token={owned:true},e=new E();e[method]=function(...args){seen.push([this===e,args.length,...args.map(atom)]);return token};
 const name=method==='listeners'?'getEventListeners':method;
 const value=prefixed[name](e,'x','ignored');return{same:value===token,seen};
});
for(const [helper,method] of [['listenerCount','listenerCount'],['getEventListeners','listeners'],['getMaxListeners','getMaxListeners']])record('hook-read-order-'+helper,()=>{
 const seen=[],e={},first=function(){seen.push('first-call');return 1},second=function(...args){seen.push(['second-call',this===e,args.map(atom)]);return 7};let reads=0;
 Object.defineProperty(e,method,{get(){seen.push('get');return ++reads===1?first:second}});
 return{result:caught(()=>prefixed[helper](e,'x')),seen};
});
for(const [helper,method] of [['listenerCount','listenerCount'],['getEventListeners','listeners'],['getMaxListeners','getMaxListeners']])record('hook-throw-identity-'+helper,()=>{
 const expected=Error('owned getter'),e={};Object.defineProperty(e,method,{get(){throw expected}});let observed;
 try{prefixed[helper](e,'x')}catch(e){observed=e}return{same:expected===observed};
});
record('listenerCount-missing-hook-rejects',()=>{const e=new E();function f(){}e.on('x',f).once('x',f);e.listenerCount=null;return caught(()=>prefixed.listenerCount(e,'x'))});
record('listenerCount-missing-hook-does-not-use-prototype-replacement',()=>{const e=new E();function f(){}e.on('x',f);e.listenerCount=null;const previous=E.prototype.listenerCount;try{E.prototype.listenerCount=()=>123;return caught(()=>prefixed.listenerCount(e,'x'))}finally{E.prototype.listenerCount=previous}});
for(const [id,value] of [['null',null],['undefined',undefined],['zero',0],['false',false],['string','owned'],['object',{}],['array',[]]])for(const helper of ['listenerCount','getEventListeners','getMaxListeners'])record('invalid-emitter-'+helper+'-'+id,()=>caught(()=>prefixed[helper](value,'x')));
record('set-default-and-existing-overrides',()=>{const previous=E.defaultMaxListeners;try{const inherited=new E(),own=new E().setMaxListeners(2);const value=prefixed.setMaxListeners(7);return{result:atom(value),default:E.defaultMaxListeners,namedDefault:prefixed.defaultMaxListeners,prior:prefixed.getMaxListeners(inherited),own:prefixed.getMaxListeners(own),later:prefixed.getMaxListeners(new E())}}finally{E.defaultMaxListeners=previous}});
record('set-omitted-count-uses-current-default',()=>{const previous=E.defaultMaxListeners;try{E.defaultMaxListeners=9;const e=new E().setMaxListeners(2);return{result:atom(prefixed.setMaxListeners(undefined,e)),count:e.getMaxListeners(),default:E.defaultMaxListeners}}finally{E.defaultMaxListeners=previous}});
for(const [id,n] of [['zero',0],['negative-zero',-0],['fraction',2.5],['infinity',Infinity],['negative',-1],['nan',NaN],['negative-infinity',-Infinity],['string','3'],['null',null],['object',{}]])record('set-value-'+id,()=>{const e=new E(),before=e.getMaxListeners();const result=caught(()=>prefixed.setMaxListeners(n,e));return{result,before,after:atom(e.getMaxListeners())}});
record('set-target-order-and-return',()=>{const seen=[],a=new E(),b={setMaxListeners(n){seen.push([this===b,n]);return'ignored'}},c=new E();const value=prefixed.setMaxListeners(4,a,b,c);return{result:atom(value),a:a.getMaxListeners(),c:c.getMaxListeners(),seen}});
record('set-target-getter-order',()=>{const seen=[],e={};let reads=0;Object.defineProperty(e,'setMaxListeners',{get(){seen.push('get');return ++reads===1?()=>seen.push('first'):function(n){seen.push(['second',this===e,n])}}});return{result:caught(()=>prefixed.setMaxListeners(5,e)),seen}});
record('set-numeric-validation-before-target-effects',()=>{const seen=[],e={get setMaxListeners(){seen.push('target-get');return()=>seen.push('call')}};return{result:caught(()=>prefixed.setMaxListeners(-1,e)),seen}});
for(const [id,bad] of [['null',null],['undefined',undefined],['object',{}],['number',4],['string','owned']])record('set-partial-before-invalid-'+id,()=>{const a=new E(),c=new E(),result=caught(()=>prefixed.setMaxListeners(4,a,bad,c));return{result,first:a.getMaxListeners(),later:c.getMaxListeners()}});
record('set-target-throw-preserves-identity-and-prior-effects',()=>{const a=new E(),c=new E(),expected=Error('owned target');let actual;try{prefixed.setMaxListeners(8,a,{setMaxListeners(){throw expected}},c)}catch(e){actual=e}return{same:actual===expected,first:a.getMaxListeners(),later:c.getMaxListeners()}});
record('getMaxListeners-stored-value-not-hook-result',()=>{const seen=[],e={_maxListeners:12,getMaxListeners(){seen.push('called');return 99}};return{value:prefixed.getMaxListeners(e),seen}});
record('getMaxListeners-stored-read-order',()=>{const seen=[],e={getMaxListeners(){throw Error('must not call')}};let reads=0;Object.defineProperty(e,'_maxListeners',{get(){seen.push('max');return ++reads===1?4:8}});return{value:prefixed.getMaxListeners(e),seen}});
record('getMaxListeners-undefined-stored-read-once',()=>{const seen=[],e={getMaxListeners(){throw Error('must not call')}};Object.defineProperty(e,'_maxListeners',{get(){seen.push('max');return undefined}});return{value:prefixed.getMaxListeners(e),seen}});
record('getMaxListeners-prototype-override-not-invoked',()=>{const e=new E(),previous=E.prototype.getMaxListeners;try{E.prototype.getMaxListeners=()=>999;return prefixed.getMaxListeners(e)}finally{E.prototype.getMaxListeners=previous}});
record('getMaxListeners-unvalidated-stored-null',()=>{const e=new E();e._maxListeners=null;return prefixed.getMaxListeners(e)});
record('set-with-no-arguments-preserves-current-default',()=>{const previous=E.defaultMaxListeners;try{E.defaultMaxListeners=6;const result=prefixed.setMaxListeners();return{result:atom(result),value:E.defaultMaxListeners}}finally{E.defaultMaxListeners=previous}});
record('set-invalid-value-does-not-change-global-default',()=>{const previous=E.defaultMaxListeners;try{E.defaultMaxListeners=6;return{result:caught(()=>prefixed.setMaxListeners(-1)),value:E.defaultMaxListeners}}finally{E.defaultMaxListeners=previous}});
// This characterization is separate from the ordinary EventEmitter comparison.
// The Rust-owned AbortSignal store is not visible to runtime_events.js.
const eventTargets=[];
if(typeof EventTarget==='function'&&typeof AbortController==='function')for(const kind of ['EventTarget','AbortSignal']) {
 const controller=kind==='AbortSignal'?new AbortController():null,target=controller?.signal??new EventTarget();function f(){}function once(){}target.addEventListener('owned',f);target.addEventListener('owned',once,{once:true});
 eventTargets.push({kind,listeners:prefixed.getEventListeners(target,'owned').map(f=>f.name),listenerCount:caught(()=>prefixed.listenerCount(target,'owned')),before:prefixed.getMaxListeners(target),set:atom(prefixed.setMaxListeners(3,target)),after:prefixed.getMaxListeners(target)});
}
if(new Set(cases.map(c=>c.id)).size!==cases.length)throw Error('duplicate helper case');
console.log(JSON.stringify({scope:'synchronous Node builtin helpers; native EventTarget domain explicitly separate',inventory,cases,eventTargets}));
