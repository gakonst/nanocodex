import assert from 'node:assert/strict';
import test from 'node:test';
import {mkdtempSync,mkdirSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname,join} from 'node:path';
import {execFileSync} from 'node:child_process';
import {recoverImages} from './recover-images.mjs';
import {fingerprint} from './managed-images.mjs';

test('locked replan retains cache-only receipts and publishes only genuinely missing images',async t=>{
 const cwd=mkdtempSync(join(tmpdir(),'recover-images-'));t.after(()=>rmSync(cwd,{recursive:true,force:true}));
 const git=(...args)=>execFileSync('git',args,{cwd,stdio:'pipe'});
 const put=(path,value)=>{mkdirSync(dirname(join(cwd,path)),{recursive:true});writeFileSync(join(cwd,path),value);};
 git('init','-q');put('Cargo.toml','[workspace]');put('crates/phone/Cargo.toml','[package]\nname="nanocodex-phone"');put('crates/hand/Cargo.toml','[package]\nname="nanocodex2-bin"');
 git('add','.');git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','fixture');
 const account='a'.repeat(32),retained=[],published=[];let configured=0;
 const receipt=image=>({version:1,image,input:fingerprint(image,account,'1',cwd),ref:`registry.cloudflare.com/${account}/nanocodex-ci-${image}@sha256:${'b'.repeat(64)}`});
 const write=image=>put(`.ci-images/${image}.json`,JSON.stringify(receipt(image)));
 write('phone');write('sandbox');
 const options={cwd,account,store:{retain:async row=>retained.push(row.image)},publish:async image=>{published.push(image);write(image);},configure:()=>configured++};
 await recoverImages(options);assert.deepEqual(published,[]);assert.deepEqual(retained,['phone','sandbox']);assert.equal(configured,1);
 rmSync(join(cwd,'.ci-images/sandbox.json'));retained.length=0;
 await recoverImages(options);assert.deepEqual(published,['sandbox']);assert.deepEqual(retained,['phone','sandbox']);assert.equal(configured,2);
 rmSync(join(cwd,'.ci-images/phone.json'));retained.length=0;
 await assert.rejects(recoverImages({...options,publish:async()=>{throw Error('image publication failed');}}),/publication failed/);
 assert.deepEqual(retained,[]);assert.equal(configured,2);
});
