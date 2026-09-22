import json, statistics
from pathlib import Path
root=Path(__file__).resolve().parent
out=root.parents[1]/'docs/performance/2026-09-16-nanocodex2-cold-master'
out.mkdir(parents=True,exist_ok=True)
data=json.loads((root/'cold.json').read_text())
events=[]
for path in root.glob('*-tail.jsonl'):
 for line in path.read_text().splitlines():
  events.append(json.loads(line))
rows=[]; selected=[]
for row in data['rows']:
 agent=row.get('session_id'); turn=row.get('turn_id')
 dos={e.get('durableObjectId') for e in events if turn and any(l['message'].get('trace_id')==turn or l['message'].get('turn_id')==turn for l in e.get('logs',[]))}-{None}
 matched=[e for e in events if e.get('durableObjectId') in dos or any(agent and (agent in l['message'].get('path','') or any(l['message'].get(k)==agent for k in ['agent_id','thread_id','session_id'])) for l in e.get('logs',[]))]
 logs=[l['message'] for e in matched for l in e.get('logs',[])]
 stages=[m for m in logs if m.get('trace_id')==turn]
 admission=next((m for m in stages if m.get('stage')=='turn.admission'),{})
 run=next((e['event']['payload'] for e in row.get('events',[]) if e['event']['type']=='run.completed'),{})
 model=row.get('model',{})
 item={k:row.get(k) for k in ['rep','session_id','turn_id','started_at','first_text_ms','agent_ready_ms','run_started_ms','model_call_receipt_ms','completion_ms','process_ms','text','error','settings','loadavg','server_admission_ms','server_model_to_text_ms','server_accepted_to_text_ms']}
 item.update({'model_first_output_ms':model.get('time_to_first_output_ns',0)/1e6,'connection_ms':run.get('connection_duration_ns',0)/1e6,'response_retries':run.get('response_retries'),'tool_calls':run.get('tool_calls'),'usage':run.get('usage'),'admission_ms':admission.get('duration_ms'),'admission_reads':admission.get('reads'),'stages':stages,'related_stages':[m for m in logs if m.get('stage') and m.get('trace_id')!=turn],'prepare_requests':[m for m in logs if m.get('type')=='managed.request' and m.get('path','').endswith('/prepare')],'requests':[m for m in logs if m.get('type') in ['managed.request','managed.auth']],'durable_object_ids':sorted(dos),'admission_versions':sorted({e.get('scriptVersion',{}).get('id') for e in matched if any(l['message'].get('trace_id')==turn for l in e['logs'])}-{None}),'cleanup_status':next((r.get('cleanup_status') for r in data['resources'] if r['id']==agent),None)})
 if item['first_text_ms'] is not None and item['admission_ms'] is not None:
  item['residual_ms']=item['first_text_ms']-item['agent_ready_ms']-item['admission_ms']-item['model_first_output_ms']
  item['model_excluding_connection_ms']=item['model_first_output_ms']-item['connection_ms']
  if item['server_model_to_text_ms'] is not None:
   item['text_residual_ms']=item['first_text_ms']-item['agent_ready_ms']-item['admission_ms']-item['server_model_to_text_ms']
   item['server_text_minus_model_first_output_ms']=item['server_model_to_text_ms']-item['model_first_output_ms']
 candidates=[e for e in events if any(l['message'].get('path')=='/v1/agents/live' for l in e.get('logs',[])) and __import__('datetime').datetime.fromisoformat(row['started_at'].replace('Z','+00:00')).timestamp()*1000 <= e.get('eventTimestamp',0) <= __import__('datetime').datetime.fromisoformat(row['started_at'].replace('Z','+00:00')).timestamp()*1000 + (row.get('agent_ready_ms') or 0)]
 item['create_live_time_window_candidates']=[l['message'] for e in candidates for l in e['logs'] if l['message'].get('type')=='managed.request']
 item['create_live_correlation_note']='Time-window candidates only: the root create-live request has no request or agent ID in these logs; cross-machine wall timestamps are not used to calculate durations.'
 selected.extend(candidates)
 rows.append(item); selected.extend(matched)
metrics=['first_text_ms','agent_ready_ms','admission_ms','connection_ms','model_first_output_ms','model_excluding_connection_ms','residual_ms','text_residual_ms','server_model_to_text_ms','server_text_minus_model_first_output_ms','completion_ms']
summary={}
for metric in metrics:
 values=[r[metric] for r in rows if r.get(metric) is not None]
 summary[metric]={'n':len(values),'median':statistics.median(values),'min':min(values),'max':max(values)} if values else None
manifest={k:v for k,v in data.items() if k!='rows'}
(out/'measurements.json').write_text(json.dumps({'summary':summary,'samples':rows},indent=2)+'\n')
(out/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
(out/'traces.json').write_text(json.dumps(list({json.dumps(e,sort_keys=True):e for e in selected}.values()),indent=2)+'\n')
print(json.dumps(summary,indent=2))
print('Resources:',[(r['id'],r.get('cleanup_status')) for r in data['resources']])
