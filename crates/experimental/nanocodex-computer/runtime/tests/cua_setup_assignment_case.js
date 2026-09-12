// Own-property observation in the existing test-only first-setup seam.
// No installed-original realm or provider authority is inferred from this case.
var {setupCUA} = await import('@oai/cua/tinyskyAlt');
var assignmentEvents=[], assignmentFacade=cua, assignmentAgentValue;
var assignmentKeys=()=>Object.keys(cua).sort();
var assignmentDescriptor=Object.getOwnPropertyDescriptor(Object,'assign');
var assignmentOriginal=assignmentDescriptor.value;
var assignmentFail=assignmentCase==='agent-installs-throwing-temporary-observer';
Object.defineProperty(globalThis,'agent',{configurable:true,
 get(){return assignmentAgentValue},
 set(value){
  assignmentAgentValue=value;
  assignmentEvents.push({at:'agent-set',keys:assignmentKeys()});
  Object.defineProperty(Object,'assign',{...assignmentDescriptor,value:function(target,...sources){
   const phase=target===assignmentFacade?'outer':'temporary';
   assignmentEvents.push({at:'assignment',phase,members:sources.map(source=>Object.keys(source).sort()),agentPublished:globalThis.agent===value});
   if(assignmentFail&&phase==='temporary'&&sources.some(source=>Object.hasOwn(source,'browsers')))
    throw Error('owned temporary browser assignment');
   return Reflect.apply(assignmentOriginal,this,[target,...sources]);
  }});
 }
});
var assignmentInitial=assignmentKeys(),assignmentResult,assignmentError,assignmentSyncThrow=false,assignmentPromise;
try {
 try {assignmentPromise=setupCUA({browser:true,computer:true})} catch(error) {assignmentSyncThrow=true;assignmentError=error}
 if(!assignmentSyncThrow)try {assignmentResult=await assignmentPromise} catch(error) {assignmentError=error}
} finally {Object.defineProperty(Object,'assign',assignmentDescriptor)}
({initial:assignmentInitial,events:assignmentEvents,syncThrow:assignmentSyncThrow,
 fulfilled:assignmentError===undefined,resultUndefined:assignmentResult===undefined,
 error:assignmentError?{name:assignmentError.name,message:assignmentError.message}:null,
 keys:assignmentKeys(),agent:{type:typeof globalThis.agent,browserType:typeof globalThis.agent?.browsers}})
