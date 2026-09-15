#!/usr/bin/env python3
"""Opt-in disposable extension + native-message conformance. Never uses a personal profile."""
import argparse, hashlib, http.server, json, os, pathlib, shutil, subprocess, sys, tempfile, threading, time, traceback, urllib.request
import websocket
ROOT=pathlib.Path(__file__).resolve().parents[1]
def input_hashes():
 paths=[ROOT/'Cargo.toml',ROOT/'Cargo.lock',ROOT/'tests/browser_extension_live.py']
 for directory in ['src','vendor','extensions/chrome']:
  paths += [path for path in (ROOT/directory).rglob('*') if path.is_file() and not any(part in ['__pycache__','target'] for part in path.relative_to(ROOT).parts)]
 return {str(path.relative_to(ROOT)):hashlib.sha256(path.read_bytes()).hexdigest() for path in sorted(set(paths))}
def capture_inputs(evidence,binary,chrome):
 hashes=input_hashes()
 for relative,digest in hashes.items():
  target=evidence/'source'/relative;target.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/relative,target)
  assert hashlib.sha256(target.read_bytes()).hexdigest()==digest,f'Source changed while copying: {relative}'
 executables={name:{'path':str(pathlib.Path(path).resolve()),'sha256':hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()} for name,path in [('binary',binary),('chrome',chrome),('python',sys.executable)]}
 record={'inputHashes':hashes,'executables':executables,'pythonVersion':sys.version,'websocketVersion':websocket.__version__}
 (evidence/'inputs.json').write_text(json.dumps(record,indent=2)+'\n')
 return record
def verify_inputs(record):
 assert input_hashes()==record['inputHashes'],'Source file set or content changed during run'
 for name,entry in record['executables'].items():
  assert hashlib.sha256(pathlib.Path(entry['path']).read_bytes()).hexdigest()==entry['sha256'],f'{name} executable changed during run'
class Cdp:
 def __init__(self,url): self.ws=websocket.create_connection(url,timeout=20,suppress_origin=True);self.id=0;self.events=[]
 def call(self,method,params=None,session=None):
  self.id+=1;request={'id':self.id,'method':method,'params':params or {}}
  if session:request['sessionId']=session
  self.ws.send(json.dumps(request))
  while True:
   value=json.loads(self.ws.recv())
   if value.get('id')!=self.id:self.events.append(value);continue
   if 'error' in value:raise RuntimeError(json.dumps(value['error']))
   return value.get('result',{})
 def close(self):self.ws.close()
class Fixture(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  body=b'<!doctype html><title>Owned extension fixture</title><input id="input"><button onclick="document.title=\'clicked\'">Click</button>'
  content_type='text/html'
  if self.path=='/download.bin':body=b'Owned extension download alpha\n';content_type='application/octet-stream'
  self.send_response(200)
  if self.path=='/download.bin':self.send_header('Content-Disposition','attachment; filename=owned-extension.txt')
  self.send_header('Content-Type',content_type);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
 def log_message(self,*args):pass

def main():
 parser=argparse.ArgumentParser();parser.add_argument('--chrome',default='/Applications/Google Chrome.app/Contents/MacOS/Google Chrome');parser.add_argument('--binary',default=str(ROOT/'target/debug/nanocodex-computer'));parser.add_argument('--evidence',required=True);args=parser.parse_args()
 evidence=pathlib.Path(args.evidence);evidence.mkdir(parents=True,exist_ok=False)
 inputs=capture_inputs(evidence,args.binary,args.chrome)
 args.binary=inputs['executables']['binary']['path'];args.chrome=inputs['executables']['chrome']['path']
 report={'binarySha256':inputs['executables']['binary']['sha256'],'scope':'Independent extension in a new disposable Chrome profile, synthetic loopback HTML only','passed':[],'limitations':['No personal profile or installed user extension touched','This does not implement the original private native-host protocol']};processes=[];connections=[]
 try:
  temporary_scope=tempfile.TemporaryDirectory(prefix='skyre-extension-fixture-')
  temporary=temporary_scope.name
  temporary=pathlib.Path(temporary);profile=temporary/'profile';profile.mkdir();(profile/'Default').mkdir();downloads=temporary/'downloads';downloads.mkdir();(profile/'Default/Preferences').write_text(json.dumps({'download':{'default_directory':str(downloads),'prompt_for_download':False}}));extension=temporary/'extension'
  subprocess.run([args.binary,'extension-export','--destination',str(extension)],check=True,stdout=subprocess.DEVNULL)
  assert {p.name:p.read_bytes() for p in extension.iterdir()}=={p.name:p.read_bytes() for p in (ROOT/'extensions/chrome').iterdir()},'Bundled extension differs from its source'
  log=open(evidence/'chrome.stderr.log','wb')
  chrome=subprocess.Popen([args.chrome,'--headless=new',f'--user-data-dir={profile}','--remote-debugging-port=0','--enable-unsafe-extension-debugging','--disable-features=DisableLoadExtensionCommandLineSwitch','--enable-logging=stderr','--vmodule=extension_service_worker*=2,service_worker_context*=2','--no-first-run','--no-default-browser-check','--disable-background-networking','--disable-component-update','--disable-default-apps','--disable-sync','--password-store=basic','--use-mock-keychain','--no-proxy-server','--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE localhost, EXCLUDE 127.0.0.1','--metrics-recording-only','--safebrowsing-disable-auto-update','about:blank'],stdout=subprocess.DEVNULL,stderr=log);processes.append(chrome)
  active=profile/'DevToolsActivePort'
  for _ in range(100):
   if active.exists():break
   if chrome.poll() is not None:raise RuntimeError('Disposable Chrome exited before DevTools became ready')
   time.sleep(.1)
  lines=active.read_text().splitlines();cdp=Cdp(f'ws://127.0.0.1:{lines[0]}{lines[1]}');connections.append(cdp)
  report['browserVersion']=cdp.call('Browser.getVersion');worker=None
  initial_page=next(t for t in cdp.call('Target.getTargets')['targetInfos'] if t['type']=='page')
  initial_session=cdp.call('Target.attachToTarget',{'targetId':initial_page['targetId'],'flatten':True})['sessionId']
  cdp.call('ServiceWorker.enable',session=initial_session)
  # Current branded Chrome may ignore --load-extension. Use its explicit
  # unpacked-extension debug endpoint only in this newly created profile.
  try:
   report['loadUnpacked']=cdp.call('Extensions.loadUnpacked',{'path':str(extension)})
   cdp.call('ServiceWorker.startWorker',{'scopeURL':'chrome-extension://'+report['loadUnpacked']['id']+'/'},initial_session)
  except Exception as error:report['loadUnpackedError']=str(error)
  for _ in range(100):
   targets=cdp.call('Target.getTargets',{'filter':[{}]})['targetInfos'];worker=next((t for t in targets if t['type']=='service_worker' and t['url'].endswith('/background.js')),None)
   if worker:break
   time.sleep(.1)
  if not worker:
   report['targets']=targets;report['workerEvents']=cdp.events
   for name in ['Preferences','Secure Preferences']:
    pref=profile/'Default'/name
    if pref.exists():report[name]=json.loads(pref.read_text()).get('extensions',{}).get('settings',{}).get(report.get('loadUnpacked',{}).get('id'))
   try:report['startWorker']=cdp.call('ServiceWorker.startWorker',{'scopeURL':'chrome-extension://'+report['loadUnpacked']['id']+'/'},initial_session)
   except Exception as error:report['startWorkerError']=str(error)
   raise RuntimeError('Chrome did not start the independent extension background service worker')
  report['extensionId']=worker['url'].split('/')[2];report['passed'].append('Actual MV3 extension loaded in disposable profile')
  worker_session=cdp.call('Target.attachToTarget',{'targetId':worker['targetId'],'flatten':True})['sessionId']
  socket=temporary/'bridge'/'bridge.sock';socket.parent.mkdir(mode=0o700)
  bridge=subprocess.Popen([args.binary,'extension-bridge','--socket',str(socket)],stdout=subprocess.PIPE,stderr=open(evidence/'bridge.stderr.log','wb'),text=True);processes.append(bridge);config=json.loads(bridge.stdout.readline())
  # Only disposable candidate lookup roots. Never write a system/user host manifest.
  host_roots=[profile/'NativeMessagingHosts']
  for destination in host_roots:
   destination.mkdir(mode=0o700,parents=True,exist_ok=True);destination.chmod(0o700)
   subprocess.run([args.binary,'extension-manifest','--destination',str(destination),'--socket',str(socket),'--extension-id',report['extensionId']],check=True,stdout=subprocess.DEVNULL)
  bridge_cdp=Cdp(config['endpoint']);connections.append(bridge_cdp)
  cdp.call('Runtime.evaluate',{'expression':'disconnectPort(); connect();'},worker_session)
  begun=bridge_cdp.call('Skyre.beginTurn');report['passed'].append('Real Chrome native messaging connected to independent Rust host and bridge')
  # The MV3 worker itself originates native asset RPCs; the authenticated CDP
  # connection only evaluates this owned worker fixture and is not the file owner.
  asset_result=cdp.call('Runtime.evaluate',{'expression':"(async()=>{const asset=await tabContextAsset('create',{fileName:'../owned%context.txt'});await tabContextAsset('appendChunk',{assetId:asset.assetId,dataBase64:'b3duZWQgZXh0ZW5zaW9uIM+A'});const first=await tabContextAsset('finish',asset),second=await tabContextAsset('finish',asset);let refusal;try{await tabContextAsset('appendChunk',{assetId:asset.assetId,dataBase64:'!'});}catch(error){refusal={code:error.code,message:error.message};}return{asset,first,second,refusal,pending:nativeAssetRequests.size};})()",'awaitPromise':True,'returnByValue':True},worker_session)
  assert 'exceptionDetails' not in asset_result,asset_result
  native_asset=asset_result['result']['value'];asset_path=pathlib.Path(native_asset['asset']['path'])
  assert native_asset['first']==native_asset['second']==native_asset['asset'] and native_asset['pending']==0
  assert native_asset['refusal']=={'code':1,'message':'Chrome tab context asset is already finished'}
  assert asset_path.read_bytes()=='owned extension π'.encode() and asset_path.stat().st_mode&0o777==0o600
  assert asset_path.parent.stat().st_mode&0o777==0o700
  report['nativeAsset']=native_asset
  removed=cdp.call('Runtime.evaluate',{'expression':"tabContextAsset('remove',"+json.dumps(native_asset['asset'])+")",'awaitPromise':True,'returnByValue':True},worker_session)
  assert 'exceptionDetails' not in removed and not asset_path.exists(),removed
  pending=cdp.call('Runtime.evaluate',{'expression':"tabContextAsset('create',{fileName:'disconnect-cleanup.txt'})",'awaitPromise':True,'returnByValue':True},worker_session)
  assert 'exceptionDetails' not in pending,pending
  asset_for_disconnect=pathlib.Path(pending['result']['value']['path']);assert asset_for_disconnect.exists()
  report['passed'].append('Actual Chrome worker native asset create/append/repeated finish/refusal/remove with exact bytes, private modes and independent response correlation')
  fixture=http.server.ThreadingHTTPServer(('127.0.0.1',0),Fixture);threading.Thread(target=fixture.serve_forever,daemon=True).start();url=f'http://127.0.0.1:{fixture.server_port}/'
  try:
   # A tab created by the fixture is user-origin from the provider's perspective.
   user=cdp.call('Target.createTarget',{'url':url})['targetId'];user_info=None
   for _ in range(100):
    tabs=bridge_cdp.call('Skyre.openTabs')['tabs'];user_info=next((t for t in tabs if t.get('url')==url),None)
    if user_info:break
    time.sleep(.05)
   if user_info is None:report['userTabs']=tabs;report['createdTarget']=cdp.call('Target.getTargetInfo',{'targetId':user});report['fixtureUrl']=url
   assert user_info is not None,'Created fixture tab never appeared in user discovery'
   user_id=user_info['id']
   try:bridge_cdp.call('Target.attachToTarget',{'targetId':user_id});raise AssertionError('Unclaimed attachment succeeded')
   except RuntimeError as error:assert 'not owned' in str(error)
   bridge_cdp.call('Skyre.claimTab',{'tab':user_id});session=bridge_cdp.call('Target.attachToTarget',{'targetId':user_id})['sessionId']
   value=None
   for _ in range(100):
    value=bridge_cdp.call('Runtime.evaluate',{'expression':'document.title','returnByValue':True},session)
    if value.get('result',{}).get('value')=='Owned extension fixture':break
    time.sleep(.05)
   assert value['result']['value']=='Owned extension fixture',str(value); report['passed'].append('Real user-tab discovery, unclaimed rejection, persisted claim, debugger attach and renderer evaluation')
   text='α🧪'*250000
   large=bridge_cdp.call('Runtime.evaluate',{'expression':json.dumps(text,ensure_ascii=False),'returnByValue':True},session)
   assert large['result']['value']==text,'Large native-message Unicode round trip differed'
   report['passed'].append('Actual Chrome native messaging chunks an input and output above 1 MiB with exact Unicode round trip')
   # The rebuilt public facade and Rust response authorization drive the actual
   # extension callback adapter. No direct Chrome download-directory command.
   bridge_cdp.close()
   for _ in range(100):
    if not asset_for_disconnect.exists():break
    time.sleep(.02)
   assert not asset_for_disconnect.exists(),'Native disconnect retained an owned tab-context asset'
   report['passed'].append('Actual native-channel disconnect drains outstanding tab-context asset files')
   policy=temporary/'download-policy.json';policy.write_text(json.dumps({'preapproved_download_origins':[url]}))
   code="var b=await cua.getBrowser({id:'owned-extension'});var t=await b.tabs.get("+json.dumps(user_id)+");var [download]=await Promise.all([t.playwright.waitForEvent('download',{timeoutMs:3000}),t.goto("+json.dumps(url+'download.bin')+").catch(error=>{if(!String(error).includes('ERR_ABORTED'))throw error;})]);var path=await download.path();if(typeof path!=='string'||!path)throw Error('Missing causal download path');nodeRepl.write(path);"
   # Allow the cell's 30-second budget plus bounded provider cleanup.
   started=time.monotonic()
   try:
    run=subprocess.run([args.binary,'--fixture','--cdp','owned-extension='+config['endpoint'],'--security-config',str(policy),'eval','--code',code],capture_output=True,text=True,timeout=60)
   except subprocess.TimeoutExpired as error:
    report['downloadPublicCli']={'timeoutSeconds':error.timeout,'stdout':(error.stdout or b'').decode('utf-8','replace') if isinstance(error.stdout,bytes) else error.stdout,'stderr':(error.stderr or b'').decode('utf-8','replace') if isinstance(error.stderr,bytes) else error.stderr}
    report['downloadWorkerState']=cdp.call('Runtime.evaluate',{'expression':'JSON.stringify({sessions:[...sessions],expected:[...expectedDownloads],active:[...activeDownloads],leases:[...leases.leases],turns:[...leases.sessions]})','returnByValue':True},worker_session)
    raise
   report['downloadPublicCli']={'exitCode':run.returncode,'stdout':run.stdout,'stderr':run.stderr,'wallTimeSeconds':time.monotonic()-started};assert run.returncode==0,report['downloadPublicCli']
   output=json.loads(run.stdout);assert not output.get('error'),output
   downloaded=list(downloads.glob('*'));assert any(path.is_file() and path.read_bytes()==b'Owned extension download alpha\n' for path in downloaded),'Owned bytes were not downloaded'
   report['passed'].append('Actual Rust public download wait/path: response-stage approval, native Chrome callback attribution and owned file bytes')
   bridge_cdp=Cdp(config['endpoint']);connections.append(bridge_cdp)
   # CLI connection end releases its user tab; explicitly reclaim it for the
   # remaining independent restart/lifecycle fixture operations.
   bridge_cdp.call('Skyre.beginTurn');begun=bridge_cdp.call('Skyre.getInfo');bridge_cdp.call('Skyre.claimTab',{'tab':user_id});session=bridge_cdp.call('Target.attachToTarget',{'targetId':user_id})['sessionId'];bridge_cdp.call('Page.navigate',{'url':url},session)
   versions=[version for event in cdp.events if event.get('method')=='ServiceWorker.workerVersionUpdated' for version in event['params']['versions'] if version['scriptURL']==worker['url']]
   assert versions,'Missing owned worker version identity'
   cdp.call('Target.detachFromTarget',{'sessionId':worker_session})
   cdp.call('ServiceWorker.stopWorker',{'versionId':versions[-1]['versionId']},initial_session)
   bridge_cdp.close()
   cdp.call('ServiceWorker.startWorker',{'scopeURL':'chrome-extension://'+report['extensionId']+'/'},initial_session)
   bridge_cdp=Cdp(config['endpoint']);connections.append(bridge_cdp)
   resumed=bridge_cdp.call('Skyre.getInfo');assert resumed['sessionId']==begun['sessionId'] and resumed['turnId']==begun['turnId']
   assert any(tab['targetId']==user_id for tab in bridge_cdp.call('Target.getTargets')['targetInfos'])
   session=bridge_cdp.call('Target.attachToTarget',{'targetId':user_id})['sessionId']
   value=bridge_cdp.call('Runtime.evaluate',{'expression':'document.title','returnByValue':True},session);assert value['result']['value']=='Owned extension fixture'
   report['passed'].append('Actual worker termination/restart and native reconnect preserve session/turn/tab lease and reattach the same renderer')
   plain=bridge_cdp.call('Target.createTarget',{'url':url})['targetId'];deliverable=bridge_cdp.call('Target.createTarget',{'url':url})['targetId'];handoff=bridge_cdp.call('Target.createTarget',{'url':url})['targetId']
   bridge_cdp.call('Skyre.markTab',{'tab':deliverable,'status':'deliverable'});bridge_cdp.call('Skyre.markTab',{'tab':handoff,'status':'handoff'});bridge_cdp.call('Skyre.turnEnded')
   try:bridge_cdp.call('Target.getTargets');raise AssertionError('Ended turn reactivated')
   except RuntimeError as error:assert 'stale' in str(error)
   bridge_cdp.call('Skyre.beginTurn');owned=bridge_cdp.call('Target.getTargets')['targetInfos'];assert [t['targetId'] for t in owned]==[handoff];all_ids={t['id'] for t in bridge_cdp.call('Skyre.openTabs')['tabs']};assert plain not in all_ids and {user_id,deliverable,handoff}<=all_ids
   report['passed'].append('Real group/mark/turn cleanup: unmarked agent closed, user/deliverable released, handoff resumed, stale turn rejected')
   bridge_cdp.call('Skyre.turnEnded')
   # Independent host-turn authority is separate from the CDP bridge capability.
   host_token=json.loads(pathlib.Path(str(socket)+'.owner.json').read_text())['hostAuthority']
   host_event={'authorityToken':host_token,'eventId':'host-start','sequence':1,'phase':'started','route':{'conversationId':'owned-fixture','threadId':'child'},'turnId':'host-turn-1'}
   try:bridge_cdp.call('Skyre.hostLifecycle',{**host_event,'authorityToken':'forged'});raise AssertionError('Forged host event was accepted')
   except RuntimeError as error:assert 'authority'in str(error)
   bridge_cdp.call('Skyre.hostLifecycle',host_event)
   trusted=bridge_cdp.call('Skyre.getInfo');bridge_cdp.call('Skyre.hostLifecycle',host_event);assert bridge_cdp.call('Skyre.getInfo')['turnId']==trusted['turnId']
   closed_by_host=bridge_cdp.call('Target.createTarget',{'url':url})['targetId']
   ended={**host_event,'eventId':'host-end','sequence':2,'phase':'ended'}
   bridge_cdp.call('Skyre.hostLifecycle',ended);bridge_cdp.call('Skyre.hostLifecycle',ended)
   try:bridge_cdp.call('Skyre.beginTurn');raise AssertionError('Model begin bypassed managed lifecycle')
   except RuntimeError as error:assert 'trusted host events'in str(error)
   bridge_cdp.call('Skyre.hostLifecycle',{**host_event,'eventId':'next-start','sequence':3,'turnId':'host-turn-2'})
   assert bridge_cdp.call('Target.getTargets')['targetInfos']==[]
   assert closed_by_host not in {t['id'] for t in bridge_cdp.call('Skyre.openTabs')['tabs']}
   bridge_cdp.call('Skyre.hostLifecycle',{**ended,'eventId':'next-end','sequence':4,'turnId':'host-turn-2'})
   report['passed'].append('Actual native bridge independently authenticates conversation/subagent lifecycle, retries acknowledgements idempotently and closes the owned tab on host end')
  finally:fixture.shutdown();fixture.server_close()
 except Exception as error:
  report['error']=str(error);report['exceptionType']=type(error).__name__;report['traceback']=traceback.format_exc()
 finally:
  for connection in reversed(connections):
   try:connection.close()
   except Exception:pass
  for process in reversed(processes):
   if process.poll() is None:process.terminate()
   try:process.wait(timeout=5)
   except subprocess.TimeoutExpired:process.kill();process.wait()
  if 'temporary_scope' in locals():temporary_scope.cleanup()
  try:verify_inputs(inputs)
  except Exception as error:report['provenanceError']=str(error);report.setdefault('error',str(error))
  report['status']='passed' if 'error' not in report else 'failed'
  (evidence/'report.json').write_text(json.dumps(report,indent=2)+'\n')
  files=[{'path':str(path.relative_to(evidence)),'sha256':hashlib.sha256(path.read_bytes()).hexdigest()} for path in sorted(evidence.rglob('*')) if path.is_file()]
  (evidence/'manifest.json').write_text(json.dumps({'schema':1,'scope':report['scope'],'files':files},indent=2)+'\n')
  print(json.dumps(report,indent=2))
 return 0 if report['status']=='passed' else 1
if __name__=='__main__':raise SystemExit(main())
