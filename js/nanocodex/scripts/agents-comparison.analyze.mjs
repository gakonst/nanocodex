// Offline analysis only. Usage-derived model costs are estimates, never billing reconciliation.
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
const path = process.argv[2]; if (!path) throw new Error('supply matrix.json');
const report = JSON.parse(readFileSync(path, 'utf8'));
const rates = { 'gpt-5.6-luna': [.2, .02, .25, 1.2], 'gpt-5.6-terra': [2, .2, 2.5, 12], 'gpt-5.6-sol': [4, .4, 5, 20], 'gpt-6-astra': [10, 1, 12.5, 50] };
const median = xs => { const v = xs.filter(Number.isFinite).sort((a,b) => a-b); return v.length ? (v[Math.floor(v.length/2)] + v[Math.ceil(v.length/2)-1])/2 : null; };
const range = xs => { const v=xs.filter(Number.isFinite); return v.length ? [Math.min(...v), Math.max(...v)] : null; };
const successful = r => r.correct && ['response.completed', 'turn_completed', 'agent.session.turn.completed'].includes(r.terminal) && !r.error;
const estimates = report.records.map(r => {
  const u=r.usage; if (!u) return null;
  const read=u.input_tokens_details?.cached_tokens;
  const write=u.input_tokens_details?.cache_write_tokens;
  if (![u.input_tokens,u.output_tokens,read].every(Number.isFinite)) return null;
  const [inputRate,readRate,writeRate,outputRate]=rates[r.model];
  const actual=r.reported_tier ?? r.tier;
  if (!['default','standard','fast','priority'].includes(actual)) return null;
  const factor=['fast','priority'].includes(actual) ? 2 : 1;
  const ordinary=u.input_tokens-read-(write ?? 0);
  if (ordinary < 0) throw new Error('invalid token categories');
  const base=(ordinary*inputRate+read*readRate+(write ?? 0)*writeRate+u.output_tokens*outputRate)*factor/1e6;
  return { low:base, high:base+(write===undefined ? ordinary*(writeRate-inputRate)*factor/1e6 : 0), cache_write_known:write!==undefined, tier_reported:r.reported_tier!==undefined };
});
const groups=[];
for (const model of report.models) for (const tier of report.tiers) for (const path of ['responses_http','responses_ws','nanocodex_node','openai_agents']) {
  const indexes=report.records.flatMap((r,i)=>r.model===model&&r.tier===tier&&r.path===path?[i]:[]);
  const rows=indexes.map(i=>report.records[i]); const good=rows.filter(successful);
  groups.push({model,tier,path,n:rows.length,successes:good.length,
    ttft_median_ms:median(good.map(r=>r.ttft_ms)), ttft_range_ms:range(good.map(r=>r.ttft_ms)),
    completion_median_ms:median(good.map(r=>r.completion_ms)),completion_range_ms:range(good.map(r=>r.completion_ms)),
    first_event_median_ms:median(good.map(r=>r.first_event_ms)),connection_median_ms:median(rows.map(r=>r.connection_ms)),
    first_text_to_completion_median_ms:median(good.map(r=>r.ttft_ms===null?null:r.completion_ms-r.ttft_ms)),
    inclusive_ttft_median_ms:median(good.map(r=>r.ttft_ms===null?null:r.ttft_ms+(r.path==='responses_ws'?(r.connection_ms??0):r.agent_setup_ms??0))),
    input_tokens_median:median(good.map(r=>r.usage?.input_tokens)),output_tokens_median:median(good.map(r=>r.usage?.output_tokens)),
    cached_tokens_median:median(good.map(r=>r.usage?.input_tokens_details?.cached_tokens)),cache_write_tokens_median:median(good.map(r=>r.usage?.input_tokens_details?.cache_write_tokens)),
    reasoning_tokens_median:median(good.map(r=>r.usage?.output_tokens_details?.reasoning_tokens)),
    cost_low_median_usd:median(indexes.map(i=>estimates[i]?.low)),cost_high_median_usd:median(indexes.map(i=>estimates[i]?.high)),
    usage_known:rows.filter(r=>r.usage).length,
    reported_tiers:[...new Set(rows.map(r=>r.reported_tier??'not reported'))],
    failures:rows.filter(r=>!successful(r)).map(r=>({repetition:r.repetition,error:r.error,terminal:r.terminal,correct:r.correct})),
  });
}
const sessions=report.records.filter(r=>r.session_id);
const summary={started_at:report.started_at,finished_at:report.finished_at,complete:report.complete??false, trials:report.records.length,successes:report.records.filter(successful).length,
 sessions_created:sessions.length,sessions_deleted:sessions.filter(r=>r.cleanup?.some(c=>[200,204,404].includes(c.status))).length,
 usage_missing:estimates.filter(e=>!e).length,estimated_model_subtotal_low_usd:estimates.reduce((n,e)=>n+(e?.low??0),0),estimated_model_subtotal_high_usd:estimates.reduce((n,e)=>n+(e?.high??0),0),
 pricing_source:'https://developers.openai.com/api/docs/pricing',pricing_checked:'2026-09-11',groups};
writeFileSync(join(dirname(path),'summary.json'),JSON.stringify(summary,null,2)+'\n');
// Keep reviewable per-trial evidence; verbose wire traces remain in the input file.
writeFileSync(join(dirname(path),'measurements.json'),JSON.stringify({
  ...Object.fromEntries(Object.entries(report).filter(([key])=>key!=='records')),
  records:report.records.map(({events,saved_items,...row},i)=>({...row,estimated_model_cost:estimates[i],
    tool_calls:row.model_calls?.reduce((n,c)=>n+c.tool_calls,0)??null,
    outgoing_requests:events.filter(e=>e.event.type==='api.event'&&e.event.payload.direction==='outbound').map(e=>{
      const request=e.event.payload.event; const encoded=JSON.stringify(request);
      if (request.type==='response.create' && (!encoded.includes(report.prompt)||!encoded.includes(report.instructions))) throw new Error('Nano wire input differs from the fixture');
      return {elapsed_ms:e.elapsed_ms,type:request.type,model:request.model,service_tier:request.service_tier,
        reasoning:request.reasoning,max_output_tokens:request.max_output_tokens??null,store:request.store};
    }),
  })),
},null,2)+'\n');
const sec=n=>n===null?'—':(n/1000).toFixed(3);
const table=['| Model | Tier | Path | OK/n | First text p50 (s) | Completion p50 (s) | Completion min–max (s) | Input / output tokens p50 | Estimated model cost p50 (USD) |',
'| --- | --- | --- | ---: | ---: | ---: | --- | --- | --- |'];
for(const g of groups) table.push(`| ${g.model} | ${g.tier} | ${g.path} | ${g.successes}/${g.n} | ${sec(g.ttft_median_ms)} | ${sec(g.completion_median_ms)} | ${g.completion_range_ms?.map(sec).join('–')??'—'} | ${g.input_tokens_median??'—'} / ${g.output_tokens_median??'—'} | ${g.cost_low_median_usd===null?'unknown':g.cost_low_median_usd.toFixed(7)+(g.cost_high_median_usd>g.cost_low_median_usd?'–'+g.cost_high_median_usd.toFixed(7):'')} |`);
writeFileSync(join(dirname(path),'TABLE.md'),table.join('\n')+'\n');
console.log(JSON.stringify({trials:summary.trials,successes:summary.successes,sessions_created:summary.sessions_created,sessions_deleted:summary.sessions_deleted,usage_missing:summary.usage_missing,cost_low:summary.estimated_model_subtotal_low_usd,cost_high:summary.estimated_model_subtotal_high_usd}));
