"""Plot recorded measurements; no credentials or network calls. Run with uv --with matplotlib."""
import json,sys,statistics
from pathlib import Path
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
from matplotlib.lines import Line2D

folder=Path(sys.argv[1])
rows=json.loads((folder/'measurements.json').read_text())['records']
models=['gpt-5.6-luna','gpt-5.6-terra','gpt-5.6-sol','gpt-6-astra']
efforts=['low','medium','high']
paths=['nanocodex_cloudflare','openai_agents','responses_http']
names={'nanocodex_cloudflare':'Nanocodex / Cloudflare','openai_agents':'OpenAI Agents','responses_http':'Responses HTTP'}
colors=dict(zip(paths,['#087f8c','#e57530','#687589']))
tasks=['extraction','schedule','long_output']
titles={'extraction':'120-ticket extraction','schedule':'18-job dependency schedule','long_output':'60-invoice long JSON output'}
plt.rcParams.update({'font.family':'DejaVu Sans','font.size':10,'axes.spines.top':False,'axes.spines.right':False,'axes.titleweight':'bold','figure.facecolor':'white','axes.facecolor':'white','svg.fonttype':'none'})
def successful(r):return r.get('terminal')=='completed' and not r.get('error')
def save(fig,name):
 fig.savefig(folder/(name+'.png'),dpi=170,bbox_inches='tight')
 fig.savefig(folder/(name+'.svg'),bbox_inches='tight')
 svg=folder/(name+'.svg');svg.write_text('\n'.join(line.rstrip()for line in svg.read_text().splitlines())+'\n')
 plt.close(fig)
main=[r for r in rows if r['label']=='matrix' and r['state']=='fresh']
for tier in ['default','fast']:
 fig,axes=plt.subplots(3,2,figsize=(17,13),sharex=True)
 for ti,task in enumerate(tasks):
  for mi,metric in enumerate(['ttft_ms','completion_ms']):
   ax=axes[ti,mi]
   for pi,path in enumerate(paths):
    for model_index,model in enumerate(models):
     for ei,effort in enumerate(efforts):
      rs=[r for r in main if (r['tier'],r['workload'],r['path'],r['model'],r['effort'])==(tier,task,path,model,effort)]
      vals=[r[metric]/1000 for r in rs if successful(r) and r.get(metric) is not None]
      if not vals:continue
      x=model_index*3+ei+(pi-1)*.22
      med=statistics.median(vals)
      ax.errorbar(x,med,yerr=[[med-min(vals)],[max(vals)-med]],fmt='o',markersize=5,color=colors[path],capsize=2,linewidth=1.1)
   ax.set_title(titles[task]+' · '+('visible TTFT' if mi==0 else 'completion'))
   ax.set_ylabel('Seconds from fresh request')
   ax.set_ylim(bottom=0);ax.grid(axis='y',alpha=.2)
   for x in [2.5,5.5,8.5]:ax.axvline(x,color='#dddddd',linewidth=.7)
   ax.set_xticks(range(12),[m.removeprefix('gpt-5.6-').removeprefix('gpt-6-')+'\n'+e for m in models for e in efforts])
 fig.suptitle('Same tasks and requested settings · '+tier+' service tier',fontsize=20,y=.995)
 fig.legend([Line2D([0],[0],marker='o',color=colors[p],linestyle='') for p in paths],[names[p] for p in paths],loc='upper center',bbox_to_anchor=(.5,.969),ncol=3,frameon=False)
 fig.text(.05,.005,'Dots: median; whiskers: observed min–max (2 planned trials per cell). Completed runs only; correctness and failures are reported separately.\nFresh Nanocodex includes create, durable admission and streaming. Built-in harness prompts differ. These are not confidence intervals.',fontsize=10,color='#444444')
 fig.tight_layout(rect=(0,.045,1,.94));save(fig,'latency-'+tier)

warm=[r for r in rows if r['label']=='warm-study']
if warm:
 fig,axes=plt.subplots(1,2,figsize=(14,5))
 for ax,metric in zip(axes,['ttft_ms','completion_ms']):
  for pi,path in enumerate(paths[:2]):
   for model_index,model in enumerate(models):
    for si,state in enumerate(['fresh','warm']):
     vals=[r[metric]/1000 for r in warm if successful(r) and r.get(metric) is not None and (r['path'],r['model'],r['state'])==(path,model,state)]
     if vals:ax.scatter(model_index+(pi-.5)*.28,statistics.median(vals),marker='o' if state=='fresh' else 'D',s=65,color=colors[path],facecolors=colors[path] if state=='warm' else 'none')
  ax.set_xticks(range(4),['Luna','Terra','Sol','Astra']);ax.set_ylabel('Seconds');ax.grid(axis='y',alpha=.2);ax.set_ylim(bottom=0)
  ax.set_title('Visible TTFT' if metric=='ttft_ms' else 'Completion')
 fig.suptitle('Repeated schedule prompt in the same session · default tier',fontsize=17)
 fig.text(.05,.01,'Hollow circle: fresh; filled diamond: warm. Teal: Nanocodex; orange: OpenAI Agents. Medians across low/high efforts and repetitions.\nWarm repeats include conversation context and provider cache effects, so this does not isolate connection reuse.',fontsize=10)
 fig.tight_layout(rect=(0,.10,1,.92));save(fig,'warm-sessions')

baseline=[r for r in rows if r['label']=='baseline' and successful(r)]
key=lambda r:tuple(r[k] for k in ['model','effort','tier','workload','state','repetition'])
lookup={key(r):r for r in main if r['path']=='nanocodex_cloudflare' and successful(r)}
paired=[(b,lookup[key(b)]) for b in baseline if key(b) in lookup]
if paired:
 fig,axes=plt.subplots(1,3,figsize=(14,5))
 for ax,metric,title in zip(axes,['create_ms','model_started_ms','completion_ms'],['Create agent','Request → model start','Request → completion']):
  for b,a in paired:
   if b.get(metric) is not None and a.get(metric) is not None:ax.plot([0,1],[b[metric]/1000,a[metric]/1000],color='#b9c5cc',alpha=.5)
  for i in range(2):
   vals=[p[i][metric]/1000 for p in paired if p[i].get(metric) is not None]
   if vals:ax.scatter(i,statistics.median(vals),s=100,color=['#687589','#087f8c'][i],zorder=3)
  ax.set_xticks([0,1],['Before','After']);ax.set_title(title);ax.set_ylabel('Seconds');ax.set_ylim(bottom=0);ax.grid(axis='y',alpha=.2)
 fig.suptitle('Restricted-agent startup optimization · matched configurations',fontsize=17)
 fig.text(.05,.01,f'{len(paired)} matched trial positions. Lines: observations; dots: pooled medians. Sequential deployments, not a randomized causal experiment.\nAfter = skip unavailable account catalogs and startup retrieval. Model/service/cache variability remains.',fontsize=10)
 fig.tight_layout(rect=(0,.10,1,.92));save(fig,'startup-before-after')

if main:
 fig,axes=plt.subplots(1,3,figsize=(16,5))
 for ax,task in zip(axes,tasks):
  for path in paths:
   for model in models:
    for tier in ['default','fast']:
     rs=[r for r in main if successful(r) and (r['path'],r['model'],r['tier'],r['workload'])==(path,model,tier,task)]
     priced=[r for r in rs if r.get('cost')]
     if not priced:continue
     x=statistics.median(r['completion_ms']/1000 for r in priced)
     y=statistics.median(r['cost']['low'] for r in priced)
     high=statistics.median(r['cost']['high'] for r in priced)
     ax.errorbar(x,y,yerr=[[0],[high-y]],fmt='o' if tier=='default' else '^',color=colors[path],markersize=6,capsize=2)
     if path=='nanocodex_cloudflare' and tier=='default':ax.annotate(model.split('-')[-1],(x,y),xytext=(4,4),textcoords='offset points',fontsize=8)
  ax.set_xscale('log');ax.set_yscale('log');ax.set_xlabel('Median completion (seconds, log scale)');ax.set_ylabel('Reported model cost (USD, log scale)');ax.set_title(titles[task]);ax.grid(alpha=.18)
 fig.suptitle('Cost and latency · medians across low / medium / high reasoning',fontsize=17)
 fig.text(.05,.01,'Teal: Nanocodex; orange: Agents; gray: Responses. Circles: default; triangles: fast. Cost bars reflect unknown cache-write premiums.\nModel-token estimates only; not invoices. Missing or late usage, Cloudflare infrastructure and external tools are excluded. Correctness is reported separately.',fontsize=10)
 fig.tight_layout(rect=(0,.10,1,.92));save(fig,'cost-latency')

advanced=[r for r in rows if r['label']=='advanced-thinking']
if advanced:
 fig,axes=plt.subplots(1,2,figsize=(16,5))
 relevant=advanced+[r for r in main if r['workload']=='schedule' and r['tier']=='default' and r['effort']=='high']
 for ax,metric in zip(axes,['ttft_ms','completion_ms']):
  for pi,path in enumerate(paths[:2]):
   for mi,model in enumerate(models):
    for ei,effort in enumerate(['high','xhigh','max']):
     vals=[r[metric]/1000 for r in relevant if successful(r) and r.get(metric) is not None and (r['path'],r['model'],r['effort'])==(path,model,effort)]
     if not vals:continue
     med=statistics.median(vals);ax.errorbar(mi*3+ei+(pi-.5)*.23,med,yerr=[[med-min(vals)],[max(vals)-med]],fmt='o',color=colors[path],capsize=2)
  ax.set_xticks(range(12),[m.split('-')[-1]+'\n'+e for m in models for e in ['high','xhigh','max']]);ax.set_ylim(bottom=0);ax.grid(axis='y',alpha=.2);ax.set_ylabel('Seconds')
  ax.set_title('Visible TTFT' if metric=='ttft_ms' else 'Completion')
 fig.suptitle('Higher reasoning efforts · fresh schedule task · default tier',fontsize=17)
 fig.text(.05,.01,'Teal: Nanocodex; orange: OpenAI Agents. Dots: medians; whiskers: observed ranges, not confidence intervals.\nHigh comes from the main matrix; xhigh/max are a later cohort at concurrency two. Unsupported configurations and failures remain in the table.',fontsize=10)
 fig.tight_layout(rect=(0,.12,1,.92));save(fig,'advanced-thinking')

if main:
 fig,axes=plt.subplots(1,2,figsize=(13,8),sharey=True)
 pairs=[]
 for model in models:
  for task in tasks:pairs.append((model,task))
 for ax,metric in zip(axes,['ttft_ms','completion_ms']):
  for ti,tier in enumerate(['default','fast']):
   for i,(model,task) in enumerate(pairs):
    chosen=[r for r in main if (r['model'],r['workload'],r['tier'])==(model,task,tier) and successful(r) and r.get(metric) is not None]
    lookup={(r['path'],r['effort'],r['repetition']):r for r in chosen}
    reductions=[]
    for effort in efforts:
     for rep in [1,2]:
      n=lookup.get(('nanocodex_cloudflare',effort,rep));o=lookup.get(('openai_agents',effort,rep))
      if n and o:reductions.append(100*(1-n[metric]/o[metric]))
    if reductions:ax.barh(i+(ti-.5)*.3,statistics.median(reductions),height=.27,color=['#087f8c','#44b0bb'][ti])
  ax.axvline(0,color='#333333',linewidth=.8);ax.grid(axis='x',alpha=.18);ax.set_axisbelow(True)
  ax.set_title('Visible TTFT reduction' if metric=='ttft_ms' else 'Completion reduction');ax.set_xlabel('Nanocodex reduction versus Agents (%)')
 axes[0].set_yticks(range(len(pairs)),[m.split('-')[-1].title()+' · '+{'extraction':'extraction','schedule':'schedule','long_output':'long output'}[t]for m,t in pairs]);axes[0].invert_yaxis()
 fig.suptitle('Where Nanocodex is faster — and where it is slower',fontsize=18)
 fig.legend([Line2D([0],[0],color=c,linewidth=8)for c in ['#087f8c','#44b0bb']],['Default tier','Fast tier'],loc='upper center',bbox_to_anchor=(.57,.945),ncol=2,frameon=False)
 fig.text(.05,.012,'Positive: Nanocodex faster. Negative: Agents faster. Median of paired reductions across low/medium/high × two repetitions.\nPairs match settings and repetition; they are not simultaneous requests. Completed outputs include incorrect results; consult correctness counts.',fontsize=10)
 fig.tight_layout(rect=(0,.07,1,.89));save(fig,'overview')

disabled=[r for r in rows if r['label']=='delegation-disabled']
if disabled:
 fig,axes=plt.subplots(3,2,figsize=(12,10),sharex=True)
 for ti,task in enumerate(tasks):
  for mi,metric in enumerate(['ttft_ms','completion_ms']):
   ax=axes[ti,mi]
   for pi,path in enumerate(paths[:2]):
    for model_index,model in enumerate(['gpt-5.6-luna','gpt-5.6-sol']):
     for ei,effort in enumerate(['low','high']):
      vals=[r[metric]/1000 for r in disabled if successful(r) and r.get(metric) is not None and (r['path'],r['model'],r['workload'],r['effort'])==(path,model,task,effort)]
      if not vals:continue
      med=statistics.median(vals);ax.errorbar(model_index*2+ei+(pi-.5)*.2,med,yerr=[[med-min(vals)],[max(vals)-med]],fmt='o',color=colors[path],capsize=3)
   ax.set_title(titles[task]+' · '+('TTFT' if mi==0 else 'completion'));ax.set_ylim(bottom=0);ax.grid(axis='y',alpha=.2);ax.set_ylabel('Seconds')
   ax.set_xticks(range(4),['Luna / low','Luna / high','Sol / low','Sol / high'])
 fig.suptitle('Explicitly disabled delegation · both hosted-agent APIs · default tier',fontsize=16)
 fig.text(.05,.012,'Teal: Nanocodex; orange: OpenAI Agents. Fresh sessions, two trials per cell, concurrency two.\nDots: medians; whiskers: observed ranges. This later cohort uses the final Worker version and is separate from the main matrix.',fontsize=10)
 fig.tight_layout(rect=(0,.065,1,.95));save(fig,'delegation-disabled')

if main:
 fig,axes=plt.subplots(1,2,figsize=(13,5))
 for ax,metric,title,scale in zip(axes,['post_text_ms','visible_characters_per_second'],['Last visible text → terminal','Visible output delivery rate'],[1000,1]):
  for pi,path in enumerate(paths):
   for ti,task in enumerate(tasks):
    vals=[r[metric]/scale for r in main if successful(r) and (r['path'],r['workload'])==(path,task) and r.get(metric) is not None]
    if vals:ax.bar(ti+(pi-1)*.23,statistics.median(vals),width=.21,color=colors[path])
  ax.set_xticks(range(3),['Extraction','Schedule','Long output']);ax.set_title(title);ax.set_ylim(bottom=0);ax.grid(axis='y',alpha=.18);ax.set_axisbelow(True)
 axes[0].set_ylabel('Median seconds');axes[1].set_ylabel('Median characters / second')
 fig.suptitle('Delivery phases · pooled main-matrix observations',fontsize=17)
 fig.legend([Line2D([0],[0],color=colors[p],linewidth=8)for p in paths],[names[p]for p in paths],loc='upper center',bbox_to_anchor=(.5,.92),ncol=3,frameon=False)
 fig.text(.05,.01,'Different harnesses expose different terminal semantics. Rate excludes the first text batch and is not billed tokens per second.\nCompleted outputs include incorrect results. These medians pool models, reasoning efforts and service tiers within each task.',fontsize=10)
 fig.tight_layout(rect=(0,.10,1,.83));save(fig,'delivery-phases')
