#!/usr/bin/env python3
"""Shipped CLI over a PTY and real RFC6455 TCP; only Messages inference is synthetic."""
from claude_code_fixture import normalize_request, wrap_tool
import argparse, base64, fcntl, hashlib, importlib.util, json, os, pty, select, socket, struct, subprocess, termios, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from uuid import uuid4
spec = importlib.util.spec_from_file_location('scheduler_journey', Path(__file__).with_name('claude-scheduler-monitor-cli-journey.py'))
h = importlib.util.module_from_spec(spec); spec.loader.exec_module(h)
require, sse, text_of = h.require, h.sse, h.text_of


def main():
 p=argparse.ArgumentParser(description=__doc__); p.add_argument('--binary',required=True,type=Path); p.add_argument('--output',type=Path,default=Path('output/claude-monitor-ws-cli')/uuid4().hex); a=p.parse_args()
 artifact=a.output.resolve(); workspace=artifact/'workspace'; workspace.mkdir(parents=True); home=artifact/'home'; home.mkdir(); binary=a.binary.resolve()
 env={'HOME':str(home),'CODEX_HOME':str(home/'codex'),'PATH':'/usr/bin:/bin','TERM':'xterm-256color','NANOCODEX_COMPUTER':'off'}
 requests=[]; receipts=[]; events=[]; errors=[]; connections=[]; transcripts={}; screens={}; processes=[]; commands=[]; checks=[]
 state={'phase':'','steps':[],'index':0,'pending':None}; ids={}; release=threading.Event(); stopping=threading.Event()
 listener=socket.socket(); listener.bind(('127.0.0.1',0)); listener.listen(); listener.settimeout(.2); port=listener.getsockname()[1]; origin=f'ws://127.0.0.1:{port}'
 def frame(sock,opcode,data):
  if isinstance(data,str): data=data.encode()
  length=len(data); header=bytes([0x80|opcode,length]) if length<126 else bytes([0x80|opcode,126])+struct.pack('!H',length)
  sock.sendall(header+data)
 def websocket(sock,record):
  sock.settimeout(10)
  try:
   raw=b''
   while b'\r\n\r\n' not in raw: raw+=sock.recv(4096)
   lines=raw.decode().split('\r\n'); headers=dict(line.split(': ',1) for line in lines[1:] if ': ' in line); path=lines[0].split()[1]
   record.update(path=path,headers=headers,handshake_at=time.time())
   accept=base64.b64encode(hashlib.sha1((headers['Sec-WebSocket-Key']+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
   protocol='Sec-WebSocket-Protocol: fixture.v1\r\n' if headers.get('Sec-WebSocket-Protocol')=='fixture.v1' else ''
   sock.sendall(('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: '+accept+'\r\n'+protocol+'\r\n').encode())
   if path=='/events':
    release.wait(10)
    frame(sock,1,'first-line\nsecond-line'); frame(sock,1,'same-batch'); frame(sock,2,b'\x01\x02\x03'); frame(sock,9,'ping')
    time.sleep(.5); frame(sock,8,struct.pack('!H',1000))
   elif path=='/oversize': frame(sock,1,'x'*4097)
   else:
    while not stopping.is_set():
     try:
      if not sock.recv(1024): break
     except socket.timeout: break
   record['closed_at']=time.time()
  except (ConnectionError,OSError): pass
  finally: sock.close()
 def accept():
  while not stopping.is_set():
   try: sock,_=listener.accept()
   except socket.timeout: continue
   except OSError: break
   record={'at':time.time()}; connections.append(record)
   threading.Thread(target=websocket,args=(sock,record),daemon=True).start()
 threading.Thread(target=accept,daemon=True).start()
 class Provider(BaseHTTPRequestHandler):
  def log_message(self,*_): pass
  def do_POST(self):
   body=json.loads(self.rfile.read(int(self.headers['content-length']))); body = normalize_request(body, artifact); requests.append({'phase':state['phase'],'body':body})
   try:
    if state['pending']:
     call,step=state['pending']; results=[b for m in body['messages'] for b in m.get('content',[]) if isinstance(b,dict) and b.get('type')=='tool_result' and b.get('tool_use_id')==call]; require(len(results)==1,f'missing receipt {call}'); result=results[0]; receipts.append({'phase':state['phase'],'result':result}); require(bool(result.get('is_error'))==step[2],f'wrong error {call}: {result}')
     value=json.loads(text_of(result)) if not step[2] else text_of(result)
     if step[3]: step[3](value)
     state['pending']=None
    if state['index']<len(state['steps']):
     step=state['steps'][state['index']]; call=f"{state['phase']}_{state['index']}"; state['index']+=1; state['pending']=(call,step); inp=step[1]() if callable(step[1]) else step[1]; block={'type':'tool_use','id':call,'name':step[0],'input':inp}
    else:
     events.append({'phase':state['phase'],'at':time.time(),'message':body['messages'][-1]}); block={'type':'text','text':state['phase']+'-complete'}
   except Exception as e: errors.append(str(e)); block={'type':'text','text':'fixture-failed'}
   data=sse(wrap_tool(block),body['model']); self.send_response(200); self.send_header('Content-Type','text/event-stream'); self.send_header('Content-Length',str(len(data))); self.end_headers(); self.wfile.write(data)
 server=ThreadingHTTPServer(('127.0.0.1',0),Provider); threading.Thread(target=server.serve_forever,daemon=True).start()
 common=['--claude','--model','claude-sonnet-5-5','--claude-api-key','synthetic-key','--claude-messages-url',f'http://127.0.0.1:{server.server_port}/v1/messages','--cwd',str(workspace),'--browser=none','--mcp-defaults','false','--mcp-codex-config','false','--image-generation','false','--subagents','false','--memory','false']
 def phase(name,steps): state.update(phase=name,steps=steps,index=0,pending=None)
 def start(label,web=True,trusted=False,flags=None):
  master,slave=pty.openpty(); fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',45,170,0,0)); cmd=[str(binary)]+common+['--web-search',str(web).lower()]+(flags or [])+['--prompt','Exercise authorized Monitor transport.']; local=dict(env)
  if trusted: cmd+=['--claude-monitor-ws-origin',origin]
  proc=subprocess.Popen(cmd,stdin=slave,stdout=slave,stderr=slave,cwd=workspace,env=local,start_new_session=True); os.close(slave); processes.append(proc); commands.append({'argv':cmd,'environment':local}); transcripts[label]=bytearray(); screens[label]=h.TerminalScreen()
  def drain():
   while select.select([master],[],[],0)[0]:
    try: data=os.read(master,65536)
    except OSError: break
    if not data: break
    transcripts[label].extend(data); screens[label].feed(data)
    if b'\x1b[6n' in data: os.write(master,b'\x1b[1;1R')
  return proc,master,drain
 def wait(check,drain,message,timeout=20):
  end=time.monotonic()+timeout
  while time.monotonic()<end:
   drain(); require(not errors,'; '.join(errors))
   if check(): return
   time.sleep(.02)
  raise AssertionError(message)
 def visible(label,text): return text in screens[label].text()
 def finish(proc,fd,drain):
  os.write(fd,b'\x03\x03')
  wait(lambda:proc.poll() is not None,drain,'TUI exit stuck',timeout=10)
  drain(); os.close(fd)
 def monitor(path,**extra): return {'ws':{'url':origin+path},'description':'Synthetic WebSocket',**extra}
 def save(value): ids['monitor']=value['task_id']
 def status(expected): return lambda value: require(value['status']==expected,f'expected {expected}: {value}')
 outcome={'success':False}
 try:
  (workspace/'.env').write_text('NANOCODEX_CLAUDE_MONITOR_WS_ORIGINS='+origin+'\n')
  rules=artifact/'deny.json'; rules.write_text(json.dumps({'permissions':{'defaultMode':'full-access','allow':['Monitor'],'deny':['WebFetch(domain:127.0.0.1)']}}))
  cases=[('missing-gate',False,True,[], 'requires explicit host web access'),('private-default-deny',True,False,[],'exclusively public Internet'),('domain-denied',True,True,['--claude-permissions',str(rules)],'denied'),('invalid-origin',True,False,['--claude-monitor-ws-origin',origin+'/path'],'expected exact ws/wss origin'),('different-origin',True,False,['--claude-monitor-ws-origin',f'ws://localhost:{port}'],'exclusively public Internet')]
  for label,web,trusted,flags,message in cases:
   phase(label,[('Monitor',monitor('/denied'),True,lambda value,m=message:require(m.lower() in value.lower(),f'missing denial {m}: {value}'))]); before=len(connections); proc,fd,drain=start(label,web,trusted,flags); wait(lambda:visible(label,label+'-complete'),drain,label+' incomplete'); finish(proc,fd,drain); require(len(connections)==before,label+' opened TCP connection'); checks.append(label+' rejected before connection')
  # Strict nested schema rejects ambient headers and invalid subprotocols before connecting.
  phase('invalid-input',[('Monitor',{'ws':{'url':origin+'/denied','headers':{'X-Test':'invalid'}},'description':'invalid headers'},True,None),('Monitor',{'ws':{'url':origin+'/denied','protocols':['bad protocol']},'description':'invalid protocol'},True,None)])
  proc,fd,drain=start('invalid-input',trusted=True); wait(lambda:visible('invalid-input','invalid-input-complete'),drain,'invalid input incomplete'); finish(proc,fd,drain); require(not connections,'invalid input connected'); checks.append('nested schema and subprotocol tokens validated before connection')
  phase('events',[('Monitor',{'ws':{'url':origin+'/events','protocols':['fixture.v1']},'description':'Multiline and binary events'},False,save)])
  proc,fd,drain=start('events',trusted=True); wait(lambda:visible('events','events-complete'),drain,'events start incomplete'); os.write(fd,b'unsent draft'); time.sleep(.2); release.set()
  end=time.monotonic()+1
  while time.monotonic()<end: drain(); time.sleep(.02)
  require(not any('Monitor event (' in json.dumps(e['message']) for e in events),'monitor interrupted composer'); os.write(fd,b'\x15')
  wait(lambda:any('Monitor finished' in json.dumps(e['message']) for e in events),drain,'idle completion absent')
  batches=[e for e in events if 'Monitor event (' in json.dumps(e['message'])]; require(len(batches)==1,f'expected 200ms batching, got {len(batches)}'); batch=json.dumps(batches[0]); require('same-batch' in batch and '[binary frame, 3 bytes]' in batch,'missing text/binary event'); require('first-line' in batch and 'second-line' in batch,'multiline missing')
  phase('inspect',[('TaskOutput',lambda:{'task_id':ids['monitor'],'block':True,'timeout':10000},False,lambda r:(status('completed')(r),require(r['close_code']==1000,'close code missing'),require(r['stdout']=='first-line\nsecond-line\nsame-batch\n[binary frame, 3 bytes]\n','frame output changed')))])
  os.write(fd,b'Inspect WebSocket result\r'); wait(lambda:visible('events','inspect-complete'),drain,'inspect incomplete'); finish(proc,fd,drain); require(connections[0]['headers']['Sec-WebSocket-Protocol']=='fixture.v1','subprotocol missing'); checks.append('trusted exact origin negotiates subprotocol; multiline/text/binary frames batch once; composer suppresses delivery; normal close and retained output observed')
  for label,path,extra,expected in [('cancel','/stall',{'persistent':True},'stopped'),('timeout','/timeout',{'timeout_ms':1000},'timed_out'),('oversize','/oversize',{},'output_limit')]:
   steps=[('Monitor',monitor(path,**extra),False,save),('TaskStop' if label=='cancel' else 'TaskOutput',lambda label=label:{'task_id':ids['monitor'],**({} if label=='cancel' else {'block':True,'timeout':10000})},False,status(expected))]
   phase(label,steps); proc,fd,drain=start(label,trusted=True); wait(lambda:visible(label,label+'-complete'),drain,label+' incomplete'); wait(lambda:any(c.get('path')==path and 'closed_at' in c for c in connections),drain,label+' transport still open'); finish(proc,fd,drain); checks.append(label+' ends real WebSocket with retained '+expected+' status')
  outcome={'success':True,'checks':checks,'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}
 finally:
  stopping.set(); listener.close(); release.set()
  for proc in processes:
   if proc.poll() is None: proc.kill(); proc.wait()
  for label,data in transcripts.items(): (artifact/(label+'.pty')).write_bytes(data); (artifact/(label+'.screen.txt')).write_text(screens[label].text())
  for name,value in [('requests',requests),('receipts',receipts),('events',events),('connections',connections),('scenario',commands),('outcome',outcome)]: (artifact/(name+'.json')).write_text(json.dumps(value,indent=2))
  server.shutdown(); print(json.dumps({'artifact':str(artifact),**outcome}))
if __name__=='__main__': main()
