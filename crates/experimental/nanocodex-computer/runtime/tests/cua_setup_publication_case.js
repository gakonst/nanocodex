var {setupCUA} = await import('@oai/cua/tinyskyAlt');
var publicationEvents=[];
var publicationKeys=()=>Object.keys(cua).sort();
var publicationAgent=()=>{var a=globalThis.agent;return {type:typeof a,browserType:typeof a?.browsers}};
var publicationOptions={browser:true,computer:true};
switch(publicationCase){
 case 'ordinary':break;
 case 'agent-observing-setter':{
  let value;
  Object.defineProperty(globalThis,'agent',{configurable:true,get(){return value},set(v){publicationEvents.push({at:'agent-set',keys:publicationKeys(),browserType:typeof v.browsers});value=v}});
  break;
 }
 case 'agent-throwing-setter':
  Object.defineProperty(globalThis,'agent',{configurable:true,set(){publicationEvents.push({at:'agent-set',keys:publicationKeys()});throw Error('owned agent setter')}});
  break;
 case 'agent-readonly-data':
  Object.defineProperty(globalThis,'agent',{configurable:true,writable:false,value:'retained'});break;
 case 'agent-accessor-without-setter':
  Object.defineProperty(globalThis,'agent',{configurable:true,get(){return 'retained'}});break;
 case 'disabled-browser-agent-throwing-setter':
  Object.defineProperty(globalThis,'agent',{configurable:true,set(){publicationEvents.push({at:'agent-set',keys:publicationKeys()});throw Error('disabled agent setter')}});
  publicationOptions={browser:false,computer:true};break;
 case 'facade-nonextensible':Object.preventExtensions(cua);break;
 case 'facade-getState-throwing-setter':
  Object.defineProperty(cua,'getState',{configurable:true,enumerable:true,set(v){publicationEvents.push({at:'cua-getState-set',keys:publicationKeys(),agent:publicationAgent(),valueType:typeof v});throw Error('owned facade setter')}});break;
 case 'facade-getBrowser-observing-setter':
  Object.defineProperty(cua,'getBrowser',{configurable:true,enumerable:true,set(v){publicationEvents.push({at:'cua-getBrowser-set',keys:publicationKeys(),agent:publicationAgent(),valueType:typeof v})}});break;
 default:throw Error('Unknown owned publication case');
}
var publicationInitial=publicationKeys(),publicationResult,publicationError,publicationSyncThrow=false,publicationPromise;
try{publicationPromise=setupCUA(publicationOptions)}catch(e){publicationSyncThrow=true;publicationError=e}
if(!publicationSyncThrow)try{publicationResult=await publicationPromise}catch(e){publicationError=e}
({initial:publicationInitial,events:publicationEvents,syncThrow:publicationSyncThrow,
 fulfilled:publicationError===undefined,resultUndefined:publicationResult===undefined,
 error:publicationError?{name:publicationError.name,message:publicationError.message}:null,
 keys:publicationKeys(),agent:publicationAgent()})
