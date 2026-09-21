import {measure,promptFor,VERSION} from './core.mjs';
const json=(body,status=200)=>Response.json(body,{status,headers:{'cache-control':'no-store'}});
// Deploy separate copies with distinct placement.region and short-lived secret bindings.
// Caller controls only a bounded synthetic benchmark case; target and key are operator-owned.
export default {async fetch(request,env) {
  if(!env.BENCH_TOKEN || request.headers.get('authorization')!==`Bearer ${env.BENCH_TOKEN}`) return json({error:'unauthorized'},401);
  if(!env.EXPIRES_AT || !Number.isFinite(Date.parse(env.EXPIRES_AT)) || Date.now()>Date.parse(env.EXPIRES_AT)) return json({error:'expired'},410);
  const evidence={placement_hint:env.PLACEMENT_REGION,cf_placement:request.headers.get('cf-placement'),
    ingress_colo:request.cf?.colo??null,ingress_country:request.cf?.country??null,version:VERSION};
  const path=new URL(request.url).pathname;
  if(path==='/evidence'&&request.method==='GET') return json(evidence);
  if(path!=='/run'||request.method!=='POST')return json({error:'not_found'},404);
  if(Number(request.headers.get('content-length')??0)>2048) return json({error:'too_large'},413);
  let input;try {const raw=await request.text();if(raw.length>2048)throw 0;input=JSON.parse(raw);}catch{return json({error:'invalid_json'},400);}
  const models=(env.ALLOWED_MODELS??'').split(',');
  if(!models.includes(input.model)||typeof input.stream!=='boolean'||!Number.isInteger(input.max_output_tokens)||input.max_output_tokens<1||input.max_output_tokens>256) return json({error:'invalid_case'},400);
  let prompt;try{prompt=promptFor(input.family,input.run_id,input.pair);}catch{return json({error:'invalid_case'},400);}
  const target=new URL(env.TARGET_URL);
  if(target.protocol!=='https:'||target.username||target.password||target.search||target.hash||!target.pathname.endsWith('/responses'))return json({error:'invalid_target'},503);
  const result=await measure({url:target.href,key:env.INFERENCE_KEY,body:{model:input.model,input:prompt,stream:input.stream,max_output_tokens:input.max_output_tokens,store:false}});
  return json({...result,runner_evidence:evidence});
}};
