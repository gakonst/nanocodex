"""Read isolated Cloudflare namespace analytics. Requires an authorized token in the environment.

Usage: script OUTPUT_DIR NAMESPACES_JSON START_ISO END_ISO
Namespaces must all belong to this experiment's nanocodex-perf-* services.
"""
import json,os,sys,datetime,urllib.request
from pathlib import Path
folder=Path(sys.argv[1]);namespaces=json.loads(Path(sys.argv[2]).read_text())
start,end=sys.argv[3:5]
assert namespaces and all(n['script'].startswith('nanocodex-perf-') for n in namespaces)
account=os.environ['CLOUDFLARE_ACCOUNT_ID'];token=os.environ['CLOUDFLARE_API_TOKEN']
time_filter='datetime_geq:'+json.dumps(start)+',datetime_lt:'+json.dumps(end)
session_namespace=next(n['id']for n in namespaces if n['class']=='DurableAgentSession')
workers=sorted({n['script']for n in namespaces}|{'nanocodex-perf-front-0911'})
filters='datetime_geq:'+json.dumps(start)+',datetime_lt:'+json.dumps(end)+',namespaceId_in:'+json.dumps([n['id']for n in namespaces])
query='{viewer{accounts(filter:{accountTag:'+json.dumps(account)+'}){'+'''
periodic:durableObjectsPeriodicGroups(filter:{FILTER},limit:1000){
 dimensions{namespaceId coloCode}
 sum{duration activeTime cpuTime rowsRead rowsWritten subrequests exceededCpuErrors exceededMemoryErrors fatalInternalErrors}
 max{memoryUsageBytes}
}
invocations:durableObjectsInvocationsAdaptiveGroups(filter:{FILTER},limit:1000){
 dimensions{namespaceId coloCode type status}
 sum{requests errors responseBodySize}
}
storage:durableObjectsSqlStorageGroups(filter:{FILTER},limit:1000){dimensions{namespaceId}max{storedBytes}}
'''.replace('FILTER',filters)+('sessions:durableObjectsPeriodicGroups(filter:{'+time_filter+',namespaceId:'+json.dumps(session_namespace)+'},limit:1000){dimensions{name objectId coloCode}sum{duration cpuTime rowsRead rowsWritten}}')+('workers:workersInvocationsAdaptive(filter:{'+time_filter+',scriptName_in:'+json.dumps(workers)+'},limit:1000){dimensions{scriptName status}sum{requests cpuTimeUs errors clientDisconnects}}')+('r2:r2OperationsAdaptiveGroups(filter:{'+time_filter+',bucketName:"nanocodex-perf-0911"},limit:1000){dimensions{actionType actionStatus storageClass}sum{requests}}')+'}}}'
request=urllib.request.Request('https://api.cloudflare.com/client/v4/graphql',data=json.dumps({'query':query}).encode(),headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
with urllib.request.urlopen(request,timeout=30) as response:result=json.load(response)
if result.get('errors'):raise RuntimeError(json.dumps(result['errors']).replace(token,'[redacted]'))
data=result['data']['viewer']['accounts'][0]
assert all(len(v)<1000 for v in data.values()),'Analytics result reached limit; narrow the window'
total={}
for row in data['periodic']:
 for k,v in row['sum'].items():total[k]=total.get(k,0)+v
requests=sum(r['sum']['requests'] for r in data['invocations'])
# Published paid-plan rates, before allowances/rounding; incoming WebSocket messages
# are a separate metering dimension and are not inferred from this request count.
estimate={'duration':total.get('duration',0)*12.5/1e6,'sqlite_rows_read':total.get('rowsRead',0)*.001/1e6,'sqlite_rows_written':total.get('rowsWritten',0)/1e6,'observed_invocations':requests*.15/1e6}
# Price only operation types observed/understood by this text-workload fixture.
r2_rates={'ListObjects':4.5,'HeadBucket':.36,'DeleteObject':0,'DeleteBucket':0,'AbortMultipartUpload':0}
r2_unpriced=[r for r in data['r2']if r['dimensions']['actionType']not in r2_rates or r['dimensions']['storageClass']!='Standard' or r['dimensions']['actionStatus']!='success']
r2_cost=sum(r['sum']['requests']*r2_rates[r['dimensions']['actionType']]/1e6 for r in data['r2']if r not in r2_unpriced)
additional={'workers_cpu_only':sum(r['sum']['cpuTimeUs']for r in data['workers'])/1000*.02/1e6,'r2_known_standard_operations':r2_cost}
report={'retrieved_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'window':{'start':start,'end':end},'namespaces':namespaces,'query':query,'data':data,'totals':total,'observed_invocations':requests,'worker_totals':{k:sum(r['sum'][k]for r in data['workers'])for k in ['requests','cpuTimeUs','errors','clientDisconnects']},'unrounded_paid_rate_components_usd':estimate,'unrounded_component_subtotal_usd':sum(estimate.values()),'additional_unrounded_components_usd':additional,'r2_unpriced_actions':r2_unpriced,'additional_pricing_sources':['https://developers.cloudflare.com/workers/platform/pricing/','https://developers.cloudflare.com/r2/pricing/'],'pricing_checked':'2026-09-11','pricing_source':'https://developers.cloudflare.com/durable-objects/platform/pricing/','limitations':['Namespace analytics may arrive late or be adaptively sampled','Not an invoice: before included allocations and account-level rounding','DO subtotal excludes separately priced Worker CPU/R2 operations, Worker request fees, container/image storage and SQL/R2 GB-month storage; service-binding request counts are not additional billed Worker requests','Observed invocation request charge excludes any separately metered incoming WebSocket messages','Do not add request wall spans to duration; periodic.duration is the GB-second counter','Memory is per isolate, possibly shared by several objects']}
folder.mkdir(parents=True,exist_ok=True)
(folder/'cloudflare-metrics.json').write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps({'window':report['window'],'totals':total,'unrounded_component_subtotal_usd':sum(estimate.values())}))
