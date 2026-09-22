import {resolveThreadRoute,routingPolicySchema,projectThreadRouteDiagnostics} from '../../js/managed/src/thread-model-routing.ts';
import {measure} from './core.mjs';
import {SOURCE,CANDIDATES,POLICY,parseCase,rayColo,assertFresh} from './jev-core.mjs';
const json=(body,status=200)=>Response.json(body,{status,headers:{'cache-control':'no-store'}});
// Separate per temporary Worker namespace; persisted reservations survive restarts.
export class JevBudget {
 constructor(ctx){this.ctx=ctx;}
 async fetch(request){
  const {id,receipt}=await request.json();
  if(!/^[0-2]:(global|regional)$/.test(id??''))return json({error:'invalid_case'},400);
  return this.ctx.storage.transaction(async tx=>{
   const key=`case:${id}`,prior=await tx.get(key);
   if(receipt){if(!prior)return json({error:'missing_reservation'},409);await tx.put(key,{...prior,receipt});return json({saved:true});}
   if(prior)return json({error:'already_reserved_no_retry',reservation:prior},409);
   const reservation={reserved_at:new Date().toISOString(),classification_upper_bound:1,downstream_upper_bound:1};
   await tx.put(key,reservation);return json(reservation);
  });
 }
}
export default {async fetch(request,env){
 if(!env.BENCH_TOKEN||request.headers.get('authorization')!==`Bearer ${env.BENCH_TOKEN}`)return json({error:'unauthorized'},401);
 if(!Number.isFinite(Date.parse(env.EXPIRES_AT))||Date.now()>Date.parse(env.EXPIRES_AT))return json({error:'expired'},410);
 const evidence={source:SOURCE,runner:env.RUNNER_ID,placement_hint:env.PLACEMENT_REGION,controller_ingress_colo:request.cf?.colo??null,expected_api_ingress:env.EXPECTED_API_INGRESS,calibration_sha256:env.CALIBRATION_SHA256};
 const path=new URL(request.url).pathname;
 if(path==='/evidence'&&request.method==='GET')return json(evidence);
 if(path!=='/run'||request.method!=='POST')return json({error:'not_found'},404);
 let input;try{const raw=await request.text();if(raw.length>256)throw 0;input=parseCase(JSON.parse(raw));}catch{return json({error:'invalid_case'},400);}
 let calibration;try{calibration=JSON.parse(env.CALIBRATION_JSON ?? Array.from({length:8},(_,i)=>env[`CALIBRATION_JSON_${i}`]??'').join(''));assertFresh(calibration);}catch{return json({error:'calibration_unusable'},503);}
 const target=new URL(env.TARGET_URL);
 if(target.protocol!=='https:'||target.username||target.password||target.search||target.hash||!target.pathname.endsWith('/responses'))return json({error:'invalid_target'},503);
 // Non-inference HEAD validates API ingress from the placed runner. Neither JSON
 // nor request headers can supply an origin. Ray is ingress evidence, not compute.
 const preflightStarted=performance.now();
 let preflight;try{
  const r=await fetch(target,{method:'HEAD',redirect:'manual',signal:AbortSignal.timeout(15000)});
  preflight={method:'HEAD',http_status:r.status,cf_ray:r.headers.get('cf-ray'),api_ingress:rayColo(r.headers.get('cf-ray'))};await r.body?.cancel();
 }catch{return json({error:'origin_preflight_failed',evidence},503);}
 if(preflight.api_ingress!==env.EXPECTED_API_INGRESS)return json({error:'origin_preflight_mismatch',evidence,preflight},503);
 const preflight_ms=performance.now()-preflightStarted;
 const budget=env.JEV_BUDGET.get(env.JEV_BUDGET.idFromName('campaign-v1'));
 const reserved=await budget.fetch('https://budget/reserve',{method:'POST',body:JSON.stringify({id:input.id})});
 if(!reserved.ok)return json({error:'case_already_reserved_no_retry'},409);
 const classification={model:'typesafe/jev',attempts:0,settled:false,ok:null,duration_ms:null,input_bytes:null};
 const started=performance.now();
 try{
  const ai={async run(model,payload){
   if(model!=='typesafe/jev'||classification.attempts!==0)throw Error('classification_limit');
   classification.attempts++;classification.input_bytes=new TextEncoder().encode(JSON.stringify(payload)).length;const t=performance.now();
   try{const result=await env.AI.run(model,payload);classification.ok=true;classification.response_state=typeof result?.state==='string'?result.state:null;return result;}
   catch{classification.ok=false;throw Error('jev_binding_error');}
   finally{classification.settled=true;classification.duration_ms=performance.now()-t;}
  }};
  const route=await resolveThreadRoute(ai,input.prompt.text,routingPolicySchema.parse(POLICY),{openrouter:true,vercel:true,cloudflare:true,workerColo:null,clientIngressColo:input.arm==='regional'?env.EXPECTED_API_INGRESS:null,provider_performance:calibration.metrics});
  const chosen=route.audit?.candidate_choice;
  if(!CANDIDATES.includes(chosen))throw Error('invalid_selected_candidate');
  const classificationAtSelection={...classification};
  const downstreamStarted=performance.now();
  const downstream=await measure({url:target.href,key:env.INFERENCE_KEY,body:{model:chosen,input:input.prompt.text,stream:true,store:false,max_output_tokens:256}});
  const downstreamColo=rayColo(downstream.headers?.['cf-ray']);
  const trustedIngress=downstream.headers?.['x-nanocodex-ingress-colo']??null;
  const regionValid=trustedIngress===env.EXPECTED_API_INGRESS&&downstreamColo===env.EXPECTED_API_INGRESS;
  const result={source:SOURCE,arm:input.arm,prompt_id:input.prompt.id,prompt_family:input.prompt.family,calibration_sha256:env.CALIBRATION_SHA256,evidence,preflight,preflight_ms,classification:classificationAtSelection,route,diagnostics:projectThreadRouteDiagnostics(route),downstream,downstream_api_ingress:trustedIngress,downstream_ray_colo:downstreamColo,experiment_first_meaningful_ms:Number.isFinite(downstream.first_meaningful_ms)?downstreamStarted-started+downstream.first_meaningful_ms:null,timing_boundary:'After protected-wrapper HEAD and durable reservation; outer Jev plus downstream API delivery, including API inner routing',origin_validated:regionValid,experiment_total_ms:performance.now()-started,error:!regionValid?'downstream_origin_mismatch':downstream.error??null};
  await budget.fetch('https://budget/receipt',{method:'POST',body:JSON.stringify({id:input.id,receipt:{classification_attempts:classification.attempts,downstream_attempts:1,selected:chosen,error:result.error}})});
  return json(result);
 }catch{
  return json({error:'experiment_outcome_unknown',source:SOURCE,evidence,preflight,classification:{...classification},attempt_upper_bound:2},500);
 }
}};
