#!/usr/bin/env python3
"""Real CLI PTY, real clock, session reopen and command process cancellation.
Only the remote Messages endpoint is synthetic; there is no fake clock.
"""
import shutil
import argparse, codecs, fcntl, hashlib, importlib.util, json, os, pty, re, select, struct, subprocess, termios, threading, time, unicodedata
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4
spec=importlib.util.spec_from_file_location('journey',Path(__file__).with_name('claude-native-cli-journey.py'))
h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
require,sse,text_of=h.require,h.sse,h.text_of

class TerminalScreen:
 """Decode the CLI's cursor-addressed VT output, including split UTF-8/CSI.

 Ratatui writes only changed cells. Removing ANSI sequences loses unchanged
 cells inside words and is not a valid observation of the displayed screen.
 Unknown control sequences fail explicitly instead of producing false evidence.
 """
 def __init__(self, rows=45, columns=170):
  self.rows,self.columns=rows,columns;self.x=self.y=0
  self.cells=[[' ']*columns for _ in range(rows)]
  self.decoder=codecs.getincrementaldecoder('utf-8')();self.pending=''
 def feed(self,data):
  self.pending+=self.decoder.decode(data)
  while self.pending:
   if self.pending.startswith('\x1b'):
    if len(self.pending)<2:return
    require(self.pending[1]=='[','unsupported terminal escape')
    match=re.match(r'\x1b\[([0-?]*)([ -/]*)([@-~])',self.pending)
    if match is None:return
    params,intermediate,command=match.groups();self.pending=self.pending[match.end():]
    require(not intermediate,'unsupported terminal intermediate')
    values=[int(v) if v else 0 for v in params.lstrip('?><=').split(';')]
    first=values[0] or 1
    if command in ('H','f'):
     self.y=min(self.rows-1,first-1);self.x=min(self.columns-1,(values[1] or 1)-1 if len(values)>1 else 0)
    elif command=='A':self.y=max(0,self.y-first)
    elif command=='B':self.y=min(self.rows-1,self.y+first)
    elif command=='C':self.x=min(self.columns-1,self.x+first)
    elif command=='D':self.x=max(0,self.x-first)
    elif command=='G':self.x=min(self.columns-1,first-1)
    elif command=='J':
     require(values[0] in (2,3),'unsupported partial terminal erase')
     self.cells=[[' ']*self.columns for _ in range(self.rows)]
    elif command=='K':
     lo,hi=(0,self.columns) if values[0]==2 else ((0,self.x+1) if values[0]==1 else (self.x,self.columns))
     self.cells[self.y][lo:hi]=[' ']*(hi-lo)
    else:require(command in ('m','h','l','n','u','c'),'unsupported terminal control '+command)
    continue
   char,self.pending=self.pending[0],self.pending[1:]
   if char=='\r':self.x=0;continue
   if char=='\n':
    self.y+=1
    if self.y>=self.rows:self.cells.pop(0);self.cells.append([' ']*self.columns);self.y=self.rows-1
    continue
   if char=='\b':self.x=max(0,self.x-1);continue
   if char=='\t':self.x=min(self.columns-1,(self.x//8+1)*8);continue
   if ord(char)<32 or ord(char)==127:continue
   if unicodedata.combining(char) or unicodedata.category(char) in ('Mn','Me','Cf'):continue
   width=2 if unicodedata.east_asian_width(char) in ('W','F') else 1
   if self.x>=self.columns:self.x=0;self.y=min(self.rows-1,self.y+1)
   self.cells[self.y][self.x]=char
   if width==2 and self.x+1<self.columns:self.cells[self.y][self.x+1]=''
   self.x+=width
 def text(self):return '\n'.join(''.join(row) for row in self.cells)

def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,default=Path('output/claude-scheduler-monitor-cli')/uuid4().hex);a=p.parse_args()
 artifact=a.output.resolve();artifact.mkdir(parents=True);workspace=artifact/'workspace';workspace.mkdir();home=artifact/'home';home.mkdir();codex_home=home/'codex';codex_home.mkdir();binary=artifact/'nanocodex-under-test';shutil.copy2(a.binary.resolve(),binary);binary.chmod(0o700);binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest()
 env={'HOME':str(home),'CODEX_HOME':str(codex_home),'PATH':'/usr/bin:/bin','TERM':'xterm-256color','NANOCODEX_COMPUTER':'off'}
 requests=[];errors=[];commands=[];processes=[];transcripts={};screens={};checks=[];ids={};events=[];receipts=[]
 state={'phase':'initial','index':0,'pending':None,'steps':[],'done':False}
 def remember(key):
  def save(r):ids[key]=r['id']
  return save
 def check_tasks(r):
  require(any(t['id']==ids['retained'] for t in r['tasks']),'recurring schedule not restored');require(all(t['id'] not in {'wakeup',ids['expired'],ids['missed-one']} for t in r['tasks']),'expired/missed/dynamic task restored');require(all(t['next_fire_at']>time.time() for t in r['tasks']),'missed recurring backlog replayed')
 def jitter(r,recurring,bound):
  task=r['task'];seed=int.from_bytes(hashlib.sha256(r['id'].encode()).digest()[:8],'big');expected=seed%(bound+1)*(1 if recurring else -1)
  require(r['jitter_seconds']==expected and task['jitter_seconds']==expected,'task ID jitter differs from receipt')
  require(task['next_fire_at']==max(task['created_at']+1,r['nominal_fire_at']+expected),'jitter not applied to persisted fire')
 def monitor_id(r):ids['monitor']=r['task_id']
 def stopped(r):require(r['status']=='stopped',f'monitor not stopped {r}')
 def steps_initial():return [
  ('CronCreate',{'cron':'* * * * *','prompt':'short-jitter','recurring':True},False,lambda r:(jitter(r,True,30),remember('short-jitter')(r))),
  ('CronDelete',lambda:{'id':ids['short-jitter']},False,None),
  ('CronCreate',{'cron':'0 * * * *','prompt':'hour-jitter','recurring':True},False,lambda r:(jitter(r,True,1800),remember('hour-jitter')(r))),
  ('CronDelete',lambda:{'id':ids['hour-jitter']},False,None),
  ('CronCreate',{'cron':'30 * * * *','prompt':'early-jitter','recurring':False},False,lambda r:(jitter(r,False,90),remember('early-jitter')(r))),
  ('CronDelete',lambda:{'id':ids['early-jitter']},False,None),
  ('CronCreate',{'cron':'7 * * * *','prompt':'exact-minute','recurring':False},False,lambda r:(jitter(r,False,0),remember('exact-minute')(r))),
  ('CronDelete',lambda:{'id':ids['exact-minute']},False,None),
  ('CronCreate',{'cron':'0 0 1 1 *','prompt':'final-seven-day-marker','recurring':True},False,remember('final-expiry')),
  ('CronCreate',{'cron':'* * * * *','prompt':'cron-once-marker','recurring':False,'timezone':'Etc/UTC'},False,None),
  ('CronCreate',{'cron':'* * * * *','prompt':'deleted-must-not-fire','recurring':False},False,remember('deleted')),
  ('CronDelete',lambda:{'id':ids['deleted']},False,lambda r:require(r['deleted'],'delete failed')),
  ('CronCreate',{'cron':'0 0 * * *','prompt':'retained-marker','timezone':'America/New_York'},False,remember('retained')),
  ('CronCreate',{'cron':'0 0 1 1 *','prompt':'expired-marker','recurring':True},False,remember('expired')),
  ('CronCreate',{'cron':'0 0 1 1 *','prompt':'missed-one-marker','recurring':False},False,remember('missed-one')),
  ('CronCreate',{'cron':'* * * * *','prompt':'bad','timezone':'Invalid/Zone'},True,None),
  ('CronCreate',{'cron':'* * * * * *','prompt':'bad'},True,None),
  ('ScheduleWakeup',{'delaySeconds':60,'prompt':'dynamic-real-clock-marker','reason':'real clock journey','noop':False},False,None)]
 def phase(name,steps):state.update(phase=name,index=0,pending=None,steps=steps,done=False)
 class Provider(BaseHTTPRequestHandler):
  def log_message(self,*_):pass
  def do_POST(self):
   request=json.loads(self.rfile.read(int(self.headers['content-length'])));requests.append(request);(artifact/'provider.json').write_text(json.dumps(requests,indent=2))
   try:
    pending=state['pending']
    if pending:
     call,step=pending;rs=[b for m in request['messages'] for b in m.get('content',[]) if isinstance(b,dict) and b.get('type')=='tool_result' and b.get('tool_use_id')==call];require(len(rs)==1,f'missing {call}')
     r=rs[0];require(bool(r.get('is_error'))==step[2],f'wrong error for {call}: {r}');receipts.append(r)
     if step[3]:step[3](json.loads(text_of(r)))
     state['pending']=None
    if state['index']<len(state['steps']):
     step=state['steps'][state['index']];call=f"{state['phase']}_{state['index']}";state['index']+=1;state['pending']=(call,step);inp=step[1]() if callable(step[1]) else step[1];block={'type':'tool_use','id':call,'name':step[0],'input':inp}
    else:
     state['done']=True
     latest=request['messages'][-1];events.append({'phase':state['phase'],'at':time.time(),'message':latest})
     block={'type':'text','text':state['phase']+'-complete'}
   except Exception as e:errors.append(str(e));block={'type':'text','text':'fixture-failed'}
   body=sse(block,request['model']);self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
 server=ThreadingHTTPServer(('127.0.0.1',0),Provider);threading.Thread(target=server.serve_forever,daemon=True).start()
 common=['--claude','--model','claude-sonnet-5-5','--claude-api-key','synthetic-key','--claude-messages-url',f'http://127.0.0.1:{server.server_port}/v1/messages','--cwd',str(workspace),'--browser=none','--mcp-defaults','false','--mcp-codex-config','false','--web-search','false','--image-generation','false','--subagents','false','--memory','false']
 def start(label,cmd):
  master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',45,170,0,0));proc=subprocess.Popen(cmd,stdin=slave,stdout=slave,stderr=slave,cwd=workspace,env=env,start_new_session=True);os.close(slave);commands.append(cmd);processes.append(proc);transcripts[label]=bytearray();screens[label]=TerminalScreen()
  def drain():
   while select.select([master],[],[],0)[0]:
    try:data=os.read(master,65536)
    except OSError:break
    if not data:break
    transcripts[label].extend(data);screens[label].feed(data)
    if b'\x1b[6n' in data:os.write(master,b'\x1b[1;1R')
  return proc,master,drain
 def wait(check,drain,label,timeout=30):
  end=time.monotonic()+timeout
  while time.monotonic()<end:
   drain();require(not errors,'; '.join(errors))
   if check():return
   time.sleep(.03)
  raise AssertionError(label)
 def visible(label,text):return text in screens[label].text()
 def finish(proc,fd,drain):os.write(fd,b'\x03');proc.wait(timeout=10);drain();os.close(fd)
 outcome={'success':False}
 try:
  phase('initial',steps_initial());started=time.time();proc,fd,drain=start('initial',[str(binary)]+common+['--prompt','Schedule real clock test.'])
  wait(lambda:visible('initial','initial-complete'),drain,'initial final missing')
  # Unsubmitted composer input must block both due firings until removed.
  os.write(fd,b'unsent draft')
  until=started+65
  while time.time()<until:drain();require(not errors,'; '.join(errors));time.sleep(.03)
  require(not any('dynamic-real-clock-marker' in json.dumps(e['message']) and 'tool_result' not in json.dumps(e['message']) for e in events),'timer ignored draft')
  os.write(fd,b'\x15')
  def fired(marker):return any(marker in json.dumps(e['message']) and 'tool_result' not in json.dumps(e['message']) for e in events)
  wait(lambda:fired('cron-once-marker') and fired('dynamic-real-clock-marker'),drain,'normal idle firings absent',20)
  require(not fired('deleted-must-not-fire'),'deleted schedule fired');checks.append('ID-derived recurring half-interval/hourly cap jitter and :30 early/non-boundary exact one-shot receipts persisted');checks.append('real 60-second wakeup and cron fire only after composer cleared; deletion suppresses fire')
  phase('prepare-reopen',[('ScheduleWakeup',{'delaySeconds':60,'prompt':'discard-on-reopen','reason':'restart policy','noop':False},False,None)])
  os.write(fd,b'Prepare restart\r');wait(lambda:visible('initial','prepare-reopen-complete'),drain,'restart setup missing');finish(proc,fd,drain)
  manifests=list((codex_home/'claude/sessions').glob('*.json'));require(len(manifests)==1,'session manifest missing');session=json.loads(manifests[0].read_text())['id']
  # Simulate an offline gap through persisted records, never a production clock
  # override. The actual reopen must discard expired/elapsed work and skip backlog.
  journals=list((codex_home/'claude/schedules').glob('*.json'));require(len(journals)==1,'scheduler journal missing');jp=journals[0];journal=json.loads(jp.read_text());past=int(time.time())-120
  journal['tasks'][ids['expired']]['expires_at']=past
  journal['tasks'][ids['missed-one']]['next_fire_at']=past
  journal['tasks'][ids['retained']]['next_fire_at']=past
  # Seven-day age is a persisted fixture; expiry delivery still uses the real live clock.
  journal['tasks'][ids['final-expiry']]['created_at']=int(time.time())-7*86400+10
  journal['tasks'][ids['final-expiry']]['expires_at']=int(time.time())+10
  (artifact/'reopen-fixture.json').write_text(json.dumps(journal,indent=2));jp.write_text(json.dumps(journal))
  phase('reopen',[('CronList',{},False,check_tasks),('CronDelete',lambda:{'id':ids['retained']},False,None),('Monitor',{'command':'printf "monitor-stdout-marker\\n"; printf "monitor-stderr-marker\\n" >&2; sleep 120 & echo $! > monitor-child.pid; wait','description':'Synthetic process cancellation','persistent':True},False,monitor_id)])
  proc,fd,drain=start('reopen',[str(binary),'resume',session]+common+['--prompt','Inspect restored schedules and start monitor.']);wait(lambda:visible('reopen','reopen-complete'),drain,'reopen tools missing')
  wait(lambda:fired('monitor-stdout-marker'),drain,'monitor stdout idle event absent');checks.append('real process reopen retains cron, drops dynamic/seeded expired/missed one-shots, skips seeded backlog; Monitor stdout arrives at idle')
  wait(lambda:fired('final-seven-day-marker'),drain,'seven-day final fire absent',20);checks.append('persisted seven-day age fixture fires once at live expiry and is removed')
  phase('stop',[('CronList',{},False,lambda r:require(all(t['id']!=ids['final-expiry'] for t in r['tasks']),'expired final task still listed')),('TaskStop',lambda:{'task_id':ids['monitor']},False,stopped),('TaskOutput',lambda:{'task_id':ids['monitor'],'block':False},False,lambda r:(stopped(r),require('monitor-stderr-marker' in r['stderr'],'stderr absent')))])
  os.write(fd,b'Stop the monitor\r');wait(lambda:visible('reopen','stop-complete'),drain,'monitor stop missing');wait(lambda:any('Monitor finished' in json.dumps(e['message']) and 'stopped' in json.dumps(e['message']) for e in events),drain,'cancellation event absent')
  child=int((workspace/'monitor-child.pid').read_text());status=Path(f'/proc/{child}/stat');require(not status.exists() or status.read_text().split()[2]=='Z','descendant still alive');checks.append('TaskStop kills descendant, TaskOutput retains bounded stdout/stderr, real cancellation event delivered')
  # A second actual CLI adopts the retained session while the first UI remains
  # alive. The stale scheduler must stop before claiming the new owner's job.
  old_proc,old_fd,old_drain=proc,fd,drain
  phase('new-owner',[('CronCreate',{'cron':'7 * * * *','prompt':'new-owner-only-marker','recurring':False},False,remember('owner-job'))])
  proc,fd,drain=start('new-owner',[str(binary),'resume',session]+common+['--prompt','Adopt scheduler ownership.']);wait(lambda:visible('new-owner','new-owner-complete'),drain,'new owner not ready')
  wait(lambda:visible('reopen','Scheduler paused:'),old_drain,'old scheduler did not fence')
  with jp.with_suffix('.lock').open('r+') as lock:
   fcntl.flock(lock,fcntl.LOCK_EX);owned=json.loads(jp.read_text());owned['tasks'][ids['owner-job']]['next_fire_at']=int(time.time())-1;(artifact/'owner-due-fixture.json').write_text(json.dumps(owned,indent=2));temporary=jp.with_suffix('.fixture.tmp');temporary.write_text(json.dumps(owned));temporary.replace(jp)
  wait(lambda:fired('new-owner-only-marker'),drain,'new owner did not dispatch due fixture');old_drain();require(sum('new-owner-only-marker' in json.dumps(e['message']) and 'tool_result' not in json.dumps(e['message']) for e in events)==1,'ownership produced duplicate fire')
  require(ids['owner-job'] not in json.loads(jp.read_text())['tasks'],'new owner one-shot not consumed');finish(old_proc,old_fd,old_drain);finish(proc,fd,drain);checks.append('two live CLI processes: resume fences older scheduler before due fixture claim; newer owner fires exactly once')
  # Headless catalogs omit the timer and Monitor tools entirely.
  phase('headless',[]);r=subprocess.run([str(binary),'run']+common+['Inspect headless catalog.'],cwd=workspace,env=env,capture_output=True,timeout=30);require(r.returncode==0,'headless failed');names={t['name'] for t in requests[-1]['tools']};require(not names.intersection({'Monitor','CronCreate','CronList','CronDelete','ScheduleWakeup'}),'headless advertised idle tools');checks.append('headless omits idle-only tools')
  rules=artifact/'monitor-deny.json';rules.write_text(json.dumps({'permissions':{'defaultMode':'full-access','allow':['Monitor'],'deny':['Bash(touch *)']}}))
  for label,flags in [('monitor-plan',['--permission-mode','plan']),('monitor-deny',['--claude-permissions',str(rules)])]:
   phase(label,[('Monitor',{'command':'touch monitor-must-not-exist','description':'Denied effect'},True,None)])
   proc,fd,drain=start(label,[str(binary)]+common+flags+['--prompt','Verify Monitor admission.']);wait(lambda:visible(label,label+'-complete'),drain,'Monitor admission final absent');finish(proc,fd,drain);require(not(workspace/'monitor-must-not-exist').exists(),'Monitor bypassed admission')
  checks.append('plan blocks Monitor and Bash deny overrides Monitor allow before process spawn')
  outcome={'success':True,'checks':checks,'elapsed_seconds':round(time.time()-started,2),'binary_sha256':binary_sha256}
 finally:
  for proc in processes:
   if proc.poll() is None:proc.kill();proc.wait()
  for label,data in transcripts.items():
   (artifact/(label+'.pty')).write_bytes(data);(artifact/(label+'.screen.txt')).write_text(screens[label].text())
  (artifact/'events.json').write_text(json.dumps(events,indent=2));(artifact/'receipts.json').write_text(json.dumps(receipts,indent=2));(artifact/'scenario.json').write_text(json.dumps({'commands':commands,'environment':env,'boundary':'actual CLI/PTY/processes/wall clock/persistence; only Messages HTTP synthetic'},indent=2));(artifact/'outcome.json').write_text(json.dumps(outcome,indent=2));server.shutdown();print(json.dumps({'artifact':str(artifact),**outcome}))
if __name__=='__main__':main()
