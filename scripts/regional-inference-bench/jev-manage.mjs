import fs from 'node:fs/promises';
import path from 'node:path';
import {spawn} from 'node:child_process';
const [action,runnersFile,keyFile,approval]=process.argv.slice(2);
if(!['dry-run','deploy','delete'].includes(action)||!runnersFile)throw Error('usage: jev-manage.mjs dry-run|deploy|delete PRIVATE_RUNNERS_JSON [PRIVATE_KEYS_JSON] [--approved]');
if(action==='deploy'&&approval!=='--approved')throw Error('parent_review_required_before_deploy');
if(action==='delete'&&keyFile!=='--approved')throw Error('review_required_before_delete');
const runners=JSON.parse(await fs.readFile(runnersFile,'utf8'));
const keys=action==='deploy'?JSON.parse(await fs.readFile(keyFile,'utf8')).keys:null;
const logdir=path.join(path.dirname(runnersFile),'logs');await fs.mkdir(logdir,{recursive:true,mode:0o700});
const wrangler=path.resolve('js/managed/node_modules/.bin/wrangler');
for(const r of runners){
 const config=JSON.parse(await fs.readFile(r.config,'utf8'));
 if(!/^nanocodex-jev-(us|eu|ap)-20260921$/.test(r.id)||config.name!==r.id)throw Error('not_isolated_experiment');
 async function command(args,input,suffix){
  return new Promise((resolve,reject)=>{
   let output='';const child=spawn(wrangler,args,{stdio:['pipe','pipe','pipe'],env:{...process.env,WRANGLER_SEND_METRICS:'false'}});
   child.stdout.on('data',x=>output+=x);child.stderr.on('data',x=>output+=x);child.on('error',reject);child.stdin.end(input);
   child.on('close',async code=>{try{
    for(const secret of [keys?.[r.key_index]?.api_key,await fs.readFile(r.tokenfile,'utf8')])if(secret)output=output.replaceAll(secret,'[REDACTED]');
    await fs.writeFile(path.join(logdir,`${r.id}-${suffix}.log`),output,{mode:0o600});
    code===0?resolve():reject(Error(`wrangler_failed_${r.id}_${suffix}; inspect private log; do not automatically retry`));
   }catch(e){reject(e);}});
  });
 }
 if(action==='dry-run')await command(['deploy','--dry-run','--config',r.config,'--outdir',path.join(path.dirname(runnersFile),'bundles',r.id)],undefined,'dry-run');
 if(action==='deploy'){
  if(Date.now()>Date.parse(config.vars.EXPIRES_AT))throw Error('prepared_config_expired');
  await command(['deploy','--config',r.config],undefined,'deploy');
  await command(['secret','bulk','--config',r.config],JSON.stringify({BENCH_TOKEN:await fs.readFile(r.tokenfile,'utf8'),INFERENCE_KEY:keys[r.key_index].api_key}),'secrets');
 }
 if(action==='delete')await command(['delete','--config',r.config,'--force'],undefined,'delete');
 console.log(JSON.stringify({action,runner:r.id,success:true}));
}
