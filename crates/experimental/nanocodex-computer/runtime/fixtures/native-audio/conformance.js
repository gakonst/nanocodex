const sourceId="org.skyre.rebuild.audiofixture.20260906";
const source=await cua.getApp(sourceId);
const rpc=(method,args={})=>{const r=JSON.parse(__skyre_rpc(method,JSON.stringify(args)));if(r.error)throw new Error(r.error.message);return r.result;};
const state=()=>rpc("get_app_state",{app:sourceId,disableDiffing:true});
function find(identifier,node=state().tree){if(node.identifier===identifier)return node;for(const child of node.children){const result=find(identifier,child);if(result)return result;}return undefined;}
let recording=false;
try {
  const started=rpc("audio.start",{app:sourceId,scope:"application",max_duration_ms:3000});recording=true;
  nodeRepl.write({diagnostic:"application audio start",scope:started.scope,pid:started.pid});
  if(started.scope!=="application")throw new Error("Recording scope was not application");
  await source.click(find("skyre.audio.play").id);
  if(!find("skyre.audio.status").value.startsWith("Playing owned"))throw new Error("Owned tone fixture did not start playback");
  await new Promise(resolve=>setTimeout(resolve,1400));
  const result=rpc("audio.stop");recording=false;
  const table="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const data=[];let accum=0,bits=0;for(const c of result.data.replace(/=+$/,"")){const n=table.indexOf(c);if(n<0)throw new Error("Malformed audio base64");accum=(accum<<6)|n;bits+=6;if(bits>=8){bits-=8;data.push((accum>>bits)&255);}}
  if(String.fromCharCode(...data.slice(0,4))!=="RIFF"||result.sample_rate!==24000||result.channels!==2||result.frames<=0||data.length!==44+result.frames*4)throw new Error("Malformed native WAV result");
  let nonzero=0,peak=0;const left=[];for(let i=44;i+1<data.length;i+=2){let value=data[i]|(data[i+1]<<8);if(value>=32768)value-=65536;if(value!==0)nonzero++;peak=Math.max(peak,Math.abs(value));if((i-44)%4===0)left.push(value);}
  if(nonzero===0)throw new Error("Native app-scoped recording contains no synthetic tone samples");
  const first=left.findIndex(value=>Math.abs(value)>peak/4);let last=left.length-1;while(last>first&&Math.abs(left[last])<=peak/4)last--;
  let crossings=0;for(let i=first+1;i<=last;i++)if(left[i-1]<=0&&left[i]>0)crossings++;
  const frequencyHz=crossings*24000/(last-first);
  if(!Number.isFinite(frequencyHz)||Math.abs(frequencyHz-440)>20)throw new Error("Captured signal does not match owned 440 Hz tone: "+frequencyHz);
  nodeRepl.write({case:"application-scoped synthetic audio WAV",pass:true,frames:result.frames,bytes:data.length,durationMs:result.duration_ms,nonzeroSamples:nonzero,peak,frequencyHz,scope:"application"});
  nodeRepl.write({artifact:"synthetic-application-audio.wav",mimeType:"audio/wav",scope:"application",sourceApp:sourceId,data:result.data});
} finally {
  if(recording){try{rpc("audio.stop");}catch(error){nodeRepl.write({cleanupError:String(error)});}}
  await source.click(find("skyre.audio.stop").id);
}
