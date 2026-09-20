"""Isolated synthetic Go/Rust publisher WebRTC benchmark; run inside perf container."""
import asyncio,json,os,pathlib,subprocess,time,argparse,array,math,uuid,sys,hashlib
from aiohttp import web
from playwright.async_api import async_playwright
ROOT=pathlib.Path('/perf'); ROOT.mkdir(exist_ok=True)
TARGET='''<html><body style="margin:0;background:black"><canvas id="c" width="1280" height="720"></canvas><script>
let state=0;onkeydown=e=>{if(e.code==='Space')state^=1};let ctx=c.getContext('2d');function draw(t){ctx.fillStyle=state?'white':'black';ctx.fillRect(0,0,1280,720);ctx.fillStyle='red';ctx.fillRect((t/5)%1200,680,80,40);requestAnimationFrame(draw)}requestAnimationFrame(draw);
let a=new AudioContext(),o=a.createOscillator(),g=a.createGain();g.gain.value=.02;o.frequency.value=440;o.connect(g).connect(a.destination);o.start();a.resume();
</script>'''
VIEWER='''<video id="v" autoplay playsinline></video><canvas id="c" width="1" height="1"></canvas><script>
window.viewerStarted=performance.now();window.firstVideoAt=null;window.samples=[];window.errors=[];let pc=new RTCPeerConnection({iceServers:[]}),ws=new WebSocket('ws://127.0.0.1:8769/viewer'),dc,gen,seq=0,pending=null,want=0,queue=[];let ctx=c.getContext('2d',{willReadFrequently:true});window.pc=pc;
pc.ontrack=e=>{if(e.track.kind==='video'){v.srcObject=new MediaStream([e.track]);v.play();}else{let a=new Audio();a.srcObject=new MediaStream([e.track]);a.autoplay=true;document.body.append(a)}};
pc.onicecandidate=e=>{if(e.candidate)ws.send(JSON.stringify({type:'signal',viewer_id:'bench',signal:{type:'candidate',...e.candidate.toJSON()}}))};
pc.ondatachannel=e=>{if(e.channel.label==='remote-control-v1'){dc=e.channel;dc.onopen=()=>dc.send(JSON.stringify({type:'acquire'}));dc.onmessage=e=>{let m=JSON.parse(e.data);if(m.type==='granted'){gen=m.generation;window.granted=true;setInterval(()=>dc.send(JSON.stringify({type:'renew',generation:gen})),3000)}}}};
ws.onmessage=async e=>{try{let m=JSON.parse(e.data),s=m.signal;if(!s)return;if(s.type==='offer'){await pc.setRemoteDescription(s);await pc.setLocalDescription(await pc.createAnswer());ws.send(JSON.stringify({type:'signal',viewer_id:'bench',signal:{type:'answer',sdp:pc.localDescription.sdp}}));for(let q of queue)await pc.addIceCandidate(q);queue=[]}else if(s.type==='candidate'){if(pc.remoteDescription)await pc.addIceCandidate(s);else queue.push(s)}}catch(e){errors.push(String(e))}};
function frame(now,meta){if(firstVideoAt===null)firstVideoAt=performance.now();ctx.drawImage(v,v.videoWidth/2,v.videoHeight/2,1,1,0,0,1,1);let bright=ctx.getImageData(0,0,1,1).data[0]>127;if(pending&&bright===!!want){samples.push({latency_ms:performance.now()-pending,expected_display_ms:meta.expectedDisplayTime-pending,media_time:meta.mediaTime});pending=null}v.requestVideoFrameCallback(frame)}v.requestVideoFrameCallback(frame);
window.sendInput=(input)=>dc.send(JSON.stringify({...input,sequence:++seq,generation:gen}));window.release=()=>dc.send(JSON.stringify({type:'release',generation:gen}));
window.fire=()=>{if(pending)return false;want^=1;pending=performance.now();for(let down of [true,false])dc.send(JSON.stringify({kind:'key',key:44,down,sequence:++seq,generation:gen}));return true};
</script>'''

# Only injected for --microphone: no getUserMedia and no physical capture device.
MICROPHONE_JS=r"""
window.micAcks=[];window.micGrant=null;const micWaiters=new Map();
window.micMessage=m=>{m.received_ms=performance.now();if(m.type==='granted')window.micGrant=m;if(m.type==='microphone'){micAcks.push(m);const key=m.generation+':'+m.requestID,w=micWaiters.get(key);if(w){clearTimeout(w.timer);micWaiters.delete(key);w.resolve(m)}}};
window.micRequest=enabled=>new Promise((resolve,reject)=>{const generation=gen,requestID=crypto.randomUUID(),key=generation+':'+requestID;const timer=setTimeout(()=>{micWaiters.delete(key);reject(new Error('microphone ACK timeout: '+requestID))},10000);micWaiters.set(key,{resolve,reject,timer});dc.send(JSON.stringify({type:'microphone',generation,requestID,enabled}))});
let micAudio,micOsc,micTrack,micSender;
window.startSyntheticMic=async()=>{if(!micGrant?.microphone)throw new Error('host did not grant microphone capability');const t=pc.getTransceivers().find(t=>t.receiver.track.kind==='audio'&&t.currentDirection==='sendrecv');if(!t)throw new Error('no negotiated sendrecv audio transceiver');micSender=t.sender;micAudio=new AudioContext({sampleRate:48000});const destination=micAudio.createMediaStreamDestination(),gain=micAudio.createGain();gain.gain.value=.2;micOsc=micAudio.createOscillator();micOsc.frequency.value=660;micOsc.connect(gain).connect(destination);micTrack=destination.stream.getAudioTracks()[0];micOsc.start();await micAudio.resume();await micSender.replaceTrack(micTrack);return {frequency_hz:660,context_state:micAudio.state,direction:t.currentDirection,track_kind:micTrack.kind}};
window.stopSyntheticMic=async()=>{if(micSender)await micSender.replaceTrack(null);if(micOsc)micOsc.stop();if(micTrack)micTrack.stop();if(micAudio)await micAudio.close()};
"""
def audio_defaults(env):
 return {kind:subprocess.check_output(['pactl','get-default-'+kind],env=env,text=True,timeout=5).strip() for kind in ['sink','source']}
async def sample_microphone(env,source):
 # Each fresh recording discards 0.5 s of startup and measures 1 s of signed PCM.
 q=await asyncio.create_subprocess_exec('parec','--record','--raw','--format=s16le','--rate=48000','--channels=1','--latency-msec=20','--device='+source,env=env,stdout=asyncio.subprocess.PIPE,stderr=asyncio.subprocess.PIPE)
 try:
  raw=await asyncio.wait_for(q.stdout.readexactly(48000*2*3//2),8)
 finally:
  if q.returncode is None:q.terminate()
  await q.communicate()
 pcm=array.array('h',raw[48000:])
 if sys.byteorder!='little':pcm.byteswap()
 values=[x/32768 for x in pcm]
 rms=math.sqrt(sum(x*x for x in values)/len(values))
 # Coherent tone projection distinguishes injected 660 Hz from host 440 Hz playback.
 def tone(hz):
  re=sum(x*math.cos(2*math.pi*hz*i/48000) for i,x in enumerate(values))
  im=sum(x*math.sin(2*math.pi*hz*i/48000) for i,x in enumerate(values))
  return 2*math.hypot(re,im)/len(values)
 return {'samples':len(values),'rms':rms,'peak':max(abs(x) for x in values),'amplitude_660hz':tone(660),'amplitude_440hz':tone(440)}
async def verify_microphone(page,env,out,defaults_before):
 source='nanocodex_remote_mic_'+uuid.uuid5(uuid.NAMESPACE_OID,'synthetic-perf').hex+'_source'
 out.update(source=source,defaults_before=defaults_before,scope='Synthetic browser 660Hz -> WebRTC Opus -> isolated host virtual PulseAudio source; no physical microphone or native app UI')
 sources=subprocess.check_output(['pactl','--format=json','list','sources'],env=env,text=True,timeout=5)
 assert any(s['name']==source for s in json.loads(sources)), 'stable virtual source missing before enable'
 out['source_present_before_enable']=True
 try:
  out['generator']=await page.evaluate('startSyntheticMic()')
  await asyncio.sleep(1)
  out['before_enable']=await sample_microphone(env,source)
  out['enable_ack']=await page.evaluate('micRequest(true)')
  assert out['enable_ack']['enabled'] is True, out['enable_ack']
  await asyncio.sleep(6)
  out['enabled']=await sample_microphone(env,source)
  out['mute_ack']=await page.evaluate('micRequest(false)')
  assert out['mute_ack']['enabled'] is False, out['mute_ack']
  await asyncio.sleep(1)
  out['after_mute']=await sample_microphone(env,source)
  out['defaults_after']=audio_defaults(env)
  out['acks']=await page.evaluate('micAcks')
  out['audio_sender_stats']=await page.evaluate("async()=>Array.from((await pc.getStats()).values()).filter(s=>s.type==='outbound-rtp'&&s.kind==='audio')")
  before,on,off=(out[k] for k in ['before_enable','enabled','after_mute'])
  out['passed']=(not any(a.get('requestID')==out['enable_ack']['requestID'] and a.get('enabled') is False for a in out['acks']) and before['rms']<.001 and on['rms']>.01 and on['amplitude_660hz']>.01 and on['amplitude_660hz']>5*on['amplitude_440hz'] and off['rms']<.001 and off['rms']<on['rms']*.05 and out['defaults_after']['sink']==defaults_before['sink'] and (out['defaults_after']['source']==defaults_before['source'] or (defaults_before['source']=='perf.monitor' and out['defaults_after']['source']==source)))
  assert out['passed'], 'microphone PCM or default-device verification failed'
 finally:
  # Muting on failure is best effort; publisher cleanup also revokes the lease.
  try:await page.evaluate('micRequest(false)')
  except Exception:pass
  await page.evaluate('stopSyntheticMic()')

async def main():
 p=argparse.ArgumentParser();p.add_argument('--publisher',default='/usr/local/bin/nanocodex-remote');p.add_argument('--label',default='go');p.add_argument('--synthetic',action='store_true');p.add_argument('--controls',action='store_true');p.add_argument('--microphone',action='store_true',help='Verify synthetic browser tone through host virtual microphone (Rust only)');args=p.parse_args()
 env=os.environ.copy();env.update(XDG_RUNTIME_DIR='/tmp/perf-runtime',WAYLAND_DISPLAY='wayland-0',WLR_BACKENDS='headless',WLR_RENDERER='pixman',WLR_HEADLESS_OUTPUTS='1',XDG_SESSION_TYPE='wayland',NANOCODEX_SCREEN_BITRATE_KBPS='6000',NANOCODEX_VIDEO_ENCODER='software')
 os.makedirs(env['XDG_RUNTIME_DIR'],mode=0o700,exist_ok=True)
 procs=[]
 def launch(name,cmd):
  f=open(ROOT/(name+'.log'),'w');q=subprocess.Popen(cmd,env=env,stdout=f,stderr=subprocess.STDOUT,start_new_session=True);procs.append(q);return q
 host=None;viewer=None;catalog=asyncio.Event()
 async def ice(r):return web.json_response({'iceServers':[]})
 async def renew(r):
  if host is not None:await host.send_json({'type':'renewed'})
  return web.json_response({})
 async def host_ws(r):
  nonlocal host
  host=web.WebSocketResponse();await host.prepare(r);await host.send_json({'type':'ready','connection_id':'local-benchmark'})
  async for msg in host:
   if msg.type!=web.WSMsgType.TEXT:continue
   m=json.loads(msg.data)
   with open(ROOT/'signals.jsonl','a') as f:f.write(json.dumps({'type':m.get('type'),'signal':m.get('signal',{}).get('type')})+'\n')
   if m['type']=='catalog':
    await host.send_json({'type':'published','generation':'local-publication'});catalog.set()
   elif viewer is not None:await viewer.send_json(m)
  return host
 async def viewer_ws(r):
  nonlocal viewer
  viewer=web.WebSocketResponse();await viewer.prepare(r);await catalog.wait();await host.send_json({'type':'viewer','viewer_id':'bench','surface_id':'desktop'})
  async for msg in viewer:
   if msg.type==web.WSMsgType.TEXT:await host.send_str(msg.data)
  return viewer
 viewer_html=VIEWER
 if args.microphone:
  viewer_html=viewer_html.replace("await pc.setRemoteDescription(s);await pc.setLocalDescription","await pc.setRemoteDescription(s);for(const t of pc.getTransceivers()){if(t.receiver.track.kind==='audio')t.direction='sendrecv'}await pc.setLocalDescription")
  viewer_html=viewer_html.replace("let m=JSON.parse(e.data);if(m.type==='granted')","let m=JSON.parse(e.data);if(window.micMessage)window.micMessage(m);if(m.type==='granted')")
  viewer_html=viewer_html.replace("let m=JSON.parse(e.data),s=m.signal;if(!s)return","let m=JSON.parse(e.data),s=m.signal;if(window.micMessage&&m.type==='control')window.micMessage(m.data);if(!s)return")
  viewer_html=viewer_html.replace('</script>',MICROPHONE_JS+'</script>')
 app=web.Application();app.router.add_post('/v1/account/hands/ice',ice);app.router.add_post('/v1/account/hands/renew',renew);app.router.add_get('/v1/account/hands/host',host_ws);app.router.add_get('/viewer',viewer_ws)
 app.router.add_get('/target',lambda r:web.Response(text=TARGET,content_type='text/html'));app.router.add_get('/',lambda r:web.Response(text=viewer_html,content_type='text/html'))
 runner=web.AppRunner(app);await runner.setup();await web.TCPSite(runner,'127.0.0.1',8769).start()
 launch('pulse',['pulseaudio','--daemonize=no','--exit-idle-time=-1']);await asyncio.sleep(1)
 subprocess.run(['pactl','load-module','module-null-sink','sink_name=perf'],env=env,stdout=subprocess.DEVNULL)
 launch('tone',['sh','-c','ffmpeg -hide_banner -loglevel error -re -f lavfi -i sine=frequency=440:sample_rate=48000 -ac 2 -f s16le - | pacat --playback --raw --format=s16le --rate=48000 --channels=2 --latency-msec=20 --device=perf'])
 launch('labwc',['labwc','--config-dir','/perf/labwc']);await asyncio.sleep(2)
 subprocess.run(['wlr-randr','--output','HEADLESS-1','--custom-mode','1280x720@60Hz'],env=env)
 if not args.synthetic:
  launch('target',['google-chrome','--no-sandbox','--disable-dev-shm-usage','--disable-gpu','--ozone-platform=wayland','--kiosk','--no-first-run','--user-data-dir=/tmp/perf-target','--autoplay-policy=no-user-gesture-required','http://127.0.0.1:8769/target'])
  await asyncio.sleep(3)
 audio_defaults_before=audio_defaults(env) if args.microphone else None
 cred=ROOT/'synthetic-credential';cred.write_text('synthetic-local-benchmark');cred.chmod(0o600)
 launch('publisher',[args.publisher,'wayland-host','--url','http://127.0.0.1:8769','--credential-file',str(cred),'--machine-id','synthetic-perf','--width','1280','--height','720','--include-loopback']+(['--waymote','/perf/synthetic-waymote.py'] if args.synthetic else []))
 result={'label':args.label,'publisher_sha256':hashlib.sha256(pathlib.Path(args.publisher).read_bytes()).hexdigest(),'harness_sha256':hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),'source_sha256':hashlib.sha256((ROOT/'synthetic-waymote.py').read_bytes()).hexdigest(),'requested':{'width':1280,'height':720,'fps':60,'bitrate_kbps':6000,'cpus':2,'memory_gib':4},'host_arch':'amd64 emulation on arm64 Docker host','synthetic_source':args.synthetic}
 try:
  await asyncio.wait_for(catalog.wait(),40)
  async with async_playwright() as pw:
   browser=await pw.chromium.launch(executable_path='/usr/bin/google-chrome',headless=True,args=['--no-sandbox','--disable-dev-shm-usage','--autoplay-policy=no-user-gesture-required'])
   page=await browser.new_page();await page.goto('http://127.0.0.1:8769/')
   await page.wait_for_function('window.granted && v.videoWidth>0',timeout=45000);await asyncio.sleep(2)
   for i in range(30):
    await page.evaluate('fire()');await asyncio.sleep(.4)
   result.update(await page.evaluate('async()=>({samples,errors,first_video_ms:firstVideoAt-viewerStarted,width:v.videoWidth,height:v.videoHeight,stats:Array.from((await pc.getStats()).values()).filter(s=>s.type==="inbound-rtp"||s.type==="candidate-pair")})'))
   vals=sorted(s['latency_ms'] for s in result['samples']);result['summary']={'n':len(vals),'p50_ms':vals[len(vals)//2] if vals else None,'p95_ms':vals[int(len(vals)*.95)] if vals else None}
   if args.microphone:
    result['microphone_checks']={}
    await verify_microphone(page,env,result['microphone_checks'],audio_defaults_before)
   if args.controls and args.synthetic:
    async def held(expected_buttons, expected_keys=None):
     for _ in range(100):
      try:
       value=json.loads((ROOT/'held-input.json').read_text())
       if value['buttons']==expected_buttons and (expected_keys is None or value['keys']==expected_keys):return value
      except (FileNotFoundError,json.JSONDecodeError):pass
      await asyncio.sleep(.02)
     raise AssertionError(f'held input mismatch: {expected_buttons}, {expected_keys}, {value}')
    await page.evaluate("sendInput({kind:'button',button:0,down:true,x:.5,y:.5});sendInput({kind:'button',button:1,down:true,x:.5,y:.5})")
    await held([272,273]);await asyncio.sleep(12)
    long_hold=await held([272,273])
    await page.evaluate("async()=>{for(let i=0;i<200;i++){sendInput({kind:'relativeMove',deltaX:1,deltaY:1});await new Promise(r=>setTimeout(r,4))}}")
    await page.evaluate("sendInput({kind:'button',button:0,down:false,x:.5,y:.5})")
    independent=await held([273])
    assert independent['relative_count']>=200,independent
    await page.evaluate("sendInput({kind:'key',key:224,down:true});release()")
    released=await held([],[])
    result['control_checks']={'simultaneous_hold_seconds':12,'held_after_renewals':long_hold,'independent_left_release':independent,'release_clears_buttons_and_keys':released}
   await browser.close()
 except Exception as e:
  result['error']=repr(e)
  try:result['diagnostics']=await page.evaluate('async()=>({granted:window.granted,width:v.videoWidth,errors,connection:pc.connectionState,stats:Array.from((await pc.getStats()).values())})')
  except Exception:pass
 finally:
  (ROOT/(args.label+'-result.json')).write_text(json.dumps(result,indent=2));print(json.dumps(result),flush=True)
  for q in reversed(procs):
   try:os.killpg(q.pid,15)
   except ProcessLookupError:pass
  await runner.cleanup()
 if result.get('error'):raise SystemExit(1)
asyncio.run(main())
