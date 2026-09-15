"""Reduce a local Wrangler JSON tail to benchmark-only, credential-free phase evidence."""
import json,re,sys,statistics
from pathlib import Path
folder=Path(sys.argv[1]); source=Path(sys.argv[2])
measurements=json.loads((folder/'measurements.json').read_text())['records']
known={r['session_id']:r for r in measurements if r.get('session_id') and r['path']=='nanocodex_cloudflare'}
objects=[]; decoder=json.JSONDecoder(); contents=source.read_text()
for match in re.finditer(r'(?m)^\{',contents):
 try: record,_=decoder.raw_decode(contents,match.start());objects.append(record)
 except ValueError: pass
mapping={}; phases=[]
for record in objects:
 for log in record.get('logs',[]):
  for message in log.get('message',[]):
   if not isinstance(message,dict) or message.get('session_id') not in known:continue
   sid=message['session_id'];mapping[record.get('durableObjectId')]=sid
   if message.get('type')=='managed.capacity' and message.get('reason')=='agent_constructed':
    phases.append({'session_id':sid,'label':known[sid]['label'],'timestamp':record.get('eventTimestamp'),'script_version':record.get('scriptVersion',{}).get('id'),**{k:v for k,v in message.items() if k.endswith('_ms')}})
mapping.pop(None,None)
invocations=[]; diagnostics=[]
for record in objects:
 sid=mapping.get(record.get('durableObjectId'))
 if not sid:continue
 for log in record.get('logs',[]):
  for message in log.get('message',[]):
   if isinstance(message,dict) and message.get('type') in ['managed.event_stream_failed','managed.event_stream_persist_failed','managed.agent_shutdown_failed']:
    diagnostics.append({'session_id':sid,'timestamp':log.get('timestamp'),'type':message['type'],'error_kind':message.get('error_kind')})
 for exception in record.get('exceptions',[]):
  diagnostics.append({'session_id':sid,'timestamp':exception.get('timestamp'),'exception_name':exception.get('name'),'inactive_durable_object':'Durable Object instance is no longer active' in exception.get('message','')})
 # Wall spans can overlap and include streaming/waitUntil. Do not infer billed GB-s.
 invocations.append({'session_id':sid,'label':known[sid]['label'],'script_version':record.get('scriptVersion',{}).get('id'),'timestamp':record.get('eventTimestamp'),'cpu_ms':record.get('cpuTime'),'wall_ms':record.get('wallTime'),'outcome':record.get('outcome'),'truncated':record.get('truncated'),'method':record.get('event',{}).get('request',{}).get('method'),'status':record.get('event',{}).get('response',{}).get('status')})
result={'source':'Wrangler live tail; only benchmark session Durable Objects','limitations':['Not billing records or complete distributed traces','Invocation wall spans may overlap','Worker performance clock may not advance during CPU-only work','Front/broker invocations are not attributed here'],'construction_phases':phases,'invocations':invocations,'diagnostics':diagnostics}
(folder/'server-profile.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps({'construction_samples':len(phases),'invocations':len(invocations)}))
