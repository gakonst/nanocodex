import {summarizeProviderObservationGroups, PROVIDER_TELEMETRY_WINDOW_MS} from '../../js/managed/src/provider-telemetry.ts';
export const SOURCE = 'experiment_api_output_delivery_ttft_proxy_not_internal_generation_ttft';
export const CANDIDATES = ['cloudflare','openrouter','vercel'].map(p=>`${p}:openai/gpt-5.6-luna:low`);
export const REGIONS = {'us-east-1':'IAD','eu-west-1':'LHR','ap-northeast-1':'NRT'};
export const PROMPTS = [
 {id:'fresh-code-01',family:'short-code',text:'Fix this JavaScript function so it returns the sum of all array elements, including the last one: function sum(xs) { let n=0; for(let i=0;i<xs.length-1;i++) n+=xs[i]; return n; } Return corrected code and one brief explanation.'},
 {id:'fresh-math-01',family:'short-math',text:'A box contains 4 red and 6 blue balls. Two balls are drawn without replacement. What is the probability that they have different colors? Give an exact fraction and a short derivation.'},
 {id:'fresh-analysis-01',family:'short-analysis',text:'A service reports that average latency fell from 200 ms to 150 ms after a change, but its traffic also shifted toward smaller requests. Explain the confounder and propose one fair comparison in three concise sentences.'},
];
export const POLICY = {strategy:'direct',candidates:CANDIDATES,frontier_model:'gpt-5.6-luna',frontier_thinking:'low',objective:'time',preferences:{completion:1,cost:1,duration:100,text:'Controlled experiment: all generationTtft fields below contain measured API first public output delivery latency at regional benchmark callers. They are an experimental proxy, not isolated internal generation TTFT. Prefer responsiveness among these identical model/effort candidates. These calibration tasks do not establish success probabilities.'},min_confidence:.75,low_confidence_fallback:'proposed'};
export function rayColo(value) {return typeof value==='string'?value.match(/-([A-Z]{3})$/)?.[1]??null:null;}
export function parseCase(input) {
 if(!input||Object.keys(input).sort().join(',')!=='arm,prompt_index'||!['global','regional'].includes(input.arm)||!Number.isInteger(input.prompt_index)||!PROMPTS[input.prompt_index]) throw Error('invalid_case');
 return {...input,id:`${input.prompt_index}:${input.arm}`,prompt:PROMPTS[input.prompt_index]};
}
export function makeCalibration(rows,runners,now=Date.now()) {
 const chosen=rows.filter(r=>Number.isInteger(r.pair)&&r.pair>=0&&r.pair<=2);
 if(chosen.length!==27)throw Error('requires_exactly_27_calibration_rows');
 const seen=new Set();
 const observations=chosen.map(r=>{
  const runner=runners.find(x=>x.id===r.runner),region=runner&&REGIONS[runner.region];
  const id=JSON.stringify([r.runner,r.model_requested,r.pair]);
  if(seen.has(id))throw Error('duplicate_calibration');seen.add(id);
  if(!region||rayColo(r.headers?.['cf-ray'])!==region||r.stage!=='streaming'||r.split!=='calibration'||r.buffering!=='streaming'||r.error||r.status!=='completed'||r.http_status!==200||!CANDIDATES.includes(r.model_requested)||r.route?.model!=='gpt-5.6-luna'||r.route?.thinking!=='low'||r.route?.backend!==r.model_requested.split(':')[0]||r.family!==(r.pair===1?'long-prefix':'short'))throw Error('invalid_calibration_evidence');
  if(!Number.isFinite(r.first_meaningful_ms)||r.first_meaningful_ms<0||!Number.isFinite(r.total_ms)||r.total_ms<r.first_meaningful_ms)throw Error('invalid_calibration_timing');
  const timestamp=Date.parse(r.timestamp);
  if(!Number.isFinite(timestamp)||timestamp>now||now-timestamp>PROVIDER_TELEMETRY_WINDOW_MS)throw Error('calibration_stale');
  return {timestamp,source:'live',workerColo:null,clientIngressColo:region,backend:r.model_requested.split(':')[0],model:'gpt-5.6-luna',effort:'low',outcome:'success',status:r.http_status,headersMs:r.headers_ms,fullResponseMs:r.total_ms,generationTtftMs:r.first_meaningful_ms,clientDeliveryMs:null,elapsedMs:r.total_ms};
 });
 return {version:1,source:SOURCE,frozen_at:new Date(now).toISOString(),calibration_pairs:[0,1,2],excluded_pairs:[3,4],sample_count:27,family_composition:'each provider/region: 2 short + 1 long-prefix; pooled proxy despite heldout prompts all short',timestamp_policy:'Original observation timestamps retained; never refreshed or rebased.',metrics:summarizeProviderObservationGroups(observations,now),rows:chosen.map(r=>({runner:r.runner,pair:r.pair,family:r.family,model_requested:r.model_requested,timestamp:r.timestamp,api_ingress:rayColo(r.headers['cf-ray']),first_meaningful_ms:r.first_meaningful_ms,total_ms:r.total_ms}))};
}
export function assertFresh(calibration,now=Date.now()) {
 if(calibration.source!==SOURCE||calibration.sample_count!==27||calibration.metrics.length!==12||calibration.metrics.some(m=>m.successCount!==m.sampleCount||m.generationTtftSampleCount<3||m.lastObservedAt>now||now-m.lastObservedAt>m.windowMs))throw Error('invalid_or_expired_calibration');
}
