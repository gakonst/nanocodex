"""Produce concise, reproducible findings from reduced benchmark measurements."""
import json,statistics,sys
from pathlib import Path
folder=Path(sys.argv[1]);rows=json.loads((folder/'measurements.json').read_text())['records']
main=[r for r in rows if r['label']=='matrix']
def ok(r):return r.get('terminal')=='completed' and not r.get('error')
def median(rs,k):
 values=[r[k] for r in rs if r.get(k) is not None]
 return statistics.median(values)if values else None
def sec(x):return '—'if x is None else f'{x/1000:.2f}'
def comparison(rs):
 key=lambda r:tuple(r[k]for k in ['model','effort','tier','workload','state','repetition'])
 a={key(r):r for r in rs if r['path']=='nanocodex_cloudflare' and ok(r)}
 b={key(r):r for r in rs if r['path']=='openai_agents' and ok(r)}
 pairs=[(v,b[k])for k,v in a.items()if k in b]
 output={}
 for metric in ['ttft_ms','completion_ms']:
  selected=[(a,b)for a,b in pairs if a.get(metric) is not None and b.get(metric)]
  valid=[(a,b)for a,b in selected if a['correct']and b['correct']]
  output[metric]={'pairs':len(selected),'nanocodex_faster':sum(a[metric]<b[metric]for a,b in selected),'median_paired_reduction_percent':statistics.median(100*(1-a[metric]/b[metric])for a,b in selected)if selected else None,'both_correct_pairs':len(valid),'both_correct_median_reduction_percent':statistics.median(100*(1-a[metric]/b[metric])for a,b in valid)if valid else None}
 return output
findings={'main':comparison(main),'delegation_disabled':comparison([r for r in rows if r['label']=='delegation-disabled'])}
lines=['## Results','', '![Matched latency comparison](overview.png)','',
 '| Main matrix path | Correct / attempted | Generation errors | Completed without TTFT | Visible TTFT median (s) | Completion median (s) |',
 '| --- | ---: | ---: | ---: | ---: | ---: |']
for path,name in [('nanocodex_cloudflare','Nanocodex / Cloudflare'),('openai_agents','OpenAI Agents'),('responses_http','Responses HTTP')]:
 rs=[r for r in main if r['path']==path];success=[r for r in rs if ok(r)]
 lines.append(f'| {name} | {sum(r.get("correct",False)for r in rs)}/{len(rs)} | {sum(bool(r.get("error"))for r in rs)} | {sum(r.get("ttft_ms") is None for r in success)} | {sec(median(success,"ttft_ms"))} | {sec(median(success,"completion_ms"))} |')
lines+=['','The pooled medians above describe this balanced task/model matrix. For matched','within-setting comparisons (including completed incorrect outputs):','']
for metric,title in [('ttft_ms','Visible TTFT'),('completion_ms','Completion')]:
 c=findings['main'][metric]
 if c['pairs']:lines.append(f'- {title}: Nanocodex faster in **{c["nanocodex_faster"]}/{c["pairs"]}** pairs; median paired reduction **{c["median_paired_reduction_percent"]:.1f}%**. Restricting to pairs where both answers were correct: **{c["both_correct_median_reduction_percent"]:.1f}%** across {c["both_correct_pairs"]} pairs.')
lines+=['', '### Text delivery and terminal notification', '', '![Delivery phases](delivery-phases.png)', '',
 '| Path | Last text → terminal median (s) | Visible characters / second median |',
 '| --- | ---: | ---: |']
for path in ['nanocodex_cloudflare','openai_agents','responses_http']:
 rs=[r for r in main if r['path']==path and ok(r)]
 rate=median(rs,'visible_characters_per_second')
 lines.append(f'| {path} | {sec(median(rs,"post_text_ms"))} | {round(rate,1)if rate is not None else "—"} |')
lines+=['','These clocks describe client-visible events. The two harnesses expose different',
 'terminal semantics; a larger completion advantage need not imply faster token',
 'generation. Character rate excludes the first batch and is not billed token throughput.','']
sol={path:[r for r in main if (r['path'],r['model'],r['tier'],r['workload'])==(path,'gpt-5.6-sol','default','extraction') and ok(r)]for path in ['nanocodex_cloudflare','openai_agents']}
if all(sol.values()):
 n=sol['nanocodex_cloudflare'];o=sol['openai_agents']
 reasoning=lambda rs:statistics.median(r['usage']['output_tokens_details']['reasoning_tokens']for r in rs if r.get('usage'))
 lines+=['', 'The chart exposes settings where the advantage reverses. For default-tier',
  f'Sol extraction, Nanocodex’s median pre-model span was {sec(median(n,"model_started_ms"))} seconds,',
  f'versus {sec(median(n,"ttft_ms"))} seconds to visible text. Reported reasoning-token medians',
  f'were {reasoning(n):g} for Nanocodex and {reasoning(o):g} for Agents. This is consistent',
  'with inference/harness behavior contributing substantially to that gap;',
  'the measurements do not attribute all of it to Cloudflare routing. This',
  'comparison is observational, not a causal decomposition.']
lines+=['',
 'Missing TTFTs are excluded from TTFT medians. Failed generations remain in',
 'attempted/quality counts; missing usage remains unknown. The [methodology](README.md)',
 'records observed failure details and the resulting recovery fix.', '',
 '| Main path | Reported model-token subtotal (USD) | Missing usage |',
 '| --- | ---: | ---: |']
for path in ['nanocodex_cloudflare','openai_agents','responses_http']:
 rs=[r for r in main if r['path']==path]
 lo=sum(r['cost']['low']for r in rs if r.get('cost'));hi=sum(r['cost']['high']for r in rs if r.get('cost'))
 amount=f'${lo:.4f}'if abs(hi-lo)<1e-12 else f'${lo:.4f}–${hi:.4f}'
 lines.append(f'| {path} | {amount} | {sum(not r.get("usage")for r in rs)} |')
lines+=['','Intervals account only for unknown cache-write premiums on reported tokens.',
 'They exclude missing usage, infrastructure and other charges; they are not bill bounds.','']
key=lambda r:tuple(r[k]for k in ['model','effort','tier','workload','state','repetition'])
after={key(r):r for r in main if r['path']=='nanocodex_cloudflare' and ok(r)}
before=[r for r in rows if r['label']=='baseline' and ok(r)and key(r)in after]
findings['startup']={}
lines+=['','### Startup optimization','','![Startup comparison](startup-before-after.png)','', '| Matched Nanocodex subset | Before median (s) | After median (s) |','| --- | ---: | ---: |']
for field,name in [('create_ms','Create'),('model_started_ms','Create through first model start'),('completion_ms','Completion')]:
 b=median(before,field);a=median([after[key(r)]for r in before],field);findings['startup'][field]={'n':len(before),'before':b,'after':a}
 lines.append(f'| {name} | {sec(b)} | {sec(a)} |')
lines+=['','These deployments were measured sequentially and at different concurrency. The','server traces directly establish which startup calls disappeared; the total','latency difference also includes provider/cache variability.','', '### Explicit delegation control','', '![Delegation disabled](delegation-disabled.png)','']
for metric,title in [('ttft_ms','Visible TTFT'),('completion_ms','Completion')]:
 c=findings['delegation_disabled'][metric]
 if c['pairs']:lines.append(f'- {title}: Nanocodex faster in {c["nanocodex_faster"]}/{c["pairs"]} pairs; median paired reduction {c["median_paired_reduction_percent"]:.1f}%.')
disabled=[r for r in rows if r['label']=='delegation-disabled'and r['path']=='nanocodex_cloudflare']
lines+=['',f'This later cohort contains {len(disabled)} Nanocodex trials, with {sum(r.get("correct",False)for r in disabled)} correct outputs and {sum((r.get("root_tool_calls")or 0)>0 for r in disabled)} trials reporting root tool calls. It is not pooled into the main matrix.','', '### Warm sessions and higher reasoning','', '![Warm sessions](warm-sessions.png)','', '| Schedule cohort | Path | Correct / attempted | TTFT median (s) | Completion median (s) |','| --- | --- | ---: | ---: | ---: |']
for state in ['fresh','warm']:
 for path in ['nanocodex_cloudflare','openai_agents']:
  rs=[r for r in rows if (r['label'],r['state'],r['path'])==('warm-study',state,path)]
  if rs:lines.append(f'| {state} | {path} | {sum(r.get("correct",False)for r in rs)}/{len(rs)} | {sec(median([r for r in rs if ok(r)],"ttft_ms"))} | {sec(median([r for r in rs if ok(r)],"completion_ms"))} |')
lines+=['','Warm repeats include retained conversation context and caching; they are not','independent cold requests. Higher reasoning results and availability are shown','separately below and in the full table.','', '![Higher reasoning](advanced-thinking.png)','', '### Cost and detailed evidence','', '![Model cost versus latency](cost-latency.png)','', '- [Default-tier detailed chart](latency-default.png) and [fast-tier detailed chart](latency-fast.png).','- [Every configuration, correctness count, observed range, token count and cost](TABLE.md).','- [Machine-readable findings](findings.json), [measurements](measurements.json) and [summary](summary.json).','- [Cloudflare namespace metrics](cloudflare-metrics.json) and [server construction traces](server-profile.json).','']
(folder/'findings.json').write_text(json.dumps(findings,indent=2)+'\n')
(folder/'RESULTS.md').write_text('\n'.join(lines).rstrip()+'\n')
p=folder/'README.md';s=p.read_text();marker='<!-- RESULTS: replaced after the complete measurement run -->'
if marker in s:p.write_text(s.replace(marker,'Read the [results and charts](RESULTS.md), including accuracy, startup, warm-session,\nhigher-reasoning and delegation comparisons.\n\n![Matched latency comparison](overview.png)'))
print(json.dumps(findings))
