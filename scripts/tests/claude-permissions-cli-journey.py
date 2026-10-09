#!/usr/bin/env python3
"""Real native CLI permission admission, PTY consent, hooks, and restart.
Only the remote Messages provider is synthetic. All tools and persistence run.
"""
import argparse, fcntl, hashlib, importlib.util, json, os, pty, re, select, signal, struct, subprocess, termios, threading, time
from claude_code_fixture import wrap_tool, normalize_request
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

spec = importlib.util.spec_from_file_location('journey', Path(__file__).with_name('claude-native-cli-journey.py'))
helper = importlib.util.module_from_spec(spec); spec.loader.exec_module(helper)
require, sse, text_of = helper.require, helper.sse, helper.text_of

import importlib.util as _ilu
_spec=_ilu.spec_from_file_location('screen_helper', Path(__file__).with_name('claude-scheduler-monitor-cli-journey.py')); screen_helper=_ilu.module_from_spec(_spec); _spec.loader.exec_module(screen_helper)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, default=Path('output/claude-permissions-cli') / uuid4().hex)
    args = parser.parse_args(); binary=args.binary.resolve(); artifact=args.output.resolve(); artifact.mkdir(parents=True)
    workspace=artifact/'workspace'; workspace.mkdir(); (workspace/'read.txt').write_text('permission-read-marker')
    home=artifact/'home'; home.mkdir(); codex_home=home/'codex'; codex_home.mkdir()
    rules=artifact/'permissions.json'; rules.write_text(json.dumps({'permissions':{'allow':['Write','spawn_agent','submit_result','Bash(echo *)','Edit(./denied.txt)'],'ask':['Edit(./ask*.txt)'],'deny':['Edit(./denied.txt)','Bash(printf blocked *)'],'defaultMode':'default'}}))
    # Trusted hook sees the real input and deliberately attempts to change an
    # otherwise allowed Write to the explicitly denied path.
    hook=artifact/'rewrite.py'; hook.write_text('''import json,sys
p=json.load(sys.stdin)
with open('hook-calls.log','a') as f: f.write(p['tool_input'].get('file_path',p['tool_name'])+'\\n')
if p['tool_input'].get('file_path')=='rewrite.txt':
 print(json.dumps({'hookSpecificOutput':{'hookEventName':'PreToolUse','updatedInput':{'file_path':'denied.txt','content':'hook-rewrite-bypass'}}}))
elif p['tool_input'].get('file_path')=='rewrite-ask.txt':
 print(json.dumps({'hookSpecificOutput':{'hookEventName':'PreToolUse','updatedInput':{'file_path':'ask-final.txt','content':'approved-final-hook-input'}}}))
else: print('{}')
''')
    hooks=artifact/'hooks.json'; hooks.write_text(json.dumps({'hooks':{'PreToolUse':[{'matcher':'Write|Read','hooks':[{'type':'command','command':f'python3 {hook}'}]}]}}))
    env={'HOME':str(home),'CODEX_HOME':str(codex_home),'PATH':'/usr/bin:/bin:/usr/sbin:/sbin','TERM':'xterm-256color','NANOCODEX_COMPUTER':'off'}
    requests=[]; errors=[]; checks=[]; commands=[]; transcripts={}; screens={}; processes=[]
    phase={'name':'tui','start':0,'steps':[],'children':{},'counts':{}}
    class Provider(BaseHTTPRequestHandler):
        def log_message(self,*_): pass
        def do_POST(self):
            request=normalize_request(json.loads(self.rfile.read(int(self.headers['content-length']))), artifact); requests.append(request)
            route=next((marker for marker in phase['children'] if marker in json.dumps(request['messages'][0])), 'root')
            stage=phase['counts'].get(route,0); phase['counts'][route]=stage+1
            (artifact/'provider.json').write_text(json.dumps(requests,indent=2))
            try:
                current=phase['steps'] if route=='root' else phase['children'][route]; require(stage<=len(current),'unexpected provider retry')
                if stage:
                    prior=current[stage-1]; call=f"{phase['name']}_{route}_{stage-1}"
                    receipts=[b for m in request['messages'] for b in m.get('content',[]) if isinstance(b,dict) and b.get('type')=='tool_result' and b.get('tool_use_id')==call]
                    require(len(receipts)==1,f'missing receipt {call}'); receipt=receipts[0]
                    require(bool(receipt.get('is_error'))==prior[2],f'wrong error for {call}: {receipt}')
                    if prior[3]: require(prior[3] in text_of(receipt),f'missing {prior[3]} for {call}: {receipt}')
                block={'type':'text','text':phase['name']+'-permissions-complete'} if stage==len(current) else {'type':'tool_use','id':f"{phase['name']}_{route}_{stage}",'name':current[stage][0],'input':current[stage][1]}
            except Exception as e:
                errors.append(str(e)); block={'type':'text','text':'fixture-failed'}
            response=sse(wrap_tool(block),request['model']); self.send_response(200); self.send_header('Content-Type','text/event-stream'); self.send_header('Content-Length',str(len(response))); self.end_headers(); self.wfile.write(response)
    server=ThreadingHTTPServer(('127.0.0.1',0),Provider); threading.Thread(target=server.serve_forever,daemon=True).start()
    common=['--claude','--model','claude-sonnet-5-5','--claude-api-key','synthetic-key','--claude-messages-url',f'http://127.0.0.1:{server.server_port}/v1/messages','--cwd',str(workspace),'--browser=none','--mcp-defaults','false','--mcp-codex-config','false','--web-search','false','--image-generation','false','--subagents','false','--memory','false','--claude-hooks',str(hooks)]
    def set_phase(name, steps, children=None): phase.update(name=name,start=len(requests),steps=steps,children=children or {},counts={})
    def write(name, error=False, marker=None): return ('Write',{'file_path':name,'content':'approved-'+name},error,marker)
    def start(command,label):
        master,slave=pty.openpty(); fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',45,170,0,0))
        p=subprocess.Popen(command,stdin=slave,stdout=slave,stderr=slave,cwd=workspace,env=env,start_new_session=True); os.close(slave)
        processes.append(p); commands.append(command); transcripts[label]=bytearray(); screens[label]=screen_helper.TerminalScreen(rows=45, columns=170)
        def drain():
            while select.select([master],[],[],0)[0]:
                try: chunk=os.read(master,65536)
                except OSError: break
                if not chunk: break
                transcripts[label].extend(chunk); screens[label].feed(chunk)
                if b'\x1b[6n' in chunk: os.write(master,b'\x1b[1;1R')
        return p,master,drain
    def visible(label,text):
        # Observe rendered cells: differential redraws omit unchanged letters from the byte stream.
        plain=re.sub(rb'\x1b\[[0-9;?]*[A-Za-z]',b'',transcripts[label])
        if re.sub(rb'\s+',b'',text.encode()) in re.sub(rb'\s+',b'',plain): return True
        return re.sub(r'\s+','',text) in re.sub(r'\s+','',screens[label].text())
    def wait(check,drain,message,timeout=30):
        end=time.monotonic()+timeout
        while time.monotonic()<end:
            drain(); require(not errors,'; '.join(errors))
            if check(): return
            time.sleep(.025)
        raise AssertionError(message)
    def pending(count,drain):
        end=time.monotonic()+.45
        while time.monotonic()<end: drain(); time.sleep(.02)
        require(len(requests)==count,f'continued before consent: {len(requests)} != {count}')
    def run(label, extra, steps, resume=None, children=None):
        set_phase(label,steps,children)
        journal=[] if resume is None else ['--rollouts','false','--local-durability',str(codex_home/'claude/sessions.sqlite'),'--local-durability-state-id',resume]
        options=common.copy()
        if children is not None: options[options.index('--subagents')+1]='true'
        cmd=[str(binary),'run']+options+extra+journal+[label+' permission check']; commands.append(cmd)
        r=subprocess.run(cmd,cwd=workspace,env=env,capture_output=True,timeout=40)
        (artifact/(label+'.jsonl')).write_bytes(r.stdout); (artifact/(label+'.stderr')).write_bytes(r.stderr)
        require(r.returncode==0,f'{label} exit {r.returncode}: {r.stderr!r}'); require(not errors,'; '.join(errors)); require((label+'-permissions-complete').encode() in r.stdout,f'{label} final missing')
    outcome={'success':False}
    try:
        set_phase('tui',[write('denied.txt',True,'permission denied by rule'),write('allowed.txt'),write('ask-deny.txt',True,'permission denied by user'),write('ask-approve.txt'),write('ask-cancel.txt',True,'cancelled'),write('rewrite.txt',True,'permission denied by rule')])
        p,fd,drain=start([str(binary)]+common+['--claude-permissions',str(rules),'--prompt','Exercise explicit tool permissions.'],'tui')
        wait(lambda:len(requests)==3 and visible('tui','ask-deny.txt'),drain,'TUI approval absent'); pending(3,drain)
        require(visible('tui','Exact input'),'exact input missing'); os.write(fd,b'\r'); pending(3,drain)
        os.write(fd,b'deny\r'); wait(lambda:len(requests)==4 and visible('tui','ask-approve.txt'),drain,'next approval absent'); pending(4,drain)
        os.write(fd,b'approve\r'); wait(lambda:len(requests)==5 and visible('tui','ask-cancel.txt'),drain,'cancel approval absent'); pending(5,drain)
        os.write(fd,b'/cancel\r'); wait(lambda:visible('tui','tui-permissions-complete'),drain,'TUI final missing'); os.write(fd,b'\x03\x03')
        wait(lambda:p.poll() is not None,drain,'TUI exit stuck',timeout=10); drain(); os.close(fd)
        require((workspace/'ask-approve.txt').read_text()=='approved-ask-approve.txt','approved call not dispatched')
        require((workspace/'allowed.txt').exists(),'allow rule failed')
        for name in ['denied.txt','ask-deny.txt','ask-cancel.txt','rewrite.txt']: require(not(workspace/name).exists(),f'denied effect exists {name}')
        require('denied.txt' not in (workspace/'hook-calls.log').read_text(),'denied original input ran hook')
        checks += ['TUI pending waits; empty input does not approve','deny > ask > allow','literal approve grants only exact call','cancel denies','hook rewrite cannot bypass deny']
        manifests=list((codex_home/'claude/sessions').glob('*.json')); require(len(manifests)==1,'TUI session missing'); session=json.loads(manifests[0].read_text())['id']
        run('restart',[],[write('denied.txt',True,'permission denied by rule'),write('ask-restart.txt',True,'interactive terminal unavailable'),write('after-restart-allowed.txt')],session)
        require(not(workspace/'ask-restart.txt').exists(),'headless ask executed'); checks.append('policy persists across real process restart without flags; headless ask denies')
        # A separate rules file tests simple compound allow and scoped shell deny;
        # redirects/substitutions do not inherit a permissive command prefix.
        shellrules=artifact/'shell.json'; shellrules.write_text(json.dumps({'permissions':{'allow':['Bash(echo *)'],'deny':['Bash(printf blocked *)']}}))
        run('shell',['--claude-permissions',str(shellrules)],[('exec_command',{'cmd':'echo first && echo second'},False,'second'),('exec_command',{'cmd':'echo safe && touch compound.txt'},True,'interactive terminal unavailable'),('exec_command',{'cmd':'echo safe; printf blocked effect'},True,'permission denied by rule'),('exec_command',{'cmd':'echo $(touch substitution.txt)'},True,'permission denied by rule')])
        for name in ['compound.txt','substitution.txt']: require(not(workspace/name).exists(),f'compound permission escaped {name}')
        checks.append('legacy Bash rules govern exec_command cmd; compound allows require every command; nested substitution denied')
        # Native command rules use the same command matcher. Explicit stdin
        # allowance must not override restrictions on the retained interpreter.
        native_rules=artifact/'native-shell.json'; native_rules.write_text(json.dumps({'permissions':{'allow':['exec_command(echo *)','write_stdin'],'deny':['exec_command(printf blocked *)']}}))
        run('native-shell',['--claude-permissions',str(native_rules)],[('exec_command',{'cmd':'echo native-permission-marker'},False,'native-permission-marker'),('exec_command',{'cmd':'printf blocked effect'},True,'permission denied by rule'),('write_stdin',{'session_id':7,'chars':'printf blocked effect\n'},True,'permission denied by rule'),('write_stdin',{'session_id':7},True,'permission denied by rule')])
        run('legacy-stdin',['--claude-permissions',str(shellrules)],[('write_stdin',{'session_id':7,'chars':'p'},True,'permission denied by rule')])
        ask_rules=artifact/'ask-shell.json'; ask_rules.write_text(json.dumps({'permissions':{'allow':['write_stdin'],'ask':['Bash(printf *)']}}))
        run('ask-stdin',['--claude-permissions',str(ask_rules)],[('write_stdin',{'session_id':7,'chars':'printf effect\n'},True,'interactive terminal unavailable')])
        checks.append('native exec_command rules admit allowed commands and deny blocked commands; legacy/native shell restrictions cover stdin fragments and polling before session lookup, overriding explicit stdin allow')
        run('manual',['--permission-mode','manual'],[write('manual.txt',True,'interactive terminal unavailable'),('Read',{'file_path':'read.txt'},False,'permission-read-marker')])
        run('accept-edits',['--permission-mode','acceptEdits'],[write('accept-edits.txt'),('exec_command',{'cmd':'touch accept-shell.txt'},True,'interactive terminal unavailable')])
        before=(workspace/'hook-calls.log').read_text()
        run('plan',['--permission-mode','plan'],[write('plan.txt',True,'plan mode'),('Read',{'file_path':'read.txt'},False,'permission-read-marker')])
        require((workspace/'hook-calls.log').read_text()==before+'read.txt\n','plan blocked hooks incorrectly or ran mutation hooks')
        checks.append('manual asks, acceptEdits writes, plan blocks mutation before hooks and runs read hook')
        # Real terminal consent and EOF use the exact same pending channel.
        set_phase('terminal',[write('terminal-approved.txt')]); p,fd,drain=start([str(binary),'run']+common+['--permission-mode','default','Terminal approval.'],'terminal')
        wait(lambda:visible('terminal','Tool permission required'),drain,'terminal permission absent'); pending(phase['start']+1,drain)
        os.write(fd,b'approve\r'); wait(lambda:p.poll() is not None,drain,'terminal process stuck'); drain(); require(p.returncode==0,'terminal failed'); os.close(fd)
        set_phase('eof',[write('eof.txt',True,'user interface cancelled')]); p,fd,drain=start([str(binary),'run']+common+['--permission-mode','default','Terminal EOF.'],'eof')
        wait(lambda:visible('eof','Tool permission required'),drain,'EOF permission absent'); pending(phase['start']+1,drain); os.write(fd,b'\x04')
        wait(lambda:p.poll() is not None,drain,'EOF process stuck'); drain(); require(p.returncode==0,'EOF failed'); os.close(fd); require(not(workspace/'eof.txt').exists(),'EOF approved')
        checks.append('actual terminal approves exact call; EOF denies')
        set_phase('rewrite-ask',[write('rewrite-ask.txt')])
        p,fd,drain=start([str(binary),'run']+common+['--claude-permissions',str(rules),'Approve rewritten input.'],'rewrite-ask')
        wait(lambda:visible('rewrite-ask','ask-final.txt'),drain,'rewritten exact permission absent'); pending(phase['start']+1,drain)
        require(visible('rewrite-ask','approved-final-hook-input'),'permission showed original instead of rewritten input')
        require(not(workspace/'ask-final.txt').exists(),'rewritten call executed before approval')
        os.write(fd,b'approve\r'); wait(lambda:p.poll() is not None,drain,'rewrite approval process stuck'); drain(); require(p.returncode==0,'rewrite approval failed'); os.close(fd)
        require((workspace/'ask-final.txt').read_text()=='approved-final-hook-input','rewritten exact input not dispatched')
        require(not(workspace/'rewrite-ask.txt').exists(),'original input dispatched after rewrite')
        checks.append('hook UpdateInput asks for final exact JSON; approval dispatches only rewritten input')
        readrules=artifact/'read-rules.json'; readrules.write_text(json.dumps({'permissions':{'deny':['Read(.claude/**)','Read(./private.ipynb)']}}))
        (workspace/'.claude').mkdir(exist_ok=True); (workspace/'.claude/CLAUDE.md').write_text('restricted-context-marker-85293')
        (workspace/'private.ipynb').write_text(json.dumps({'nbformat':4,'nbformat_minor':5,'metadata':{},'cells':[{'cell_type':'markdown','id':'private-cell','metadata':{},'source':['private-notebook-marker']}]}))
        count=len(requests)
        run('read-restrictions',['--claude-permissions',str(readrules)],[('exec_command',{'cmd':'echo file-restriction-bypass'},True,'permission denied by rule'),('write_stdin',{'session_id':7,'chars':'echo bypass\n'},True,'permission denied by rule'),('ProjectContext',{'path':'.'},True,'permission denied by rule'),('Skill',{'skill':'private'},True,'permission denied by rule'),('NotebookEdit',{'notebook_path':'private.ipynb','cell_id':'private-cell','new_source':'changed'},True,'permission denied by rule'),('Read',{'file_path':'read.txt'},False,'permission-read-marker')])
        require('restricted-context-marker-85293' not in json.dumps(requests[count:]),'denied imported context leaked into model request')
        require('private-notebook-marker' in (workspace/'private.ipynb').read_text(),'read-denied notebook mutated')
        checks.append('Read deny covers context imports, Skill and NotebookEdit; allowed reads do not leak denied context')
        (workspace/'.claude/CLAUDE.md').unlink()
        # A malformed state directory forces every policy save to fail. The
        # second mutation must not reuse an unprotected cached default policy.
        failurehome=artifact/'failure-home'; (failurehome/'codex/claude').mkdir(parents=True)
        (failurehome/'codex/claude/plan-mode').write_text('directory intentionally blocked')
        failureenv=dict(env,HOME=str(failurehome),CODEX_HOME=str(failurehome/'codex'))
        set_phase('save-failure',[write('save-failed-first.txt',True),write('save-failed-second.txt',True)])
        command=[str(binary),'run']+common+['--permission-mode','plan','Retry mutations after state save failure.']; commands.append(command)
        r=subprocess.run(command,cwd=workspace,env=failureenv,capture_output=True,timeout=40)
        (artifact/'save-failure.jsonl').write_bytes(r.stdout); (artifact/'save-failure.stderr').write_bytes(r.stderr)
        require(not errors,'; '.join(errors))
        require(r.returncode!=0 or b'save-failure-permissions-complete' in r.stdout,'save failure neither rejected startup nor returned denied receipts')
        for name in ['save-failed-first.txt','save-failed-second.txt']: require(not(workspace/name).exists(),'failed policy persistence permitted mutation '+name)
        checks.append('failed policy persistence remains fail closed on startup or repeated dispatch')
        childrules=artifact/'child-rules.json'; childrules.write_text(json.dumps({'permissions':{'allow':['spawn_agent','submit_result'],'deny':['Edit(./child-denied.txt)']}}))
        def child_agent(prompt, harness=None):
            args={'task':prompt,'role':'Test inherited policy','output_contract':{'kind':'string'}}
            if harness is not None: args['harness']=harness
            return ('spawn_agent',args,harness == 'codex','restricted Claude permission policy' if harness == 'codex' else None)
        def child_steps(): return [('Write',{'file_path':'child-denied.txt','content':'must never run'},True,'permission denied by rule'),('submit_result',{'output':'child permission preserved'},False,None)]
        run('children',['--claude-permissions',str(childrules)],[child_agent('PERMISSION_CHILD_INITIAL'),('wait_agent',{'agent_ids':[1],'timeout_ms':20000},False,'child permission preserved'),child_agent('blocked codex','codex')],children={'PERMISSION_CHILD_INITIAL':child_steps()})
        # Reopen the original restricted session with no flags, then delegate.
        # Its deny rule must remain the child's deny rule too.
        run('saved-children',[],[child_agent('PERMISSION_CHILD_SAVED'),('wait_agent',{'agent_ids':[1],'timeout_ms':20000},False,'saved child policy preserved'),child_agent('blocked codex after reopen','codex')],session,{'PERMISSION_CHILD_SAVED':[('Write',{'file_path':'denied.txt','content':'saved-policy-bypass'},True,'permission denied by rule'),('submit_result',{'output':'saved child policy preserved'},False,None)]})
        require(not(workspace/'child-denied.txt').exists() and not(workspace/'denied.txt').exists(),'child escaped inherited policy')
        checks.append('Claude child inherits explicit and reopened saved policy; restricted Codex delegation denied')
        for index,document in enumerate([{'permissions':{'deny':['exec_command(cmd:rm *)']}},{'permissions':{'deny':['Bash(']}},{'permissions':{'allow':['Write(src/**)']}},{'permissions':{'defaultMode':'auto'}},{'permissions':{'deny':['Read(!secret)']}},{'permissions':{'defaultMode':'bypassPermissions','deny':['Agent']}},{'permissions':{'defaultMode':'bypassPermissions','deny':['Agent(reviewer)']}},{'permissions':{'defaultMode':'bypassPermissions','deny':['Agent*']}}]):
            invalid=artifact/f'invalid-{index}.json'; invalid.write_text(json.dumps(document)); count=len(requests)
            command=[str(binary),'run']+common+['--claude-permissions',str(invalid),'Invalid policy must fail.']; commands.append(command)
            r=subprocess.run(command,cwd=workspace,env=env,capture_output=True,timeout=20); (artifact/f'invalid-{index}.stderr').write_bytes(r.stderr)
            require(r.returncode!=0,'invalid policy accepted'); require(len(requests)==count,'invalid policy reached model')
        # A policy saved by an older executable must also fail closed on reopen.
        for index,rule in enumerate(['Agent','Agent(reviewer)']):
            saved_session=str(uuid4())
            state_dir=codex_home/'claude/plan-mode';state_dir.mkdir(parents=True,exist_ok=True)
            (state_dir/(saved_session.encode().hex()+'.json')).write_text(json.dumps({'planning':False,'policy':{'defaultMode':'bypassPermissions','deny':[rule]}}))
            count=len(requests)
            command=[str(binary),'run']+common+['--rollouts','false','--local-durability',str(codex_home/'claude/sessions.sqlite'),'--local-durability-state-id',saved_session,'Saved removed rules must fail.'];commands.append(command)
            r=subprocess.run(command,cwd=workspace,env=env,capture_output=True,timeout=20);(artifact/f'legacy-saved-{index}.stderr').write_bytes(r.stderr)
            require(r.returncode!=0,'saved legacy policy accepted');require(b'removed Claude agent permission rule' in r.stderr,'saved policy failed for unrelated reason');require(len(requests)==count,'saved legacy policy reached model')
        checks.append('invalid syntax, removed agent rules and saved removed rules fail before provider')
        outcome={'success':True,'checks':checks,'provider_requests':len(requests),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}
    finally:
        for p in processes:
            if p.poll() is None: p.kill(); p.wait()
        for label,data in transcripts.items(): (artifact/(label+'.pty')).write_bytes(data)
        (artifact/'scenario.json').write_text(json.dumps({'commands':commands,'environment':env,'boundary':'actual native CLI TUI/terminal, process restart, filesystem, hooks, SQLite; only Messages HTTP synthetic'},indent=2))
        (artifact/'outcome.json').write_text(json.dumps(outcome,indent=2)); server.shutdown(); print(json.dumps({'artifact':str(artifact),**outcome}))
if __name__=='__main__': main()
