import json,collections,statistics,os
from pathlib import Path
root=Path(__file__).resolve().parent
out=root.parents[1]/'docs/performance/2026-09-16-warmup-experiments'
out.mkdir(parents=True,exist_ok=True)
ev=[]
for name in ['managed','egress','account']:
 for line in (root/f'{name}-tail.jsonl').read_text().splitlines():
  try: ev.append(json.loads(line))
  except json.JSONDecodeError: pass
rows=[];selected=[];receipts={}
for cohort in os.environ.get('EXPERIMENT_COHORTS','hosted,hosted-confirm').split(','):
 p=root/f'{cohort}.json'
 if not p.exists(): continue
 d=json.loads(p.read_text());receipts[cohort]={k:d.get(k) for k in ['started_at','finished_at','versions_start','versions_end']}
 for j in d['journeys']:
  if 'run' not in j: continue
  r=next(r for r in d['resources'] if r['agent']==j['agent'])
  dos={e.get('durableObjectId') for e in ev if any(l.get('message',{}).get('trace_id')==j['id'] or l.get('message',{}).get('turn_id')==j['id'] for l in e.get('logs',[]))}-{None}
  reqs={q.get('request_id') for q in d['requests'] if j['agent'] in q.get('path','')}|{r['created_request_id']}
  matched=[e for e in ev if e.get('durableObjectId') in dos or any(l.get('message',{}).get('request_id') in reqs-{None} for l in e.get('logs',[]))]
  logs=[l for e in matched for l in e.get('logs',[])]
  stages=[l['message'] for l in logs if l['message'].get('trace_id')==j['id']]
  admission=next((s for s in stages if s.get('stage')=='turn.admission'),{})
  run=j['run'];model=j['model_calls'][0]
  row={'cohort':cohort,'arm':j['task'],'sample':j['sample'],'agent':j['agent'],'turn':j['id'],'durable_object_ids':sorted(dos),'ttft_ms':j['first_text_ms'],'create_to_text_ms':r['create_to_first_text_ms'],'lead_to_text_ms':r['intentional_idle_ms']+j['first_text_ms'],'completion_ms':j['completion_ms'],'connection_ms':run['connection_duration_ns']/1e6,'model_first_output_ms':model['time_to_first_output_ns']/1e6,'model_first_event_ms':model['time_to_first_event_ns']/1e6,'outside_model_ms':j['first_text_ms']-model['time_to_first_output_ns']/1e6,'admission_ms':admission.get('duration_ms'),'admission_worker_versions':sorted({e.get('scriptVersion',{}).get('id') for e in matched if any(l['message'].get('trace_id')==j['id'] for l in e['logs'])}-{None}),'admission_reads':admission.get('reads'),'preparation':r.get('prepare'),'intentional_idle_ms':r['intentional_idle_ms'],'cache_tokens':run['usage']['cached_input_tokens'],'input_tokens':run['usage']['input_tokens'],'warmup_ms':run['warmup_duration_ns']/1e6,'correct':j['answer'].strip()=='42','tools':run['tool_calls'],'deleted':r.get('deleted'),'loadavg':j['loadavg'],'stages':stages,'idle_shutdown_logs':[l for l in logs if 'idle' in l['message'].get('type','')],'trace_logs':logs}
  rows.append(row);selected.extend(matched)
metrics=['ttft_ms','lead_to_text_ms','create_to_text_ms','connection_ms','admission_ms','model_first_output_ms','outside_model_ms']
groups={}
for arm in dict.fromkeys(r['arm'] for r in rows):
 rr=[r for r in rows if r['arm']==arm];group={'n':len(rr)}
 for m in metrics:
  v=[r[m] for r in rr if r[m] is not None];group[m]={'median':statistics.median(v),'min':min(v),'max':max(v)} if v else None
 groups[arm]=group
payload={'receipts':receipts,'groups':groups,'samples':rows,'trace_caveat':'Exact turn/request IDs and same Durable Object invocation correlation. Nested spans overlap. Named read counters are not a complete SQL statement audit.'}
(out/(os.environ.get('EXPERIMENT_PREFIX','hosted')+'-measurements.json')).write_text(json.dumps(payload,indent=2)+'\n')
unique={json.dumps(e,sort_keys=True):e for e in selected}
(out/(os.environ.get('EXPERIMENT_PREFIX','hosted')+'-traces.json')).write_text(json.dumps(list(unique.values()),indent=2)+'\n')
print(json.dumps(groups,indent=2))
