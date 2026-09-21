#!/usr/bin/env node
// Offline analysis only. Reads completed fixed-provider matrices; never calls inference.
import fs from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {promptFor, quantile} from './core.mjs';

const project = fileURLToPath(new URL('../../', import.meta.url));
const relativeRoot = 'docs/performance/2026-09-21-regional-inference';
const root = path.join(project, relativeRoot);
const providers = ['cloudflare', 'openrouter', 'vercel'];
const regions = ['us-east-1', 'eu-west-1', 'ap-northeast-1'];
const colos = {'us-east-1':'IAD', 'eu-west-1':'LHR', 'ap-northeast-1':'NRT'};
const runners = {'us-east-1':'nanocodex-bench-us-20260921', 'eu-west-1':'nanocodex-bench-eu-20260921', 'ap-northeast-1':'nanocodex-bench-ap-20260921'};
const sha = text => createHash('sha256').update(text).digest('hex');
const number = x => Number.isFinite(x) ? x : null;
const subtract = (a,b) => a === null || b === null ? null : a-b;
const percent = (after,before) => before > 0 && after !== null ? 100*(after/before-1) : null;
const counts = values => Object.fromEntries([...new Set(values)].sort().map(v=>[String(v),values.filter(x=>x===v).length]));
const success = r => r.http_status === 200 && !r.error && r.status === 'completed' && r.terminal === true && Number.isFinite(r.first_meaningful_ms) && Number.isFinite(r.total_ms);
const identity = r => JSON.stringify([r.run_id,r.runner,r.model_requested,r.family,r.pair,r.split,r.arm,r.stream]);
const usageFields = {
  input_tokens:r=>r.usage?.input_tokens,
  output_tokens:r=>r.usage?.output_tokens,
  reasoning_tokens:r=>r.usage?.output_tokens_details?.reasoning_tokens,
  cached_tokens:r=>r.usage?.input_tokens_details?.cached_tokens,
};
function usage(rows) {
  return Object.fromEntries(Object.entries(usageFields).map(([key,get])=>{
    const values = rows.map(get).filter(Number.isFinite);
    return [key,{known_n:values.length,unknown_n:rows.length-values.length,sum:values.length?values.reduce((a,b)=>a+b,0):null,p50:quantile(values,.5),positive_n:values.filter(x=>x>0).length}];
  }));
}
function sample(r) {
  return {
    timestamp:r.timestamp,http_status:r.http_status,status:r.status,error:r.error ?? null,terminal:r.terminal,
    buffering:r.buffering,transport:r.transport,
    ...Object.fromEntries(['headers_ms','first_body_ms','first_meaningful_ms','completed_ms','total_ms','delta_count'].map(k=>[k,number(r[k])])),
    first_meaningful_kind:r.first_meaningful_kind,
    usage:Object.fromEntries(Object.entries(usageFields).map(([k,get])=>[k,number(get(r))])),
    evidence:{runner_response_placement:r.runner_response_placement ?? null,runner_evidence:r.runner_evidence,
      api_ray_colo:r.headers?.['cf-ray']?.split('-').at(-1) ?? null,api_ingress_colo:r.headers?.['x-nanocodex-ingress-colo'] ?? null,
      route_origin:r.route?.origin ?? null,route_worker_colo:r.route?.workerColo ?? null,route_client_ingress_colo:r.route?.clientIngressColo ?? null},
  };
}
function metric(pairs,key) {
  const eligible=pairs.filter(p=>p.completed_pair);
  const before=eligible.map(p=>p.before[key]),after=eligible.map(p=>p.after[key]);
  const deltas=eligible.map(p=>subtract(p.after[key],p.before[key])).filter(Number.isFinite);
  const a=quantile(before,.5),b=quantile(after,.5);
  return {paired_n:deltas.length,before_p50_ms:a,after_p50_ms:b,p50_delta_ms:subtract(b,a),p50_delta_percent:percent(b,a),
    before_p95_ms:quantile(before,.95),after_p95_ms:quantile(after,.95),
    paired_delta_p50_ms:quantile(deltas,.5),paired_delta_p95_ms:quantile(deltas,.95),
    paired_delta_percent_p50:quantile(eligible.map(p=>percent(p.after[key],p.before[key])),.5),
    faster_n:deltas.filter(d=>d<0).length,slower_n:deltas.filter(d=>d>0).length,equal_n:deltas.filter(d=>d===0).length};
}
function group(pairs,provider,region,family) {
  const selected=pairs.filter(p=>p.provider===provider&&p.region===region&&(family==='mixed'?true:p.family===family));
  const stageUsage=side=>usage(selected.map(p=>({usage:{input_tokens:p[side].usage.input_tokens,output_tokens:p[side].usage.output_tokens,input_tokens_details:{cached_tokens:p[side].usage.cached_tokens},output_tokens_details:{reasoning_tokens:p[side].usage.reasoning_tokens}}})));
  return {provider,region,family,description:family==='mixed'?'Descriptive mixture: three short and two long-prefix prompts, not a homogeneous latency population.':null,
    n:selected.length,completed_before:selected.filter(p=>p.before.status==='completed'&&!p.before.error).length,
    completed_after:selected.filter(p=>p.after.status==='completed'&&!p.after.error).length,
    first_meaningful:metric(selected,'first_meaningful_ms'),terminal_event:metric(selected,'completed_ms'),total_completion:metric(selected,'total_ms'),
    usage_before:stageUsage('before'),usage_after:stageUsage('after'),pair_ids:selected.map(p=>p.id)};
}

export async function analyze() {
  const inputs={};
  for(const stage of ['baseline','streaming']) {
    const text=await fs.readFile(path.join(root,stage,'results.jsonl'),'utf8');
    const manifest=JSON.parse(await fs.readFile(path.join(root,stage,'manifest.json'),'utf8'));
    const rows=text.trim().split('\n').filter(Boolean).map(line=>JSON.parse(line));
    if(rows.length!==45) throw Error(`${stage}: expected completed matrix with 45 saved rows; received ${rows.length}`);
    inputs[stage]={rows,manifest,sha256:sha(text)};
  }
  const mismatches=[];
  const check=(ok,row,assertion,observed,expected)=>{if(!ok)mismatches.push({stage:row.stage,region:row.placement_hint,model_requested:row.model_requested,pair:row.pair,assertion,observed:observed??null,expected});};
  for(const [stage,{rows}] of Object.entries(inputs)) {
    const seen=new Set();
    for(const r of rows) {
      const key=identity(r);
      if(seen.has(key))throw Error(`${stage}: duplicate pair ${key}`);
      seen.add(key);
      const provider=r.model_requested?.split(':')[0],expectedColo=colos[r.placement_hint];
      check(r.stage===stage,r,'stage',r.stage,stage);
      check(providers.includes(provider),r,'provider',provider,providers);
      check(regions.includes(r.placement_hint),r,'region',r.placement_hint,regions);
      check(r.runner===runners[r.placement_hint],r,'runner',r.runner,runners[r.placement_hint]);
      check(Number.isInteger(r.pair)&&r.pair>=0&&r.pair<5,r,'pair_index',r.pair,'0..4');
      check(r.family===(r.pair%2?'long-prefix':'short'),r,'family',r.family,r.pair%2?'long-prefix':'short');
      check(r.split===(r.pair<3?'calibration':'heldout'),r,'split',r.split,r.pair<3?'calibration':'heldout');
      for(const [k,v] of Object.entries({run_id:'regional-20260921-matched',arm:'fixed-luna-low',stream:true,model:'gpt-5.6-luna',model_requested:`${provider}:openai/gpt-5.6-luna:low`,buffering:stage==='baseline'?'buffered':'streaming',transport:'sse'}))check(r[k]===v,r,k,r[k],v);
      check(r.route?.backend===provider,r,'route_provider',r.route?.backend,provider);
      check(r.route?.thinking==='low',r,'route_effort',r.route?.thinking,'low');
      check(r.headers?.['x-nanocodex-provider']===provider,r,'header_provider',r.headers?.['x-nanocodex-provider'],provider);
      check(r.runner_evidence?.placement_hint===`aws:${r.placement_hint}`,r,'runner_placement_hint',r.runner_evidence?.placement_hint,`aws:${r.placement_hint}`);
      const named=r.runner_response_placement?.match(/^remote-([A-Z]{3})$/)?.[1];
      if(named)check(named===expectedColo,r,'named_execution_colo',named,expectedColo);
      // API ingress is a separate observation, not proof of provider compute origin.
      const ingress=r.headers?.['x-nanocodex-ingress-colo'];
      if(ingress)check(ingress===expectedColo,r,'api_ingress_vs_expected_runner_colo',ingress,expectedColo);
      const ray=r.headers?.['cf-ray']?.split('-').at(-1);
      if(ingress&&ray)check(ingress===ray,r,'api_ingress_header_vs_ray',ingress,ray);
    }
  }
  const after=new Map(inputs.streaming.rows.map(r=>[identity(r),r]));
  const pairs=inputs.baseline.rows.map(a=>{
    const b=after.get(identity(a));
    if(!b)throw Error(`Unmatched baseline pair ${identity(a)}`);
    const before=sample(a),next=sample(b);
    return {id:`${a.model_requested.split(':')[0]}/${a.placement_hint}/${a.family}/${a.pair}`,provider:a.model_requested.split(':')[0],region:a.placement_hint,runner:a.runner,
      family:a.family,pair:a.pair,split:a.split,model_requested:a.model_requested,run_id:a.run_id,arm:a.arm,stream:a.stream,
      prompt_sha256:sha(promptFor(a.family,a.run_id,a.pair)),completed_pair:success(a)&&success(b),before,after:next,
      delta_after_minus_before:{...Object.fromEntries(['first_meaningful_ms','completed_ms','total_ms'].map(k=>[k,subtract(next[k],before[k])])),
        ...Object.fromEntries(Object.keys(usageFields).map(k=>[k,subtract(next.usage[k],before.usage[k])]))}};
  }).sort((a,b)=>a.id.localeCompare(b.id));
  const familyGroups=providers.flatMap(p=>regions.flatMap(r=>['short','long-prefix'].map(f=>group(pairs,p,r,f))));
  if(familyGroups.some(g=>g.n!==(g.family==='short'?3:2)))throw Error('Missing or extra expected provider/region/family cases');
  const stages=Object.fromEntries(Object.entries(inputs).map(([stage,{rows,manifest,sha256}])=>[stage,{
    source:`${relativeRoot}/${stage}/results.jsonl`,sha256,manifest_source:manifest.source,
    started_at:rows.map(r=>r.timestamp).sort()[0],last_started_at:rows.map(r=>r.timestamp).sort().at(-1),
    attempts:rows.length,completed:rows.filter(success).length,errors:rows.filter(r=>r.error).map(r=>({pair_identity:identity(r),error:r.error})),
    incomplete:rows.filter(r=>r.status!=='completed').length,buffering:counts(rows.map(r=>r.buffering)),
    delta_count_range:[Math.min(...rows.map(r=>r.delta_count)),Math.max(...rows.map(r=>r.delta_count))],
    public_delta_before_terminal_n:rows.filter(r=>r.first_meaningful_ms<r.completed_ms).length,
    public_delta_before_body_end_n:rows.filter(r=>r.first_meaningful_ms<r.total_ms).length,
    public_delta_equals_terminal:rows.filter(r=>r.first_meaningful_ms===r.completed_ms).map(r=>({provider:r.route?.backend,region:r.placement_hint,family:r.family,pair:r.pair,first_meaningful_ms:r.first_meaningful_ms,completed_ms:r.completed_ms,total_ms:r.total_ms,delta_count:r.delta_count})),
    execution_placement:counts(rows.map(r=>r.runner_response_placement??'missing')),
    execution_placement_missing_named_n:rows.filter(r=>!/^remote-[A-Z]{3}$/.test(r.runner_response_placement??'')).length,
    api_ingress:counts(rows.map(r=>r.headers?.['x-nanocodex-ingress-colo']??'missing')),
    api_ray_colo:counts(rows.map(r=>r.headers?.['cf-ray']?.split('-').at(-1)??'missing')),
    controller_ingress:counts(rows.map(r=>r.runner_evidence?.ingress_colo??'missing')),
    route_origin_missing_n:rows.filter(r=>r.route?.origin==null).length,
    usage:usage(rows),usage_by_family:Object.fromEntries(['short','long-prefix'].map(f=>[f,usage(rows.filter(r=>r.family===f))])),
  }]));
  return {schema:'regional-inference-streaming-comparison-v1',stages,
    definitions:{delta:'after minus before; negative is faster',quantile:'nearest rank ceil(p*n); p50 for n=2 is lower observation; p95 is maximum at n=2,3,5',
      first_meaningful:'Runner outbound POST to first non-whitespace public text/tool-argument delta. Includes routing, network, queueing, reasoning, generation and delivery; not isolated model TTFT.',
      completed_ms:'Time to public terminal event',total_ms:'Time to body exhaustion / completed measurement after terminal event',
      prompts:'Synthetic prompt identity reconstructed with core.mjs promptFor; matched run_id/family/pair, runner, requested model/effort, arm, split and SSE request.',
      cache:'Positive provider-reported cached_tokens substantiates cache use. Missing is unknown, not zero.',
      confidence:'Descriptive low-N before/after comparison; load, time, generated/reasoning token counts and cache state are confounded. No causal regional or streaming-only effect, robust tail estimate, or tuned heldout claim.'},
    scope:{included:['baseline/results.jsonl','streaming/results.jsonl'],excluded:['local-baseline','baseline-failed-prefetch','postdeploy-buffered-aborted','Jev experiment and any future samples'],
      aborted_buffered_saved_rows:24,aborted_conservative_attempts:27,
      streaming_production_source:inputs.streaming.manifest.source,prior_attempts_upper_bound:99,streaming_attempts:45,total_attempts_upper_bound_at_streaming_close:144,max_attempts:200,remaining_attempts_at_streaming_close:56,
      incremental_budget_usd:5,cost:'Actual provider spend is not recorded in these rows; token usage is not an invoice. Attempt accounting predates any separate Jev experiment.'},
    validation:{matched_pairs:pairs.length,completed_pairs:pairs.filter(p=>p.completed_pair).length,mismatches,origin_assertion_scope:'Named cf-placement must match the runner hint; API ingress is checked separately against expected colo and Ray. Missing named placements and missing route origin are counted, never treated as successful origin assertions. Provider compute origin is not measured.'},
    family_groups:familyGroups,mixed_descriptive_groups:providers.flatMap(p=>regions.map(r=>group(pairs,p,r,'mixed'))),pairs};
}

const fmt=x=>x===null?'unknown':Number.isInteger(x)?String(x):x.toFixed(1);
const signed=x=>x===null?'unknown':`${x>0?'+':''}${fmt(x)}`;
const arrow=(a,b)=>`${fmt(a)} → ${fmt(b)}`;
function timingTable(groups) {
  return ['| Provider | Caller region | Family | n | First delta p50 B → S ms | Δ p50 % | Paired Δ p50 ms | Total p50 B → S ms | Δ p50 % | Paired total Δ p50 ms |',
    '|---|---|---|---:|---:|---:|---:|---:|---:|---:|',...groups.map(g=>{
      const f=g.first_meaningful,t=g.total_completion;
      return `| ${g.provider} | ${g.region} | ${g.family} | ${g.n} | ${arrow(f.before_p50_ms,f.after_p50_ms)} | ${signed(f.p50_delta_percent)}% | ${signed(f.paired_delta_p50_ms)} | ${arrow(t.before_p50_ms,t.after_p50_ms)} | ${signed(t.p50_delta_percent)}% | ${signed(t.paired_delta_p50_ms)} |`;
    })].join('\n');
}
function markdown(report) {
  const {baseline:b,streaming:s}=report.stages;
  const groups=report.family_groups;
  const faster=groups.filter(g=>g.first_meaningful.p50_delta_ms<0).length;
  const totalFaster=groups.filter(g=>g.total_completion.p50_delta_ms<0).length;
  const extreme=key=>[...groups].sort((a,b)=>a[key].p50_delta_percent-b[key].p50_delta_percent);
  const extremes=key=>[extreme(key)[0],extreme(key).at(-1)].map(g=>`${g.provider}/${g.region}/${g.family}: ${signed(g[key].p50_delta_ms)} ms (${signed(g[key].p50_delta_percent)}%)`).join('; ');
  return `# Buffered baseline versus production streaming

Generated offline by \`node scripts/regional-inference-bench/analyze.mjs\` from this checkout. [Protocol](PROTOCOL.md), [baseline rows](baseline/results.jsonl), [streaming rows](streaming/results.jsonl), and [complete paired comparison](comparison.json) contain the reproducible evidence. The JSON includes source hashes, all 45 pairs, reconstructed synthetic prompt hashes, nearest-rank p50/p95, per-pair timing and token deltas, completion counts and origin checks. No inference calls are made by the analyzer.

Both stages completed ${b.completed}/45 and ${s.completed}/45 calls, respectively; errors ${b.errors.length}/${s.errors.length}, incomplete ${b.incomplete}/${s.incomplete}. Each uses the same explicit Luna low candidate across three providers and three placed callers. Short prompts have **n=3 per cell** (pairs 0/2/4); long-prefix prompts have **n=2 per cell** (pairs 1/3). There are ${report.validation.matched_pairs} exact matched pairs and ${report.validation.mismatches.length} recorded assertion mismatches.

Baseline source: \`${b.manifest_source}\`; first-to-last request starts ${b.started_at}–${b.last_started_at}. Production streaming source: \`${s.manifest_source}\`; starts ${s.started_at}–${s.last_started_at}. These sequential windows are about 25 minutes apart. The streaming production deployment is ce18887f / 5d343432-7e2f-440d-a720-f60bff2add7f; the earlier aborted buffered attempt is excluded.

## What the clocks measure

First meaningful public delta is elapsed time from the placed runner's outbound POST until the first non-whitespace public text or tool-argument delta. Headers, creation, reasoning and empty deltas do not qualify. It includes routing, network, queueing and generation; **it is not isolated model TTFT**. Controller-to-runner network time is excluded. Total completion is body exhaustion (\`total_ms\`); terminal-event time (\`completed_ms\`) is retained separately in JSON.

All ${b.attempts} baseline responses declared \`buffered\`, despite SSE requests: public delta counts ranged ${b.delta_count_range.join('–')}, with first delta before terminal in ${b.public_delta_before_terminal_n}/45 calls. All ${s.attempts} streaming responses declared \`streaming\`, with ${s.delta_count_range.join('–')} meaningful deltas and first delta before terminal in ${s.public_delta_before_terminal_n}/45 calls (before body exhaustion in ${s.public_delta_before_body_end_n}/45). This is observed incremental public output versus buffered SSE replay. Streaming delivery alone does not guarantee an earlier first public delta in every request. The streaming exception with no observed gap before terminal was ${s.public_delta_equals_terminal.map(r=>`${r.provider}/${r.region}/${r.family}/pair-${r.pair} (first and terminal ${r.first_meaningful_ms} ms, body end ${r.total_ms} ms, ${r.delta_count} deltas)`).join('; ')}; event multiplicity and the delivery header do not establish a separately timed arrival for every event in that request.

## Family comparisons

B → S means baseline → streaming. Negative changes mean lower latency. Δ p50 is the difference (or ratio for %) between stage medians; paired Δ p50 is the median of individual matched after-minus-before differences, which need not equal the difference of medians. Quantiles use nearest rank \`ceil(p*n)\`, so n=2 p50 is the lower observation, not an averaged midpoint. Each cell completed n/n in both stages.

${timingTable(groups)}

First-delta p50 fell in ${faster}/18 family cells; total p50 fell in ${totalFaster}/18. These are cell counts, not a pooled latency or a statistical significance test. First-delta extremes: ${extremes('first_meaningful')}. Total-completion extremes: ${extremes('total_completion')}. All matched per-request gains and regressions remain in comparison.json.

## Mixed descriptive medians: n=5 only

Each row below mixes exactly three short and two long-prefix prompts. These n=5 medians describe this fixed workload mix; they do not establish a homogeneous latency distribution, a provider winner, or an optimal caller region. Interpret them alongside the family table.

${timingTable(report.mixed_descriptive_groups)}

## Generated tokens and cache state

The following are sums across each cell, not per-request medians. Output tokens are provider-reported generated usage (including reasoning where reported), not a measured count of visible text tokens. Cache “hits” count rows with positive provider-reported cached tokens. Missing token fields remain unknown in JSON.

| Provider | Caller region | Family | n | Output tokens B → S | Reasoning tokens B → S | Cache-hit rows B → S | Cached tokens B → S |
|---|---|---|---:|---:|---:|---:|---:|
${groups.map(g=>`| ${g.provider} | ${g.region} | ${g.family} | ${g.n} | ${arrow(g.usage_before.output_tokens.sum,g.usage_after.output_tokens.sum)} | ${arrow(g.usage_before.reasoning_tokens.sum,g.usage_after.reasoning_tokens.sum)} | ${arrow(g.usage_before.cached_tokens.positive_n,g.usage_after.cached_tokens.positive_n)} | ${arrow(g.usage_before.cached_tokens.sum,g.usage_after.cached_tokens.sum)} |`).join('\n')}

For accounting across the entire fixed 45-call workload (not a latency aggregate), input tokens were ${arrow(b.usage.input_tokens.sum,s.usage.input_tokens.sum)}, output tokens ${arrow(b.usage.output_tokens.sum,s.usage.output_tokens.sum)} (${signed(subtract(s.usage.output_tokens.sum,b.usage.output_tokens.sum))}), and reasoning tokens ${arrow(b.usage.reasoning_tokens.sum,s.usage.reasoning_tokens.sum)}. Long-prefix cache-hit rows were ${arrow(b.usage_by_family['long-prefix'].cached_tokens.positive_n,s.usage_by_family['long-prefix'].cached_tokens.positive_n)} of 18, and cached-token sums ${arrow(b.usage.cached_tokens.sum,s.usage.cached_tokens.sum)}. Short-prompt cache-hit rows were ${arrow(b.usage_by_family.short.cached_tokens.positive_n,s.usage_by_family.short.cached_tokens.positive_n)} of 27. Cached usage is known for ${b.usage.cached_tokens.known_n}/${b.attempts} baseline and ${s.usage.cached_tokens.known_n}/${s.attempts} streaming rows. The long repeated prefix offered reuse in both stages, but observed cache state changed. Generated and reasoning token differences also confound total completion and first-delta timing; no token-normalized causal estimate is claimed.

## Location evidence and assertion checks

Runner placement hints were aws:us-east-1, aws:eu-west-1 and aws:ap-northeast-1. Named authenticated runner-response placements corroborate IAD, LHR and NRT. Baseline placement counts: ${JSON.stringify(b.execution_placement)}; streaming: ${JSON.stringify(s.execution_placement)}. Named execution evidence is missing for ${b.execution_placement_missing_named_n}/45 baseline and ${s.execution_placement_missing_named_n}/45 streaming requests (\`remote-\` is not a named colo). Do not replace those missing observations with inferred per-request execution locations.

The controller ingress was ${JSON.stringify(b.controller_ingress)} baseline and ${JSON.stringify(s.controller_ingress)} streaming; SJC is not runner execution geography. The production API ingress header is absent in all baseline rows and reports ${JSON.stringify(s.api_ingress)} in streaming. API Ray suffix counts are ${JSON.stringify(b.api_ray_colo)} baseline and ${JSON.stringify(s.api_ray_colo)} streaming. API ingress is not provider compute geography. Explicit route origin is missing in ${b.route_origin_missing_n}/45 baseline and ${s.route_origin_missing_n}/45 streaming rows; this fixed-provider campaign cannot verify classifier-origin propagation.

The analyzer checks named runner placements against hints, API ingress against expected colo and Ray, exact pairing, requested/returned provider and effort, stage delivery mode, and family/split identities. Recorded mismatches: **${report.validation.mismatches.length}**. Missing evidence is counted separately and is not a passed origin assertion. HTTP/protocol errors: **${b.errors.length + s.errors.length}**. ${report.validation.mismatches.length?'See validation.mismatches in comparison.json for each discrepancy.':'No contradictory observed origin/placement assertions were found within these checks.'}

## Limits, exclusions and budget

This is a low-N sequential before/after observation. Time, load, cache state, routing overhead and generated/reasoning token variation remain confounded with delivery mode and deployment changes. Per-family p95 (and mixed n=5 p95) equals the maximum; neither robust population tails nor confidence in a regional ranking follows. No isolated model TTFT, randomized causal speedup, task-success probability or regional router quality claim is supported.

The four local calls and initial failed regional attempts are excluded. The earlier post-deployment attempt saved 24 **buffered** rows and conservatively counted 27 attempts; its preserved data in [postdeploy-buffered-aborted](postdeploy-buffered-aborted/results.jsonl) is excluded from the streaming arm. Campaign accounting at streaming close is **99 prior + 45 = 144 attempts**, leaving 56 of the 200-attempt cap before any separate Jev experiment. The incremental budget ceiling is $5; these records do not contain an actual-spend receipt, and output-token counts do not establish dollars spent.

This report consumes only the two completed fixed-provider matrices. Calibration/heldout labels are preserved per pair; descriptive reporting of the existing matched rows does not select or tune on the heldout rows. The separate active global-versus-regional Jev experiment and future samples are outside this analysis and belong in JEV.md.
`;
}

if(process.argv[1]===fileURLToPath(import.meta.url)) {
  const report=await analyze();
  await fs.writeFile(path.join(root,'comparison.json'),JSON.stringify(report,null,2)+'\n');
  await fs.writeFile(path.join(root,'STREAMING.md'),markdown(report));
  console.log(JSON.stringify({matched_pairs:report.validation.matched_pairs,completed_pairs:report.validation.completed_pairs,mismatches:report.validation.mismatches.length,outputs:[`${relativeRoot}/comparison.json`,`${relativeRoot}/STREAMING.md`]}));
  if(report.validation.mismatches.length)process.exitCode=1;
}
