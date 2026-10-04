const syntheticJob = 'async-canary-original-job';
function requestBody(token) {
  return {
    model: 'gpt-6.1-sol', instructions: 'Read the supplied tool transcript. Return only the exact result token from the completed result for job async-canary-original-job. If the job is still pending, return only PENDING. Do not call tools.',
    stream: true, store: false, parallel_tool_calls: false, reasoning: { effort: 'low', context: 'all_turns' },
    tools: [{type:'namespace',name:'functions',description:'Code Mode tools',tools:[{ type: 'custom', name: 'exec', description: 'Run JavaScript. A pending result names a job. Host completion delivery appears as a fresh exec call/output pair and carries the original job_id.', format: {type: 'text'} }]}],
    input: [
      { role: 'user', content: [{type: 'input_text', text: 'Return the result token for the original async job once its completion is present.'}] },
      { type: 'custom_tool_call', call_id: 'call_async_original', name: 'exec', namespace:'functions', input: 'await tools.runCanary();' },
      { type: 'custom_tool_call_output', call_id: 'call_async_original', output: JSON.stringify({status:'pending',job_id:syntheticJob}) },
      { type: 'custom_tool_call', id:'ctc_async_delivery_001', status:'completed', call_id: 'async_delivery_001', name: 'exec', namespace:'functions', input: '// Host delivery of a previously accepted Code Mode job.\n// '+JSON.stringify({kind:'async_completion',job_id:syntheticJob,original_call_id:'call_async_original',delivery_id:'delivery_001'}) },
      { type: 'custom_tool_call_output', id:'ctco_async_delivery_001', call_id: 'async_delivery_001', status:'completed', output: JSON.stringify({status:'completed',job_id:syntheticJob,result:token}) }
    ]
  };
}
function projection(events, token) {
  const terminal = events.find(e => ['response.completed','response.failed','response.incomplete'].includes(e.type));
  const text = terminal?.response?.output?.filter(i=>i.type==='message').flatMap(i=>i.content??[]).filter(c=>c.type==='output_text').map(c=>c.text??'').join('') || events.filter(e=>e.type==='response.output_text.delta').map(e=>e.delta??'').join('');
  const codes = events.filter(e=>e.type==='error'||e.type==='response.failed').map(e=>({type:e.type,code:e.error?.code??e.response?.error?.code??null,param:e.error?.param??null,message:(e.error?.message??e.response?.error?.message??'').slice(0,400)}));
  return {event_types:[...new Set(events.map(e=>e.type))],terminal:terminal?.type??null,response_status:terminal?.response?.status??null,model:terminal?.response?.model??null,uptake:text.trim()===token,output_text:text.slice(0,200),expected_token:token,error_codes:codes};
}
export default {async fetch(request,env) {
  const path = new URL(request.url).pathname;
  if (request.method!=='POST'||!['/http','/ws','/ws-incremental'].includes(path)) return new Response(null,{status:404});
  const account = env.USERS.getByName(env.OWNER);
  const ar = await account.fetch('https://user.internal/authorization');
  if(!ar.ok)return Response.json({stage:'verify_owner',status:ar.status},{status:502});
  const auth=await ar.json();
  if(auth.userId!==env.OWNER||auth.grant?.teamId!==env.TEAM)return Response.json({stage:'verify_owner',status:'mismatch'},{status:403});
  const mr=await env.NANOCODEX.fetch(`https://broker.internal/users/${env.OWNER}/credentials`);
  if(!mr.ok)return Response.json({stage:'credential_metadata',status:mr.status},{status:502});
  const metadata=await mr.json();
  if(metadata.active!=='chatgpt'||!metadata.chatgpt?.connected)return Response.json({stage:'chatgpt_subscription_unavailable'},{status:409});
  const digest=await crypto.subtle.digest('SHA-256',new TextEncoder().encode(`browser-model-v1:${env.OWNER}`));
  const subject=btoa(String.fromCharCode(...new Uint8Array(digest))).replaceAll('+','-').replaceAll('/','_').replace(/=+$/,'');
  const token='ASYNC_CANARY_'+crypto.randomUUID().replaceAll('-','').slice(0,16);
  const body=requestBody(token);
  const headers={'authorization':'Bearer NANOCODEX_PROVIDER_CREDENTIAL','x-nanocodex-subject':subject,'x-openai-internal-codex-responses-lite':'true','session-id':crypto.randomUUID(),'thread-id':crypto.randomUUID()};
  const events=[];
  if(path==='/http') {
    const r=await env.NANOCODEX.fetch('https://nanocodex.internal/v1/responses',{method:'POST',headers:{...headers,'content-type':'application/json'},body:JSON.stringify(body),signal:AbortSignal.timeout(90000)});
    if(!r.ok){const error=await r.json().catch(()=>null);return Response.json({transport:'http',status:r.status,stage:'provider',error_code:error?.error?.code??null,param:error?.error?.param??null,message:(error?.error?.message??'').slice(0,400)});}
    const raw=await r.text();
    for(const line of raw.split('\n'))if(line.startsWith('data: ')){try{events.push(JSON.parse(line.slice(6)))}catch{}}
    return Response.json({transport:'http',status:r.status,subscription:true,projection_shape:'native_exact',subject_source:'existing_browser_model_binding',...projection(events,token)});
  }
  const r=await env.NANOCODEX.fetch('https://nanocodex.internal/v1/responses',{headers:{...headers,upgrade:'websocket','openai-beta':'responses_websockets=2026-02-06'},signal:AbortSignal.timeout(90000)});
  if(r.status!==101||!r.webSocket){const error=await r.json().catch(()=>null);return Response.json({transport:'ws',status:r.status,stage:'upgrade',error_code:error?.error?.code??null,param:error?.error?.param??null,message:(error?.error?.message??'').slice(0,400)});}
  const ws=r.webSocket;ws.accept();
  async function exchange(frame) {
    const received=[];
    const finish=await new Promise(resolve=>{
      function done(reason){clearTimeout(timer);ws.removeEventListener('message',onMessage);ws.removeEventListener('error',onError);ws.removeEventListener('close',onClose);resolve(reason)}
      function onMessage(e){try{const data=JSON.parse(e.data);received.push(data);if(['response.completed','response.failed','response.incomplete','error'].includes(data.type))done('terminal')}catch{}}
      function onError(){done('socket_error')}
      function onClose(){done('closed')}
      const timer=setTimeout(()=>done('timeout'),90000);
      ws.addEventListener('message',onMessage);ws.addEventListener('error',onError);ws.addEventListener('close',onClose);
      ws.send(JSON.stringify({type:'response.create',...frame}));
    });
    return {received,finish};
  }
  try {
    let first;
    let frame=body;
    if(path==='/ws-incremental') {
      const initial=await exchange({...body,tool_choice:'none',input:body.input.slice(0,3)});
      first=projection(initial.received,'PENDING');
      const previous=initial.received.find(e=>e.type==='response.completed')?.response?.id;
      if(!previous||!first.uptake)return Response.json({transport:'ws-incremental',status:r.status,stage:'initial_pending',first,uptake:false});
      frame={...body,previous_response_id:previous,input:body.input.slice(3)};
    }
    const result=await exchange(frame);
    return Response.json({transport:path.slice(1),status:r.status,subscription:true,subject_source:'existing_browser_model_binding',projection_shape:'native_exact',incremental_previous_response_id:path==='/ws-incremental',...(first?{first}:{}),finish:result.finish,...projection(result.received,token)});
  } finally {try{ws.close(1000,'canary_complete')}catch{}}
}};
