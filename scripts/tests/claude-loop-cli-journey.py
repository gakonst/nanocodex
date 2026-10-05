#!/usr/bin/env python3
"""Actual CLI/PTy /loop journey. Only inference is synthetic.
Long fallback durations and seven-day age use clearly labeled persisted-journal
fixtures under the scheduler's file lock. This is not a real 20-minute wait.
"""
import shutil
import argparse, fcntl, hashlib, importlib.util, json, os, pty, select, struct, subprocess, termios, threading, time
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4
spec=importlib.util.spec_from_file_location('scheduler_journey',Path(__file__).with_name('claude-scheduler-monitor-cli-journey.py'))
h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
require,sse,text_of=h.require,h.sse,h.text_of

def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,default=Path('output/claude-loop-cli')/uuid4().hex);a=p.parse_args()
 artifact=a.output.resolve();artifact.mkdir(parents=True);workspace=artifact/'workspace';workspace.mkdir();home=artifact/'home';home.mkdir();codex_home=home/'codex';codex_home.mkdir();binary=artifact/'nanocodex-under-test';shutil.copy2(a.binary.resolve(),binary);binary.chmod(0o700);binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest();(workspace/'.claude').mkdir()
 rules=artifact/'permissions.json';rules.write_text(json.dumps({'permissions':{'defaultMode':'full-access','deny':['Skill(blocked)']}}))
 env={'HOME':str(home),'CODEX_HOME':str(codex_home),'PATH':'/usr/bin:/bin','TERM':'xterm-256color','NANOCODEX_COMPUTER':'off'}
 requests=[];errors=[];receipts=[];checks=[];fixtures=[];state={'phase':'ready','steps':[],'index':0,'pending':None};transcript=bytearray();screen=h.TerminalScreen();command=[]
 def phase(name,steps=[]):state.update(phase=name,steps=steps,index=0,pending=None)
 class Provider(BaseHTTPRequestHandler):
  def log_message(self,*_):pass
  def do_POST(self):
   req=json.loads(self.rfile.read(int(self.headers['content-length'])));requests.append({'phase':state['phase'],'request':req});(artifact/'provider.json').write_text(json.dumps(requests,indent=2))
   try:
    if state['phase']=='fixed' and state['index']==0:require('*/6 * * * *' in json.dumps(req['messages'][-1]),'frontend did not select 7m clean cadence')
    if state['pending']:
     call,step=state['pending'];rs=[b for m in req['messages'] for b in m.get('content',[]) if isinstance(b,dict) and b.get('type')=='tool_result' and b.get('tool_use_id')==call];require(len(rs)==1,'missing result '+call);r=rs[0];require(not r.get('is_error'),str(r));value=json.loads(text_of(r));receipts.append(value)
     if len(step)>2:step[2](value)
     state['pending']=None
    if state['index']<len(state['steps']):
     step=state['steps'][state['index']];call=state['phase']+str(state['index']);state['index']+=1;state['pending']=(call,step);block={'type':'tool_use','id':call,'name':step[0],'input':step[1]}
    else:block={'type':'text','text':state['phase']+'-complete'}
   except Exception as e:errors.append(str(e));block={'type':'text','text':'fixture-failed'}
   body=sse(block,req['model']);self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
 server=ThreadingHTTPServer(('127.0.0.1',0),Provider);threading.Thread(target=server.serve_forever,daemon=True).start()
 common=['--claude-permissions',str(rules),'--claude','--model','claude-sonnet-5-5','--claude-api-key','synthetic-key','--claude-messages-url',f'http://127.0.0.1:{server.server_port}/v1/messages','--cwd',str(workspace),'--browser=none','--mcp-defaults','false','--mcp-codex-config','false','--web-search','false','--image-generation','false','--subagents','false','--memory','false']
 master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',45,170,0,0));command=[str(binary)]+common+['--prompt','Start loop acceptance.'];proc=subprocess.Popen(command,stdin=slave,stdout=slave,stderr=slave,cwd=workspace,env=env,start_new_session=True);os.close(slave)
 def drain():
  while select.select([master],[],[],0)[0]:
   try:data=os.read(master,65536)
   except OSError:break
   if not data:break
   transcript.extend(data);screen.feed(data)
   if b'\x1b[6n' in data:os.write(master,b'\x1b[1;1R')
 def wait(check,label,timeout=20):
  end=time.monotonic()+timeout
  while time.monotonic()<end:
   drain();require(not errors,'; '.join(errors))
   if check():return
   time.sleep(.03)
  raise AssertionError(label)
 def completed():wait(lambda:state['phase']+'-complete' in screen.text(),'response missing '+state['phase'])
 def enter(text):os.write(master,text.encode()+b'\r')
 def journal_path():
  paths=list((codex_home/'claude/schedules').glob('*.json'));require(len(paths)==1,'one journal required');return paths[0]
 def journal():return json.loads(journal_path().read_text())
 def seed_due(label):
  path=journal_path()
  with path.with_suffix('.lock').open('r+') as lock:
   fcntl.flock(lock,fcntl.LOCK_EX);j=json.loads(path.read_text());require('wakeup' in j['tasks'],'fallback missing');j['tasks']['wakeup']['next_fire_at']=int(time.time())-1;fixtures.append({'label':label,'method':'persisted next_fire_at fixture; real production clock unchanged','journal':j});temp=path.with_suffix('.fixture.tmp');temp.write_text(json.dumps(j));temp.replace(path)
 def latest_prompt():return json.dumps(requests[-1]['request']['messages'][-1])
 outcome={'success':False}
 try:
  completed();(workspace/'.claude/loop.md').write_text('maintenance-first-marker\n')
  phase('maintenance-first');enter('/loop');completed();require('maintenance-first-marker' in latest_prompt(),'bare loop did not load project default')
  wait(lambda:'wakeup' in journal()['tasks'],'missing 20-minute fallback');j=journal();task=j['tasks']['wakeup'];require(1180<=task['next_fire_at']-time.time()<=1201,'fallback not 1200 seconds');require(j['wakeup_fallback_used'],'fallback not marked');checks.append('bare /loop reads project prompt and schedules one 1200-second fallback after unscheduled iteration')
  (workspace/'.claude/loop.md').write_text('maintenance-second-marker\n'+'x'*26000+'must-be-truncated-marker')
  phase('maintenance-second');seed_due('accelerate first 1200-second fallback');completed();require('maintenance-second-marker' in latest_prompt(),'loop.md was not read fresh');require('must-be-truncated-marker' not in latest_prompt(),'loop.md exceeded bound')
  wait(lambda:'wakeup' not in journal()['tasks'] and not journal()['wakeup_iteration_active'],'second unscheduled iteration did not end loop');checks.append('persisted fallback fixture reloads edited bounded loop.md; second unscheduled iteration ends loop')
  phase('custom',[('ScheduleWakeup',{'delaySeconds':1,'prompt':'custom-dynamic-marker','reason':'short chosen delay','noop':False},lambda r:require(r['delaySeconds']==60,'lower clamp missing'))]);enter('/loop custom-dynamic-marker');completed();require('maintenance-second-marker' not in latest_prompt(),'explicit task imported loop.md')
  wait(lambda:'wakeup' in journal()['tasks'],'custom wakeup missing');os.write(master,b'\x1b');wait(lambda:'wakeup' not in journal()['tasks'],'Esc did not cancel dynamic loop');checks.append('task-only loop schedules clamped 60-second dynamic wakeup; Esc cancels it')
  phase('fixed',[('CronCreate',{'cron':'*/6 * * * *','prompt':'fixed-marker','recurring':True})]);enter('/loop 7m fixed-marker');completed();j=journal();require(any(t.get('cron')=='*/6 * * * *' and t['prompt']=='fixed-marker' for t in j['tasks'].values()),'fixed loop schedule missing');require('wakeup' not in j['tasks'],'fixed loop created dynamic fallback');os.write(master,b'\x1b');time.sleep(.2);drain();require(any(t.get('cron')=='*/6 * * * *' for t in journal()['tasks'].values()),'Esc removed fixed cron');checks.append('7m fixed loop yields clean cron, persists cadence; Esc preserves fixed schedule')
  for name,disabled,body in [('allowed',False,'allowed-body-marker $ARGUMENTS'),('disabled',True,'disabled-body-must-not-load'),('blocked',False,'denied-body-must-not-load'),('clear',False,'builtin-shadow-must-not-load'),('override-off',False,'off-override-must-not-load'),('override-user',False,'user-only-override-must-not-load')]:
   folder=workspace/'.claude/skills'/name;folder.mkdir(parents=True);(folder/'SKILL.md').write_text('---\nname: '+name+'\ndescription: synthetic scheduled skill\ndisable-model-invocation: '+str(disabled).lower()+'\n---\n'+body+'\n')
  (workspace/'.claude/settings.local.json').write_text(json.dumps({'skillOverrides':{'override-off':'off','override-user':'user-invocable-only'}}))
  for name,expected,forbidden in [('allowed','allowed-edited-marker literal-arg','allowed-body-marker'),('disabled','/disabled','disabled-body-must-not-load'),('blocked','/blocked','denied-body-must-not-load'),('clear','/clear','builtin-shadow-must-not-load'),('override-off','/override-off','off-override-must-not-load'),('override-user','/override-user','user-only-override-must-not-load')]:
   phase('schedule-'+name,[('ScheduleWakeup',{'delaySeconds':60,'prompt':'/'+name+' literal-arg','reason':'scheduled skill provenance','noop':False})]);enter('Schedule provenance '+name);completed()
   if name=='allowed':(workspace/'.claude/skills/allowed/SKILL.md').write_text('---\nname: allowed\ndescription: edited scheduled skill\n---\nallowed-edited-marker $ARGUMENTS\n')
   phase('fire-'+name);seed_due('accelerate scheduled '+name);completed();require(expected in latest_prompt(),'scheduled expected content absent '+name);require(forbidden not in latest_prompt(),'withheld/stale skill body entered model '+name)
   os.write(master,b'\x1b');wait(lambda:'wakeup' not in journal()['tasks'],'cleanup dynamic '+name)
  checks.append('actual scheduled fires expand freshly edited MODEL-allowed skill; disabled/deny/settings-withheld/builtin commands remain plain text')
  (workspace/'.claude/loop.md').unlink();(home/'.claude').mkdir();(home/'.claude/loop.md').write_text('user-home-default-marker')
  phase('home-default');enter('/loop');completed();require('user-home-default-marker' in latest_prompt(),'user fallback not loaded');os.write(master,b'\x1b');wait(lambda:'wakeup' not in journal()['tasks'],'home loop cancellation missing');checks.append('missing project loop.md falls back to trusted user home')
  # New real CLI sessions prove automatic fallback cannot bypass Ask or Deny.
  os.write(master,b'\x03');proc.wait(timeout=10);drain()
  for admission in ['ask','deny']:
   case_rules=artifact/(admission+'-permissions.json');case_rules.write_text(json.dumps({'permissions':{'defaultMode':'full-access',admission:['ScheduleWakeup']}}))
   before=set((codex_home/'claude/schedules').glob('*.json'));case_flags=common.copy();case_flags[case_flags.index(str(rules))]=str(case_rules);phase('policy-'+admission)
   cm,cs=pty.openpty();fcntl.ioctl(cs,termios.TIOCSWINSZ,struct.pack('HHHH',45,170,0,0));cp=subprocess.Popen([str(binary)]+case_flags+['--prompt','/loop permission-fallback-marker'],stdin=cs,stdout=cs,stderr=cs,cwd=workspace,env=env,start_new_session=True);os.close(cs);case_screen=h.TerminalScreen();case_bytes=bytearray()
   try:
    end=time.monotonic()+20
    while time.monotonic()<end and state['phase']+'-complete' not in case_screen.text():
     if select.select([cm],[],[],.03)[0]:
      data=os.read(cm,65536);case_bytes.extend(data);case_screen.feed(data)
      if b'\x1b[6n' in data:os.write(cm,b'\x1b[1;1R')
    require(state['phase']+'-complete' in case_screen.text(),'policy loop did not finish '+admission);time.sleep(.2)
    added=set((codex_home/'claude/schedules').glob('*.json'))-before;require(len(added)==1,'policy scheduler journal missing');case_journal=json.loads(next(iter(added)).read_text());require('wakeup' not in case_journal['tasks'] and not case_journal['wakeup_iteration_active'],'automatic fallback bypassed '+admission);(artifact/(admission+'-journal.json')).write_text(json.dumps(case_journal,indent=2))
   finally:
    if cp.poll() is None:
     os.write(cm,b'\x03')
     try:cp.wait(timeout=10)
     except subprocess.TimeoutExpired:cp.kill();cp.wait()
    os.close(cm);(artifact/(admission+'.pty')).write_bytes(case_bytes);(artifact/(admission+'.screen.txt')).write_text(case_screen.text())
  checks.append('separate actual CLI Ask/Deny ScheduleWakeup sessions finish /loop without arming automatic fallback')
  outcome={'success':True,'checks':checks,'binary_sha256':binary_sha256,'timing_boundary':'20-minute fallback accelerated by explicit persisted journal fixture; no fake production clock'}
 finally:
  if proc.poll() is None:
   os.write(master,b'\x03')
   try:proc.wait(timeout=10)
   except subprocess.TimeoutExpired:proc.kill();proc.wait()
  drain();os.close(master);server.shutdown();(artifact/'terminal.pty').write_bytes(transcript);(artifact/'screen.txt').write_text(screen.text());(artifact/'receipts.json').write_text(json.dumps(receipts,indent=2));(artifact/'persisted-time-fixtures.json').write_text(json.dumps(fixtures,indent=2));(artifact/'scenario.json').write_text(json.dumps({'command':command,'environment':env},indent=2));(artifact/'outcome.json').write_text(json.dumps(outcome,indent=2));print(json.dumps({'artifact':str(artifact),**outcome}))
if __name__=='__main__':main()
