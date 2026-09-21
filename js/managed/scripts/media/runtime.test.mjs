import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {after, before, test} from 'node:test';
import {Miniflare} from 'miniflare';
import {transform} from 'esbuild';
import {createComputerRuntime, createMediaCommands} from 'nanocodex-tools';
import {createWorkspace} from 'nanocodex-tools/workspace';

const root = new URL('../../', import.meta.url);
let mf;
let fixture;
before(async () => {
  fixture = await readFile(new URL('test-fixtures/media/color-bars.mov', root));
  const source = await readFile(new URL('src/media-runtime.ts', root), 'utf8');
  const compiled = await transform(source, {loader:'ts',format:'esm',target:'es2024'});
  const modules = [{type:'ESModule',path:'/media-runtime.js',contents:compiled.code}, {
    type:'ESModule',path:'/entry.js',contents:`
      import {createMediaExecutor} from './media-runtime.js';
      export default {async fetch(request,env){
        const f=await request.formData();
        const args=JSON.parse(f.get('args'));
        const file=f.get('input');
        try {
          const result=await createMediaExecutor(env.LOADER)({command:f.get('command'),args,
            files:file?[{path:'/input.mov',data:new Uint8Array(await file.arrayBuffer())}]:[]});
          const out=new FormData();
          out.set('result',JSON.stringify({...result,files:undefined}));
          for(const file of result.files)out.set('output',new Blob([file.data]),file.path);
          return new Response(out);
        }catch(error){return new Response(String(error),{status:500})}
      }};`,
  }];
  for(const program of ['ffmpeg','ffprobe']) {
    for(const [suffix,type] of [['js.txt','Text'],['wasm.bin','Data']]) {
      const path=`media/generated/${program}.${suffix}`;
      modules.push({type,path:`/${path}`,contents:await readFile(new URL(`src/${path}`,root),type==='Text'?'utf8':undefined)});
    }
  }
  modules.push({type:'Text',path:'/media/worker.js.txt',contents:await readFile(new URL('src/media/worker.js.txt',root),'utf8')});
  mf=new Miniflare({modulesRoot:'/',modules:[modules[1],modules[0],...modules.slice(2)],compatibilityDate:'2026-07-29',workerLoaders:{LOADER:{}},outboundService:()=>{throw new Error('Media attempted network access')}});
}, {timeout:30000});
after(async()=>{await mf?.dispose()});
async function run(command,args,input=fixture) {
  const body=new FormData();body.set('command',command);body.set('args',JSON.stringify(args));
  if(input)body.set('input',new Blob([input]),'input.mov');
  const serialized=new Response(body);
  const response=await mf.dispatchFetch('https://media.test/',{method:'POST',headers:Object.fromEntries(serialized.headers),body:await serialized.arrayBuffer()});
  assert.equal(response.status,200, response.status===200?'':await response.text());
  const form=await response.formData();
  return {...JSON.parse(form.get('result')),output:form.get('output')};
}
test('real workerd ffprobe reads H264/AAC MOV metadata',async()=>{
  const result=await run('ffprobe',['-v','error','-show_entries','format=duration:stream=codec_name,codec_type,width,height','-of','json','/input.mov']);
  assert.equal(result.exitCode,0,result.stderr);
  const metadata=JSON.parse(result.stdout);
  assert.ok(metadata.streams.some(s=>s.codec_name==='h264'&&s.width===160&&s.height===120));
  assert.ok(metadata.streams.some(s=>s.codec_name==='aac'));
  assert.equal(Number(metadata.format.duration),2);
});
test('real workerd ffmpeg decodes frames and makes a JPEG contact sheet',async()=>{
  const result=await run('ffmpeg',['-hide_banner','-loglevel','error','-y','-i','/input.mov','-vf','fps=1,scale=80:-1,tile=2x1','-frames:v','1','/output.jpg']);
  assert.equal(result.exitCode,0,result.stderr);
  const bytes=new Uint8Array(await result.output.arrayBuffer());
  assert.deepEqual([...bytes.slice(0,2)],[255,216]);
  assert.ok(bytes.length>1000);
});
test('real workerd ffmpeg extracts mono 16kHz PCM WAV',async()=>{
  const result=await run('ffmpeg',['-hide_banner','-loglevel','error','-y','-i','/input.mov','-vn','-ac','1','-ar','16000','/output.wav']);
  assert.equal(result.exitCode,0,result.stderr);
  const data=new Uint8Array(await result.output.arrayBuffer());
  assert.equal(new TextDecoder().decode(data.slice(0,4)),'RIFF');
  assert.equal(new TextDecoder().decode(data.slice(8,12)),'WAVE');
  assert.ok(data.length>60000&&data.length<70000);
});
test('corrupt media fails without a successful output file',async()=>{
  const result=await run('ffmpeg',['-y','-i','/input.mov','-frames:v','1','/output.jpg'],new TextEncoder().encode('not a movie'));
  assert.notEqual(result.exitCode,0);
  assert.equal(result.output,null);
});
test('independent commands have fresh memory and no prior input',async()=>{
  const version=await run('ffprobe',['-version'],null);
  assert.equal(version.exitCode,0);
  assert.match(version.stdout,/ffprobe version 5\.1\.10/);
  const missing=await run('ffprobe',['-v','error','/input.mov'],null);
  assert.notEqual(missing.exitCode,0);
});

test('Just Bash parses quoted paths, pipes probe JSON, and persists actual WASM output',async()=>{
  const files=new Map([['',null],['clip file.mov',new Uint8Array(fixture)]]);
  const workspace=createWorkspace({root:'/brain',backend:{
    async list(path,{recursive}){return [...files].filter(([name])=>name!==path&&(recursive?name.startsWith(path?path+'/':''):name.slice(0,Math.max(0,name.lastIndexOf('/')))===path)).map(([path,data])=>({path,kind:data===null?'directory':'file',...(data===null?{}:{size:data.length})}));},
    async readFile(path){return files.get(path).slice()},
    async writeFile(path,data){files.set(path,data.slice())},
    async mkdir(path){files.set(path,null)},async remove(path){files.delete(path)},
  }});
  const runtime=await createComputerRuntime({filesystem:workspace,fetch:()=>{throw new Error('unexpected network')},networkMode:'disabled',
    commands:({filesystem})=>createMediaCommands({filesystem,execute:async request=>{
      const response=await run(request.command,request.args,request.files[0]?.data??null);
      return {...response,files:response.output?[{path:request.args.at(-1),data:new Uint8Array(await response.output.arrayBuffer())}]:[]};
    }})});
  assert.ok(runtime.descriptor.commands.includes('ffmpeg'));
  assert.ok(runtime.descriptor.commands.includes('ffprobe'));
  const context={signal:new AbortController().signal};
  const probe=await runtime.tool.handler({cmd:`ffprobe -v error -show_entries format=duration -of json 'clip file.mov' | jq -r '.format.duration'`},context);
  assert.equal(probe.exit_code,0,probe.output);
  assert.match(probe.output,/2\.000000/);
  const convert=await runtime.tool.handler({cmd:`ffmpeg -hide_banner -loglevel error -i 'clip file.mov' -vf 'fps=1,scale=80:-1,tile=2x1' -frames:v 1 sheet.jpg`},context);
  assert.equal(convert.exit_code,0,convert.output);
  assert.deepEqual([...files.get('sheet.jpg').slice(0,2)],[255,216]);
});
if(process.env.MEDIA_SMOKE_FILE) test('screen-sized clip finishes within the inspection envelope',async()=>{
  const input=await readFile(process.env.MEDIA_SMOKE_FILE);
  const start=performance.now();
  const result=await run('ffmpeg',['-hide_banner','-loglevel','error','-y','-i','/input.mov','-vf','fps=1/2,scale=643:-1,tile=3x2','-frames:v','1','/output.jpg'],input);
  assert.equal(result.exitCode,0,result.stderr);
  const bytes=new Uint8Array(await result.output.arrayBuffer());
  assert.deepEqual([...bytes.slice(0,2)],[255,216]);
  console.log(JSON.stringify({input_bytes:input.length,output_bytes:bytes.length,wall_ms:Math.round(performance.now()-start)}));
});

test('WASM output writes stop at 16 MiB and return failure without partial output',async()=>{
  const result=await run('ffmpeg',['-hide_banner','-loglevel','error','-y','-i','/input.mov','-vn','-ac','8','-ar','768000','/output.wav']);
  assert.notEqual(result.exitCode,0,result.stderr);
  assert.equal(result.output,null);
  assert.match(result.stderr,/too large|Error writing|Error closing|I\/O error/i);
});
