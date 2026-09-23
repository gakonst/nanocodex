"""Allowlisted tail collector. Raw requests, headers, bodies and stderr never touch disk."""
import json, subprocess, threading, pathlib, signal, os, re, sys, time, math
root = pathlib.Path(sys.argv[1]); cwd = pathlib.Path.cwd(); processes=[]; threads=[]; stopping=False
names=['nanocodex-durable-agent','nanocodex','nanocodex-egress']
keep={'type','request_id','method','path','status','deployment_sha','was_running','recovered','transport','relay_transport','socket_reused','trace_id','stage','object_id','agent_id','session_id','turn_id','resolve_id','credential_broker_resolve_id','started_at','event','action','count','call_id','call_index','cache_hit','fact_count','message_type','operation_kind','attempt_count','outcome','failure_phase','replay_mode','next_attempt','max_attempts','connection_generation','model_call_index','status_code','opens_new_socket','server_requested_delay','reason','read_count','read_ms','statement_count','total_ms','agent_subject','rule','credential_kind','model_source','operation','model','queue_scope','egress_request_id','relay_id','operations_ahead','waiting_at_start','waiting_at_finish','active_operation_at_enqueue','cause','request_colo','cache_state','ready','relay_region','dns_observed','last_phase','recover'}
def sanitize(m):
    if not isinstance(m,dict): return None
    if m.get('type') == 'managed.performance' and m.get('stage') in {'transport.socket_queue','transport.provider_timing'}:
        safe = {k:m[k] for k in ['type','stage']}
        if isinstance(m.get('session_id'),str) and re.fullmatch(r'[A-Za-z0-9_-]{1,128}',m['session_id']): safe['session_id']=m['session_id']
        if isinstance(m.get('response_id'),str) and re.fullmatch(r'resp_[A-Za-z0-9_-]{1,128}',m['response_id']): safe['response_id']=m['response_id']
        counts = {'message_count','delivered_message_count','buffered_message_count','discarded_message_count'}
        timings = {'queue_residence_total_ms','queue_residence_max_ms'} if m['stage'] == 'transport.socket_queue' else {'pre_inference_ms','engine_queue_max_ms','engine_service_ttft_total_ms'}
        for k in timings | (counts if m['stage'] == 'transport.socket_queue' else set()):
            value = m.get(k)
            if type(value) in (int,float) and math.isfinite(value) and value >= 0:
                if (k not in counts or isinstance(value,int)) and (k in counts or value <= 86_400_000): safe[k]=value
        return safe
    if not str(m.get('type','')).startswith(('managed.','egress.','account.','model.','responses.relay','voice.relay')): return None
    safe={k:v for k,v in m.items() if ((k.endswith('_ms') and isinstance(v,(int,float))) or (k in keep and isinstance(v,(str,int,float,bool,type(None))))) }
    if 'path' in safe: safe['path']=str(safe['path']).split('?')[0] if str(safe['path']).startswith('/v1/agents') or str(safe['path']) in {'/v1/responses','/backend-api/codex/responses','/provider-api'} else '[other route]'
    if isinstance(m.get('reads'),dict):
        safe['reads']={k:({field:value for field,value in v.items() if field in {'count','duration_ms'} and isinstance(value,(int,float))} if isinstance(v,dict) else v) for k,v in m['reads'].items() if isinstance(v,(dict,int,float))}
    if isinstance(m.get('activation_phases'),dict):
        safe['activation_phases']={k:v for k,v in m['activation_phases'].items() if k in {'storage_load_ms','vault_open_ms','restore_ms','migration_ms','reseal_ms','alarm_ms'} and isinstance(v,(int,float))}
    for k in ['error_kind','error_code','code']:
        if isinstance(m.get(k),str) and re.fullmatch(r'[A-Za-z][A-Za-z0-9_.-]{0,79}',m[k]): safe[k]=m[k]
    return safe

def collect(name):
    p=subprocess.Popen([os.environ.get('NC_NODE','node'),os.environ['NC_WRANGLER'],'tail',name,'--config',str(cwd/'js/managed/wrangler.jsonc'),'--format','json'],cwd=cwd,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,bufsize=1,start_new_session=True)
    processes.append(p)
    # Count stderr; do not retain it because tools can include authentication metadata.
    def stderr():
        count=0
        for line in p.stderr: count+=1
        (root/(name+'-tail-status.json')).write_text(json.dumps({'exit_code':p.wait(),'stderr_lines':count}))
    threading.Thread(target=stderr,daemon=True).start()
    decoder=json.JSONDecoder(); buffer=''
    with open(root/(name+'-tail.jsonl'),'a') as out:
        for line in p.stdout:
            buffer+=line
            while True:
                buffer=buffer.lstrip()
                if not buffer: break
                if not buffer.startswith('{'):
                    start=buffer.find('{'); buffer=buffer[start:] if start>=0 else ''
                    if not buffer: break
                try: event,offset=decoder.raw_decode(buffer)
                except ValueError: break
                buffer=buffer[offset:]; logs=[]
                for log in event.get('logs',[]):
                    for message in log.get('message',[]):
                        if isinstance(message,str):
                            try: message=json.loads(message)
                            except ValueError: continue
                        safe=sanitize(message)
                        if safe: logs.append({'timestamp':log.get('timestamp'),'message':safe})
                if logs or event.get("durableObjectId"):
                    result={k:event.get(k) for k in ['wallTime','cpuTime','scriptName','scriptVersion','eventTimestamp','outcome','durableObjectId']}
                    result['logs']=logs; out.write(json.dumps(result)+'\n');out.flush()

def stop(*_):
    global stopping
    stopping=True
    for p in processes:
        try: os.killpg(p.pid,signal.SIGTERM)
        except ProcessLookupError: pass
signal.signal(signal.SIGTERM,stop);signal.signal(signal.SIGINT,stop)
for name in names:
    t=threading.Thread(target=collect,args=(name,),daemon=True);t.start();threads.append(t)
while any(t.is_alive() for t in threads) and not stopping: time.sleep(.2)
for t in threads:t.join(2)
