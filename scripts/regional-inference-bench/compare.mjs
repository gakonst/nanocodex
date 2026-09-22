#!/usr/bin/env node
import fs from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {quantile} from './core.mjs';
// Compare exactly one variable. All other dimensions must match.
export function compare(rows,dimension,before,after) {
  if(!['stage','arm','stream'].includes(dimension)||before===after)throw new Error('invalid_comparison');
  const dims=['run_id','runner','model_requested','family','stream','arm','stage'].filter(k=>k!==dimension);
  // Stage/arm often carry endpoint labels. Keep them controlled rather than guessing equivalence.
  const groups=new Map();
  for(const row of rows) {
    const side=String(row[dimension]);if(side!==before&&side!==after)continue;
    const key=JSON.stringify(dims.map(k=>row[k]));if(!groups.has(key))groups.set(key,{before:new Map(),after:new Map()});
    const bucket=groups.get(key)[side===before?'before':'after'];
    if(bucket.has(row.pair))throw new Error('duplicate_pair');bucket.set(row.pair,row);
  }
  const success=r=>r&&!r.error&&r.status==='completed'&&Number.isFinite(r.first_meaningful_ms);
  return [...groups].map(([key,g])=>{
    const a=[...g.before.values()].filter(success).map(r=>r.first_meaningful_ms),b=[...g.after.values()].filter(success).map(r=>r.first_meaningful_ms);
    const pairs=[...g.before].flatMap(([pair,x])=>{const y=g.after.get(pair);return success(x)&&success(y)?[{pair,before_ms:x.first_meaningful_ms,after_ms:y.first_meaningful_ms,delta_ms:y.first_meaningful_ms-x.first_meaningful_ms,delta_percent:x.first_meaningful_ms>0?100*(y.first_meaningful_ms/x.first_meaningful_ms-1):null}]:[];});
    const a50=quantile(a,.5),b50=quantile(b,.5),a95=quantile(a,.95),b95=quantile(b,.95);
    return {dimensions:Object.fromEntries(dims.map((d,i)=>[d,JSON.parse(key)[i]])),comparison:{dimension,before,after},attempts_before:g.before.size,attempts_after:g.after.size,completed_before:a.length,completed_after:b.length,paired_n:pairs.length,before_p50_ms:a50,after_p50_ms:b50,p50_delta_ms:a50!==null&&b50!==null?b50-a50:null,before_p95_ms:a95,after_p95_ms:b95,p95_delta_ms:a95!==null&&b95!==null?b95-a95:null,paired_delta_p50_ms:quantile(pairs.map(p=>p.delta_ms),.5),paired_delta_p95_ms:quantile(pairs.map(p=>p.delta_ms),.95),p95_caution:Math.min(a.length,b.length)<20?'fewer_than_20_samples':null,pairs};
  });
}
if(process.argv[1]===fileURLToPath(import.meta.url)) {
  try {const [dimension,before,after,...files]=process.argv.slice(2);if(!files.length)throw 0;const rows=(await Promise.all(files.map(f=>fs.readFile(f,'utf8')))).flatMap(s=>s.trim().split('\n').filter(Boolean).map(l=>JSON.parse(l)));console.log(JSON.stringify(compare(rows,dimension,before,after),null,2));}
  catch{console.error('Comparison failed: provide dimension before after JSONL-files; duplicate pairs are rejected.');process.exitCode=1;}
}
