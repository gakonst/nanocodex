import {readFile,writeFile} from 'node:fs/promises';
import {deployments} from './receipts.mjs';
const credentials=JSON.parse(await readFile('/Users/georgios/Library/Application Support/Nanocodex/Native/credentials.json','utf8'));
const base=credentials.baseUrl.replace(/\/$/,'');if(base!=='https://nanocodex.gakonst.workers.dev')throw Error('Unexpected origin');
const rows=[];
for(const file of ['hosted.json','hosted-confirm.json','hosted-current.json']){
 const d=JSON.parse(await readFile(new URL(file,import.meta.url),'utf8'));
 for(const resource of d.resources){const start=performance.now();const r=await fetch(base+'/v1/agents/'+resource.agent,{headers:{authorization:'Bearer '+credentials.apiKey},signal:AbortSignal.timeout(15000)});await r.text();rows.push({agent:resource.agent,cohort:file,status:r.status,ms:performance.now()-start});if(r.status!==404)process.exitCode=1;}
}
const report={at:new Date().toISOString(),rows};const save=()=>writeFile(new URL('cleanup.json',import.meta.url),JSON.stringify(report,null,2)+'\n');await save();for(let n=0;n<3;n++){try{report.versions=await deployments();break;}catch(e){report.receipt_attempt_errors??=[];report.receipt_attempt_errors.push(String(e));if(n<2)await new Promise(r=>setTimeout(r,1000));}}await save();if(!report.versions)process.exitCode=1;console.log(JSON.stringify({agents:rows.length,all_404:rows.every(r=>r.status===404),receipts:!!report.versions}));
