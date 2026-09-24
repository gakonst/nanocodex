import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {after, before, test} from 'node:test';
import {Miniflare} from 'miniflare';
import {build, transform} from 'esbuild';
import {createComputerRuntime, createMediaCommands} from 'nanocodex-tools';
import {createWorkspace} from 'nanocodex-tools/workspace';

const root = new URL('../../', import.meta.url);
const mediaRoot = new URL('../../../media/', import.meta.url);
let mf;
let mediaMf;
let mediaModules;
let fixture;
before(async () => {
  fixture = await readFile(new URL('test-fixtures/media/color-bars.mov', root));
  const clientSource = await readFile(new URL('src/media-runtime.ts', root), 'utf8');
  const client = await transform(clientSource, {loader:'ts',format:'esm',target:'es2024'});
  const sidecarSource = await readFile(new URL('src/index.ts',mediaRoot), 'utf8');
  const sidecar = await transform(sidecarSource.replace('"./media-runtime"','"./media-runtime.js"')
    .replace('"nanocodex-tools/pdf-extract"','"./pdf-extract.js"')
    .replace('export default { fetch(): Response { return new Response("Not found", { status: 404 }); } };',
      'export default {fetch(request,env){return new URL(request.url).pathname==="/pdf-text"?runPdfTextRequest(request):runMediaRequest(request,env.LOADER)}};'), {loader:'ts',format:'esm',target:'es2024'});
  const runtimeSource = await readFile(new URL('src/media-runtime.ts',mediaRoot), 'utf8');
  const runtime = await transform(runtimeSource, {loader:'ts',format:'esm',target:'es2024'});
  const managed = [{type:'ESModule',path:'/entry.js',contents:`
      import {createMediaExecutor} from './client.js';
      export default {async fetch(request,env){
        const f=await request.formData();
        const args=JSON.parse(f.get('args'));
        const file=f.get('input');
        try {
          const result=await createMediaExecutor(env.MEDIA)({command:f.get('command'),args,
            files:file?[{path:'/input.mov',data:new Uint8Array(await file.arrayBuffer())}]:[]});
          const out=new FormData();
          out.set('result',JSON.stringify({...result,files:undefined}));
          for(const file of result.files)out.set('output',new Blob([file.data]),file.path);
          return new Response(out);
        }catch(error){return new Response(String(error),{status:500})}
      }};`,
  }, {type:'ESModule',path:'/client.js',contents:client.code}];
  const pdfBundle=await build({entryPoints:[fileURLToPath(new URL('../../../nanocodex-tools/src/pdf-extract.ts',import.meta.url))],
    bundle:true,format:'esm',platform:'browser',target:'es2024',write:false});
  const modules = [{type:'ESModule',path:'/index.js',contents:sidecar.code},
    {type:'ESModule',path:'/media-runtime.js',contents:runtime.code},
    {type:'ESModule',path:'/pdf-extract.js',contents:pdfBundle.outputFiles[0].text}];
  for(const program of ['ffmpeg','ffprobe']) {
    for(const [suffix,type] of [['js.txt','Text'],['wasm.bin','Data']]) {
      const path=`media/generated/${program}.${suffix}`;
      modules.push({type,path:`/${path}`,contents:await readFile(new URL(`src/${path}`,mediaRoot),type==='Text'?'utf8':undefined)});
    }
  }
  modules.push({type:'Text',path:'/media/worker.js.txt',contents:await readFile(new URL('src/media/worker.js.txt',mediaRoot),'utf8')});
  mediaModules=modules;
  mediaMf=new Miniflare({modulesRoot:'/',modules,compatibilityDate:'2026-07-29',
    compatibilityFlags:['enable_request_signal'],workerLoaders:{LOADER:{}},
    outboundService:()=>{throw new Error('Media attempted network access')}});
  mf=new Miniflare({modulesRoot:'/',modules:managed,compatibilityDate:'2026-07-29',
    compatibilityFlags:['enable_request_signal'],serviceBindings:{MEDIA: request=>mediaMf.dispatchFetch(request)},
    outboundService:()=>{throw new Error('Media attempted network access')}});
}, {timeout:30000});
after(async()=>{await Promise.all([mf?.dispose(),mediaMf?.dispose()])});
async function run(command,args,input=fixture) {
  const body=new FormData();body.set('command',command);body.set('args',JSON.stringify(args));
  if(input)body.set('input',new Blob([input]),'input.mov');
  const serialized=new Response(body);
  const response=await mf.dispatchFetch('http://localhost/',{method:'POST',headers:Object.fromEntries(serialized.headers),body:await serialized.arrayBuffer()});
  assert.equal(response.status,200, response.status===200?'':await response.text());
  const form=await response.formData();
  return {...JSON.parse(form.get('result')),output:form.get('output')};
}
test('sidecar alone owns WASM assets; media loader has no network or custom limits',async()=>{
  const compiled=await build({
    entryPoints:[fileURLToPath(new URL('src/media-runtime.ts',mediaRoot))],
    bundle:true,format:'esm',write:false,
    plugins:[{name:'stub-media-assets',setup(builder){
      builder.onResolve({filter:/\.(?:txt|bin)$/},args=>({path:args.path,namespace:'media-assets'}));
      builder.onLoad({filter:/.*/,namespace:'media-assets'},()=>({contents:'export default "";',loader:'js'}));
    }}],
  });
  const {createMediaExecutor}=await import(`data:text/javascript;base64,${Buffer.from(compiled.outputFiles[0].contents).toString('base64')}`);
  const configurations=[];
  const execute=createMediaExecutor({load(configuration){
    configurations.push(configuration);
    return {getEntrypoint(){return {async fetch(){
      const result=new FormData();
      result.set('result',JSON.stringify({stdout:'version\n',stderr:'',exitCode:0}));
      return new Response(result);
    }}}};
  }});
  for(const command of ['ffmpeg','ffprobe']) {
    assert.deepEqual(await execute({command,args:['-version'],files:[]}),{stdout:'version\n',stderr:'',exitCode:0,files:[]});
  }
  assert.equal(configurations.length,2);
  for(const configuration of configurations) {
    assert.equal(Object.hasOwn(configuration,'limits'),false);
    assert.equal(configuration.globalOutbound,null);
  }
  const client=await readFile(new URL('src/media-runtime.ts',root),'utf8');
  assert.doesNotMatch(client,/\.(?:wasm\.bin|js\.txt)|workerSource|loader\.load/);
});

test('private media service extracts compressed Unicode PDF without egress',async()=>{
  const {pdf}=await import('../../../nanocodex-tools/test/fixtures/pdf.mjs');
  const body=new FormData();
  body.set('input',new Blob([pdf()]),'input.pdf');
  body.set('options',JSON.stringify({first:2,layout:false,raw:false,pageBreaks:false}));
  const service=await mediaMf.getWorker();
  const serialized=new Response(body);
  const response=await service.fetch('http://localhost/pdf-text',{method:'POST',headers:Object.fromEntries(serialized.headers),body:await serialized.arrayBuffer()});
  assert.equal(response.status,200,await response.clone().text());
  assert.equal(await response.text(),'Page two: Ω\n');
  body.set('options',JSON.stringify({first:0,layout:false,raw:false,pageBreaks:false}));
  const invalid=new Response(body);
  assert.equal((await service.fetch('http://localhost/pdf-text',{method:'POST',headers:Object.fromEntries(invalid.headers),body:await invalid.arrayBuffer()})).status,400);
});

test('production media default fetch cannot expose private PDF extraction',async()=>{
  const source=await readFile(new URL('src/index.ts',mediaRoot),'utf8');
  const transformed=await transform(source.replace('"./media-runtime"','"./media-runtime.js"')
    .replace('"nanocodex-tools/pdf-extract"','"./pdf-extract.js"'),
    {loader:'ts',format:'esm',target:'es2024'});
  const publicMf=new Miniflare({modulesRoot:'/',
    modules:[{type:'ESModule',path:'/index.js',contents:transformed.code},...mediaModules.slice(1)],
    compatibilityDate:'2026-07-29',compatibilityFlags:['enable_request_signal'],
    workerLoaders:{LOADER:{}},outboundService:()=>{throw new Error('Media attempted network access')},
  });
  try {
    const response=await publicMf.dispatchFetch('http://localhost/pdf-text',{method:'POST'});
    assert.equal(response.status,404);
  } finally { await publicMf.dispose(); }
});

test('sidecar rejects unsupported requests',async()=>{
  const service=await mediaMf.getWorker();
  assert.equal((await service.fetch('http://localhost/not-run',{method:'POST'})).status,404);
  const body=new FormData();body.set('command','invalid');body.set('args','["-version"]');
  assert.equal((await service.fetch('https://media.internal/run',{method:'POST',body})).status,400);
  body.set('command','ffmpeg');body.set('args','not JSON');
  assert.equal((await service.fetch('https://media.internal/run',{method:'POST',body})).status,400);
});

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

test('real workerd generates and probes a WAV larger than 16 MiB',async()=>{
  const result=await run('ffmpeg',['-hide_banner','-loglevel','error','-y','-i','/input.mov','-vn','-t','2','-ac','8','-ar','576000','/output.wav']);
  assert.equal(result.exitCode,0,result.stderr);
  const data=new Uint8Array(await result.output.arrayBuffer());
  assert.ok(data.length>16*1024*1024,`WAV contains ${data.length} bytes`);
  assert.equal(new TextDecoder().decode(data.slice(0,4)),'RIFF');
  assert.equal(new TextDecoder().decode(data.slice(8,12)),'WAVE');
  const probe=await run('ffprobe',['-v','error','-show_entries','format=size,duration:stream=codec_name,sample_rate,channels','-of','json','/input.mov'],data);
  assert.equal(probe.exitCode,0,probe.stderr);
  const metadata=JSON.parse(probe.stdout);
  assert.equal(Number(metadata.format.size),data.length);
  assert.equal(Number(metadata.format.duration),2);
  assert.equal(metadata.streams[0].codec_name,'pcm_s16le');
  assert.equal(Number(metadata.streams[0].sample_rate),576000);
  assert.equal(metadata.streams[0].channels,8);
});

test('real workerd preserves diagnostics beyond 64 KiB with long valid probe options',async()=>{
  // Alternating fields prevent FFmpeg's repeated-log suppression from hiding diagnostics.
  const entries=`format=${Array(600).fill('duration,size').join(',')}`;
  const options=Array.from({length:65},(_,index)=>['-show_entries',index<2?entries:'format=duration,size']).flat();
  const result=await run('ffprobe',['-v','debug',...options,'-of','json','/input.mov']);
  assert.equal(result.exitCode,0,result.stderr);
  assert.ok(Buffer.byteLength(result.stderr)>65536);
  assert.equal(result.stderr.match(/Adding 'duration' to the entries to show in section 'format'/g)?.length,1263);
  assert.equal(result.stderr.match(/Adding 'size' to the entries to show in section 'format'/g)?.length,1263);
  assert.equal(Number(JSON.parse(result.stdout).format.duration),2);
});
