"""Generate isolated configs from production. Run with uv --with json5."""
from pathlib import Path
import json,json5
folder=Path(__file__).resolve().parent
managed=json5.loads((folder.parent/'wrangler.jsonc').read_text())
for k in ('$schema','env','containers','browser','ai_search'): managed.pop(k,None)
managed.update(name='nanocodex-perf-managed-0911',main='worker.ts',workers_dev=True)
managed['alias']={'node-rsa':'../../nanocodex/tools/browser/unsupportedNodeRsa.mjs'}
managed['services']=[{'binding':'NANOCODEX','service':'nanocodex-perf-egress-0911'}]
for b in managed['r2_buckets']: b['bucket_name']='nanocodex-perf-0911'
managed['durable_objects']['bindings']=[b for b in managed['durable_objects']['bindings'] if b['name']!='NANOCODEX_SANDBOXES']
managed['migrations']=[{'tag':'v1','new_sqlite_classes':[b['class_name'] for b in managed['durable_objects']['bindings']]}]
managed['durable_objects']['bindings'].append({'name':'NANOCODEX_SANDBOXES','class_name':'Sandbox'})
managed['migrations'].append({'tag':'v2','new_sqlite_classes':['Sandbox']})
managed['containers']=[{'class_name':'Sandbox','image':'registry.cloudflare.com/16ce0442a940f01beefdb15a196a43ea/nanocodex-durable-agent-sandbox@sha256:41927b3c3f7dc3fd9a868cba14bf4dac460d9ebdf812d5c99a1926ad03edfa34','instance_type':'standard-1','max_instances':2}]
managed['browser']={'binding':'BROWSER'}
managed['vars']={'AGENT_IDLE_TIMEOUT_MS':'30000','MANAGED_AGENT_DIRECT_CREDENTIALS':'true','BENCHMARK_USER':'81382dad-0a01-4eab-8bbf-091100000001'}
broker=json5.loads((folder.parent.parent/'egress/wrangler.broker.jsonc').read_text())
for k in ('$schema','env'): broker.pop(k,None)
broker.update(name='nanocodex-perf-egress-0911',main='../../egress/src/egress.ts',alias=managed['alias'])
broker['services']=[{'binding':'MANAGED_AGENT_OWNERSHIP','service':managed['name'],'entrypoint':'ManagedAgentOwnership'}]
broker['durable_objects']['bindings']=[b for b in broker['durable_objects']['bindings'] if 'script_name' not in b]
broker['migrations']=[{'tag':'v1','new_sqlite_classes':[b['class_name'] for b in broker['durable_objects']['bindings']]}]
bootstrap={'name':managed['name'],'main':'bootstrap.ts','compatibility_date':managed['compatibility_date'],'workers_dev':True}
front={'name':'nanocodex-perf-front-0911','main':'front.ts','compatibility_date':managed['compatibility_date'],'workers_dev':True,'services':[{'binding':'NANOCODEX_BACKEND','service':managed['name']}],'observability':{'enabled':True}}
for name,data in [('managed',managed),('egress',broker),('bootstrap',bootstrap),('front',front)]: (folder/f'{name}.json').write_text(json.dumps(data,indent=2)+'\n')
