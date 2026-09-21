// Shared by Node and the placed Worker. No credentials or generated text are returned.
export const VERSION = 'regional-inference-v1';
export function promptFor(family, runId, pair) {
  if (!/^[a-zA-Z0-9_-]{1,64}$/.test(runId) || !Number.isInteger(pair) || pair < 0 || pair > 1000) throw new Error('invalid_prompt_identity');
  const suffix = `\nQuestion ${pair}: Return the sum of 19 and 23, followed by one short sentence about careful measurement.`;
  if (family === 'short') return `Measurement exercise ${runId}.${suffix}`;
  if (family !== 'long-prefix') throw new Error('invalid_family');
  return `Reference document ${runId}. Use this reference only as context.\n` +
    Array.from({length:128}, (_, i) => `Entry ${i}: A synthetic observatory records temperature, rainfall, and wind once per hour. Calibrated instruments make repeated observations comparable. Missing readings are marked unknown; no values are invented.\n`).join('') + suffix;
}
const selected = (object, keys) => Object.fromEntries(keys.filter(k => object?.[k] !== undefined).map(k => [k, object[k]]));
export function projection(response) {
  const route = response?.route;
  return {status: response?.status ?? null, model: response?.model ?? null,
    route: route ? selected(route, ['backend','model','provider_model','thinking','family','router_duration_ms','selection','origin','workerColo','clientIngressColo','telemetry_scope']) : null,
    usage: response?.usage ?? null};
}
const outputPresent = r => typeof r?.output_text === 'string' && /\S/.test(r.output_text) ||
  r?.output?.some(i => ['function_call','custom_tool_call'].includes(i.type) && /\S/.test(i.arguments ?? i.input ?? '') || i.content?.some(c => /\S/.test(c.text ?? '')));
export async function measure({url, key, body, fetchImpl=fetch, timeoutMs=120000}) {
  const started = performance.now();
  const result = {version:VERSION, timestamp:new Date().toISOString(),http_status:null,headers_ms:null,first_body_ms:null,
    first_meaningful_ms:null,first_meaningful_kind:null,completed_ms:null,total_ms:null,buffering:null,event_count:0,delta_count:0,body_bytes:0,terminal:false,error:null};
  let terminal;
  try {
    const response = await fetchImpl(url, {method:'POST', headers:{authorization:`Bearer ${key}`,'content-type':'application/json'},
      body:JSON.stringify(body), redirect:'manual',signal:AbortSignal.timeout(timeoutMs)});
    result.headers_ms=performance.now()-started; result.http_status=response.status;
    result.buffering=response.headers.get('x-nanocodex-inference-buffering');
    result.headers=Object.fromEntries(['cf-ray','cf-placement','x-nanocodex-provider','x-nanocodex-model','x-nanocodex-thinking'].map(k=>[k,response.headers.get(k)]));
    const sse=(response.headers.get('content-type')??'').includes('text/event-stream');
    result.transport=sse?'sse':'json';
    const decoder=new TextDecoder(); let pending='';
    function event(frame) {
      const data=frame.split('\n').filter(l=>l.startsWith('data:')).map(l=>l.slice(5).replace(/^ /,'')).join('\n');
      if (!data || data==='[DONE]') return;
      let e; try {e=JSON.parse(data);} catch {result.error??='invalid_sse_json'; return;}
      result.event_count++;
      if (['response.output_text.delta','response.function_call_arguments.delta','response.custom_tool_call_input.delta'].includes(e.type) && typeof e.delta==='string' && /\S/.test(e.delta)) {
        result.delta_count++; if(result.first_meaningful_ms===null) {result.first_meaningful_ms=performance.now()-started;result.first_meaningful_kind=e.type;}
      }
      if(['response.completed','response.incomplete','response.failed'].includes(e.type)) {terminal=e.response;result.terminal=true;result.completed_ms=performance.now()-started;}
      if(e.type==='error'||e.type==='response.failed') result.error??='provider_error';
    }
    for await (const chunk of response.body) {
      result.first_body_ms??=performance.now()-started; result.body_bytes+=chunk.length;
      if(result.body_bytes>4*1024*1024) throw new Error('body_limit');
      pending+=decoder.decode(chunk,{stream:true});
      if(sse) {pending=pending.replace(/\r\n/g,'\n');let boundary;while((boundary=pending.indexOf('\n\n'))>=0) {event(pending.slice(0,boundary));pending=pending.slice(boundary+2);}}
    }
    pending+=decoder.decode();
    if(sse) {if(pending.trim()) event(pending);}
    else {
      try {terminal=JSON.parse(pending);} catch {result.error??='invalid_json';}
      result.terminal=['completed','incomplete','failed'].includes(terminal?.status);
      if(result.terminal) result.completed_ms=performance.now()-started;
      if(outputPresent(terminal)) {result.first_meaningful_ms=performance.now()-started;result.first_meaningful_kind='buffered_json_output';}
    }
    Object.assign(result,projection(terminal));
    if(!response.ok) result.error??=`http_${response.status}`;
    if(!result.terminal && response.ok) result.error??='missing_terminal';
  } catch(error) {result.exception_name=error?.name??null;result.exception_hint=['redirect','AbortSignal','timeout','fetch','URL','signal','illegal'].filter(x=>String(error?.message).includes(x)).join(',');result.error=['TimeoutError','AbortError'].includes(error?.name)?'timeout_outcome_unknown':'transport_or_protocol_error';}
  result.total_ms=performance.now()-started;
  return result;
}
export function quantile(values,p) {const s=values.filter(Number.isFinite).sort((a,b)=>a-b);return s.length?s[Math.max(0,Math.ceil(p*s.length)-1)]:null;}
export function summarize(rows) {
  const groups=new Map();
  for(const r of rows) {const key=JSON.stringify([r.stage,r.runner,r.arm,r.model_requested,r.family,r.stream]);if(!groups.has(key))groups.set(key,[]);groups.get(key).push(r);}
  return [...groups].map(([group,all])=>{const ok=all.filter(r=>!r.error&&r.status==='completed'&&r.first_meaningful_ms!==null);return {group:JSON.parse(group),attempts:all.length,completed:ok.length,errors:all.filter(r=>r.error).length,incomplete:all.filter(r=>r.status==='incomplete').length,
    meaningful_p50_ms:quantile(ok.map(r=>r.first_meaningful_ms),.5),meaningful_p95_ms:quantile(ok.map(r=>r.first_meaningful_ms),.95),total_p50_ms:quantile(ok.map(r=>r.total_ms),.5),total_p95_ms:quantile(ok.map(r=>r.total_ms),.95),p95_caution:ok.length<20?'fewer_than_20_samples':null};});
}
