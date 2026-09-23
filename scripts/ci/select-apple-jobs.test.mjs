import assert from 'node:assert/strict';
import test from 'node:test';
import {selectAppleJobs} from './select-apple-jobs.mjs';
test('desktop and shared JS changes do not launch iPhone SDK builds',()=>{
  for(const path of ['js/nanocodex/cloudflare/workers-ai-responses.mjs','js/desktop-runtime/src/index.ts','macos/Nanocodex/App.swift','pnpm-lock.yaml'])
    assert.deepEqual(selectAppleJobs([path]),{macos:true,ios:false},path);
  for(const path of ['apple/NanocodexUI/Sources/View.swift','crates/nanocodex-voice-ffi/src/lib.rs','Cargo.lock','unknown'])
    assert.deepEqual(selectAppleJobs([path]),{macos:true,ios:true},path);
});
