import json, subprocess, threading, pathlib, signal, os, re
root = pathlib.Path(__file__).parent
cwd = '/Users/georgios/github/gakonst/nanocodex-master'
pnpm = '/Users/georgios/Library/pnpm/.tools/pnpm/11.25.0/bin/pnpm'
processes=[]
def collect(name, config):
    err=open(root/(name+'-tail.stderr'),'w')
    p=subprocess.Popen([pnpm,'exec','wrangler','tail','--config',cwd+'/'+config,'--format','json'],cwd=cwd+'/js/managed',stdout=subprocess.PIPE,stderr=err,text=True,bufsize=1,start_new_session=True)
    processes.append(p)
    decoder=json.JSONDecoder(); buffer=''
    with open(root/(name+'-tail.jsonl'),'a') as out:
        for line in p.stdout:
            buffer+=line
            while True:
                buffer=buffer.lstrip()
                if not buffer: break
                if not buffer.startswith('{'):
                    start=buffer.find('{')
                    buffer=buffer[start:] if start>=0 else ''
                    if not buffer:break
                try: event,offset=decoder.raw_decode(buffer)
                except ValueError: break
                buffer=buffer[offset:]
                # Save only explicitly selected timings/IDs; never request headers or bodies.
                logs=[]
                for log in event.get('logs',[]):
                    for m in log.get('message',[]):
                        if isinstance(m,str):
                            if 'truncat' in m.lower():
                                logs.append({'timestamp':log.get('timestamp'),'message':{'type':'worker.log_truncated'}})
                            try:m=json.loads(m)
                            except ValueError:continue
                        if isinstance(m,dict) and m.get('type')=='managed.sql_batch':
                            for statement in m.get('statements',[]):
                                safe={k:statement[k] for k in ['statement_id','operation','tables','exec_ms','consume_ms','rows_read','rows_written','success','count'] if k in statement}
                                safe.update({k:m[k] for k in ['object_id','trace_id','stage'] if k in m})
                                safe['type']='managed.sql'
                                logs.append({'timestamp':log.get('timestamp'),'message':safe})
                            continue
                        if isinstance(m,dict) and (str(m.get('type','')).startswith(('managed.','egress.','voice.','model.','sandbox.'))):
                            safe={k:v for k,v in m.items() if k.endswith('_ms') or k in ['type','rule','request_id','voice_session_id','mode','method','path','status','deployment_sha','was_running','recovered','at','transport','relay_transport','socket_reused','trace_id','stage','reads','tables','statement_id','operation','object_id','rows_read','rows_written','success','agent_id','thread_id','session_id','turn_id','resolve_id','credential_broker_resolve_id','subject','agent_subject','started_at','event','action','count','call_id','call_index','cache_hit','fact_count','message_type','operation_kind','attempt_count','outcome','failure_phase','replay_mode','next_attempt','max_attempts','connection_generation','model_call_index','status_code','opens_new_socket','server_requested_delay','reason']}
                            for key in ['error_kind', 'error_code', 'code']:
                                if isinstance(m.get(key),str) and re.fullmatch(r'[A-Za-z][A-Za-z0-9_.-]{0,79}',m[key]):safe[key]=m[key]
                            logs.append({'timestamp':log.get('timestamp'),'message':safe})
                for failure in event.get("exceptions", []):
                    logs.append({"timestamp": failure.get("timestamp"), "message": {"type": "worker.exception", "name": failure.get("name")}})
                if logs:
                    result={k:event.get(k) for k in ['wallTime','cpuTime','scriptName','scriptVersion','eventTimestamp','outcome','durableObjectId']}
                    result['logs']=logs
                    out.write(json.dumps(result)+'\n');out.flush()
def stop(*_):
    for p in processes:
        try:os.killpg(p.pid,signal.SIGTERM)
        except ProcessLookupError:pass
signal.signal(signal.SIGTERM,stop);signal.signal(signal.SIGINT,stop)
threads=[threading.Thread(target=collect,args=(name,config)) for name,config in [('managed','js/managed/wrangler.jsonc'),('egress','js/egress/wrangler.broker.jsonc'),('account','js/account/dist/nanocodex/wrangler.json')]]
for t in threads:t.start()
for t in threads:t.join()
