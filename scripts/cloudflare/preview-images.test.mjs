import assert from 'node:assert/strict';
import test from 'node:test';
import {mkdtempSync,mkdirSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname,join} from 'node:path';
import {execFileSync} from 'node:child_process';
import {changedImages} from './preview-images.mjs';

test('preview image selection uses committed image inputs, not Worker or SDK JavaScript',t=>{
 const cwd=mkdtempSync(join(tmpdir(),'preview-images-')); t.after(()=>rmSync(cwd,{recursive:true,force:true}));
 const git=(...args)=>execFileSync('git',args,{cwd,encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
 const put=(path,content)=>{mkdirSync(dirname(join(cwd,path)),{recursive:true});writeFileSync(join(cwd,path),content);};
 const commit=()=>{git('add','.');git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','fixture');return git('rev-parse','HEAD');};
 git('init','-q');put('Cargo.toml','[workspace]');
 put('crates/phone/Cargo.toml','[package]\nname="nanocodex-phone"');put('crates/hand/Cargo.toml','[package]\nname="nanocodex2-bin"');
 const base=commit(),account='a'.repeat(32);
 put('js/managed/src/index.ts','export {};');put('js/nanocodex/cloudflare/provider.mjs','export {};');commit();
 assert.deepEqual(changedImages({base,account,cwd}),[]);
 put('js/managed/scripts/phone-bridge.mjs','export {};');const phone=commit();
 assert.deepEqual(changedImages({base,account,cwd}),['phone']);
 put('hands/remote/image/labwc/config','desktop');commit();
 assert.deepEqual(changedImages({base:phone,account,cwd}),['sandbox']);
 assert.deepEqual(changedImages({base:'missing',account,cwd}),['phone','sandbox']);
});
