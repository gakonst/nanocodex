#!/usr/bin/env python3
"""Actual CLI profile/skill/isolation lifecycle; only external inference is synthetic."""
import argparse, json, os, subprocess, threading, time, traceback, pty, select, fcntl, termios, struct
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from uuid import uuid4


def require(value, message):
    if not value: raise AssertionError(message)

def text_of(result):
    content = result.get('content', '')
    return content if isinstance(content, str) else ''.join(v.get('text', '') for v in content)

def sse(block, model):
    tool = block['type'] == 'tool_use'
    start = dict(block, input={}) if tool else {'type':'text','text':''}
    delta = {'type':'input_json_delta','partial_json':json.dumps(block['input'])} if tool else {'type':'text_delta','text':block['text']}
    events = [{'type':'message_start','message':{'id':'fixture','role':'assistant','model':model,'content':[],'usage':{'input_tokens':1,'output_tokens':0}}}, {'type':'content_block_start','index':0,'content_block':start}, {'type':'content_block_delta','index':0,'delta':delta}, {'type':'content_block_stop','index':0}, {'type':'message_delta','delta':{'stop_reason':'tool_use' if tool else 'end_turn'},'usage':{'output_tokens':1}}, {'type':'message_stop'}]
    return ''.join('data: '+json.dumps(v)+'\n\n' for v in events).encode()

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',required=True,type=Path);p.add_argument('--output',type=Path,default=Path('output/claude-profiles-cli')/uuid4().hex);a=p.parse_args()
    artifact=a.output.resolve();workspace=artifact/'workspace';workspace.mkdir(parents=True);home=artifact/'home';home.mkdir();binary=a.binary.resolve()
    def write(path,text):
        file=workspace/path;file.parent.mkdir(parents=True,exist_ok=True);file.write_text(text)
    def git(*args): return subprocess.check_output(['git',*args],cwd=workspace,text=True).strip()
    write('tracked.txt','parent-original\n');write('.gitignore','.claude/worktrees/\n')
    write('CLAUDE.md','ROOT_CONTEXT_SECRET not available when Read is blocked.\n')
    write('.claude/agents/reviewer.md','---\nname: reviewer\ndescription: Restricted reviewer\nmodel: haiku\ntools: Read, Agent, TaskOutput\n---\nPROFILE_INSTRUCTIONS_SENTINEL\n')
    write('.claude/agents/no-read.md','---\nname: no-read\ndescription: No file context\ntools: Grep\n---\nNO_READ_PROFILE_SENTINEL\n')
    write('.claude/agents/planner.md','---\nname: planner\ndescription: Plan-only profile\npermissionMode: plan\n---\nPLAN_PROFILE_SENTINEL\n')
    write('.claude/agents/isolated.md','---\nname: isolated\ndescription: Isolated work\nisolation: worktree\ntools: Read, Write, Bash\n---\nISOLATED_PROFILE_SENTINEL\n')
    write('.claude/agents/malformed.md','---\nname: malformed\ndescription: Invalid elevation\npermissionMode: bypassPermissions\n---\nNo.\n')
    write('.claude/skills/fork-review/SKILL.md','---\nname: fork-review\ndescription: Review in a real clean child\ncontext: fork\nagent: reviewer\nmodel: haiku\n---\nSKILL_CHILD_MARKER $ARGUMENTS\n')
    write('.claude/skills/disabled/SKILL.md','---\nname: disabled\ndisable-model-invocation: true\ncontext: fork\n---\nNever run.\n')
    git('init','-q');git('config','user.email','fixture@example.invalid');git('config','user.name','Synthetic Fixture');git('add','.');git('commit','-qm','fixture base');base=git('rev-parse','HEAD')
    requests=[];receipts=[];errors=[];commands=[];checks=[];streams={};ids={};paths={};lock=threading.RLock();phase={'name':'main'}; resumed=threading.Event()
    def save_agent(key):
        def check(value):
            ids[key]=value['task_id']
            if value.get('isolation'):
                info=value['isolation']; paths[key]=Path(info['workspace']); require(paths[key]!=workspace,'child did not isolate');require(paths[key].is_dir(),'missing child worktree')
        return check
    def completed(value): require('completed' in json.dumps(value),'child result missing completion: '+str(value))
    def denied(value): pass
    def clean_closed(value): require(not paths['clean'].exists(),'unchanged child worktree survived CloseAgent');checks.append('unchanged worktree safely removed')
    def dirty_closed(value): require((paths['dirty']/'isolated.txt').read_text()=='child-only','dirty worktree lost');require('kept' in json.dumps(value),'missing retained receipt');checks.append('dirty worktree retained')
    def commit_closed(value): require(paths['committed'].exists(),'committed worktree lost');require('new commits' in json.dumps(value),'committed preservation reason missing');checks.append('committed worktree retained')
    def catalog(value):
        names=[v['name'] for v in value['profiles']];require('reviewer' in names and 'isolated' in names and 'malformed' not in names,'invalid profile catalog');require(value['diagnostics'],'missing invalid-definition diagnostic')
    def spawn(marker,**extra): return {'prompt':marker,'description':'Fixture child',**extra}
    def step(name,inp,error=False,check=None):return (name,inp,error,check)
    root=[step('ListAgentProfiles',{},check=catalog),step('Agent',spawn('MISSING',subagent_type='missing'),True),
          step('Agent',spawn('CONFLICT',subagent_type='reviewer',model='sonnet'),True),step('Agent',spawn('CROSS',subagent_type='reviewer',harness='codex'),True),
          step('Agent',spawn('PROFILE_CHILD_MARKER',subagent_type='reviewer'),check=lambda v:(save_agent('profile')(v),completed(v))),
          step('Skill',{'skill':'fork-review','args':'SKILL_ARGUMENT_LITERAL'},check=completed),step('Skill',{'skill':'disabled'},True),
          step('Agent',spawn('PLAN_CHILD_MARKER',subagent_type='planner'),check=completed),
          step('Agent',spawn('NO_READ_CHILD_MARKER',subagent_type='no-read'),check=completed),
          step('Agent',spawn('CLEAN_CHILD_MARKER',subagent_type='isolated'),check=lambda v:(save_agent('clean')(v),completed(v))),
          step('Bash',{'command':'pwd; cat tracked.txt'},check=lambda v:require(str(workspace) in json.dumps(v) and 'parent-original' in json.dumps(v),'parent workspace changed')),
          step('CloseAgent',lambda:{'task_id':ids['clean']},check=clean_closed),
          step('Agent',spawn('TREE_PARENT_MARKER',isolation='worktree'),check=lambda v:(save_agent('tree')(v),completed(v))),
          step('CloseAgent',lambda:{'task_id':ids['tree']},check=lambda v:require(not paths['tree'].exists(),'nested pinned subtree not cleaned on close')),
          step('Agent',spawn('DIRTY_CHILD_MARKER',isolation='worktree'),check=lambda v:(save_agent('dirty')(v),completed(v))),
          step('CloseAgent',lambda:{'task_id':ids['dirty']},check=dirty_closed),
          step('Agent',spawn('COMMITTED_CHILD_MARKER',isolation='worktree'),check=lambda v:(save_agent('committed')(v),completed(v))),
          step('CloseAgent',lambda:{'task_id':ids['committed']},check=commit_closed),
          step('Agent',lambda:spawn('resume override denied',resume=ids['profile'],subagent_type='isolated'),True),
          step('CloseAgent',lambda:{'task_id':ids['profile']}),step('Agent',lambda:spawn('closed resume denied',resume=ids['profile']),True)]
    child={
        'RESUME_PROFILE_MARKER':[step('Write',{'file_path':'resume-forbidden.txt','content':'must not write'},True),step('SubmitResult',{'output':'resume-first-complete'})],
        'PLAN_CHILD_MARKER':[step('Write',{'file_path':'plan-forbidden.txt','content':'must not write'},True),step('SubmitResult',{'output':'plan-complete'})],
        'TREE_PARENT_MARKER':[step('Agent',spawn('TREE_NESTED_MARKER',isolation='worktree'),check=completed),step('SubmitResult',{'output':'nested-tree-parent-complete'})],
        'TREE_NESTED_MARKER':[step('Bash',{'command':'pwd'}),step('SubmitResult',{'output':'nested-tree-complete'})],
        'PROFILE_CHILD_MARKER':[step('Write',{'file_path':'forbidden.txt','content':'must not write'},True),step('Agent',spawn('ILLEGAL_MODEL_OVERRIDE',model='sonnet'),True),step('Agent',spawn('NESTED_CHILD_MARKER'),check=completed),step('SubmitResult',{'output':'profile-child-complete'})],
        'NESTED_CHILD_MARKER':[step('Write',{'file_path':'nested-forbidden.txt','content':'must not write'},True),step('SubmitResult',{'output':'nested-complete'})],
        'SKILL_CHILD_MARKER':[step('Write',{'file_path':'skill-forbidden.txt','content':'must not write'},True),step('SubmitResult',{'output':'skill-complete'})],
        'NO_READ_CHILD_MARKER':[step('Grep',{'pattern':'parent-original','path':'tracked.txt'},check=lambda v:require('ROOT_CONTEXT_SECRET' not in json.dumps(v),'Grep leaked blocked project context')),step('SubmitResult',{'output':'no-read-complete'})],
        'CLEAN_CHILD_MARKER':[step('Bash',{'command':'pwd'},check=lambda v:require('.claude/worktrees/agent-' in json.dumps(v),'isolation callback did not run')),step('SubmitResult',{'output':'clean-complete'})],
        'DIRTY_CHILD_MARKER':[step('Write',{'file_path':'isolated.txt','content':'child-only'}),step('SubmitResult',{'output':'dirty-complete'})],
        'COMMITTED_CHILD_MARKER':[step('Bash',{'command':'printf committed > committed.txt; git add committed.txt; git commit -qm child-commit'}),step('SubmitResult',{'output':'committed-complete'})],
    }
    class Provider(BaseHTTPRequestHandler):
        def log_message(self,*_):pass
        def do_POST(self):
            body=json.loads(self.rfile.read(int(self.headers['content-length'])))
            with lock:
                try:
                    first=json.dumps(body['messages'][0]); key=next((k for k in child if k in first),'root'); is_resume=key=='RESUME_PROFILE_MARKER' and 'RESUME_SECOND_PASS' in json.dumps(body['messages']); streamkey=key+(':resumed' if is_resume else '');stage=streams.setdefault((phase['name'],streamkey),{'index':0,'pending':None});requests.append({'phase':phase['name'],'stream':key,'body':body})
                    if key!='root':
                        require('ROOT_HISTORY_SECRET' not in json.dumps(body['messages']),'clean child inherited parent transcript')
                        if key in ('PROFILE_CHILD_MARKER','NESTED_CHILD_MARKER','SKILL_CHILD_MARKER','RESUME_PROFILE_MARKER'):
                            require(body['model']=='claude-haiku-4-5','profile model was not enforced');require('PROFILE_INSTRUCTIONS_SENTINEL' in json.dumps(body.get('system')),'admission lost profile instructions')
                        if key=='SKILL_CHILD_MARKER':require('SKILL_ARGUMENT_LITERAL' in first,'skill arguments absent')
                        if key=='NO_READ_CHILD_MARKER':require('ROOT_CONTEXT_SECRET' not in json.dumps(body.get('system')),'startup leaked blocked project context')
                    if stage['pending']:
                        call,previous=stage['pending'];results=[b for m in body['messages'] if isinstance(m.get('content'),list) for b in m['content'] if b.get('type')=='tool_result' and b.get('tool_use_id')==call];require(len(results)==1,'missing receipt '+call);result=results[0];receipts.append({'stream':key,'result':result});require(bool(result.get('is_error'))==previous[2],'wrong error '+call+': '+text_of(result))
                        value=text_of(result)
                        try:value=json.loads(value)
                        except ValueError:pass
                        if previous[3]:previous[3](value)
                        stage['pending']=None
                    plan=root if key=='root' else child[key]
                    if phase['name']=='resume' and key=='root':
                        plan=[step('Agent',spawn('RESUME_PROFILE_MARKER',subagent_type='reviewer'),check=lambda v:(save_agent('resumable')(v),completed(v))),step('Write',{'file_path':'.claude/agents/reviewer.md','content':'---\nname: reviewer\ndescription: edited profile\nmodel: sonnet\ntools: Write\n---\nCHANGED_PROFILE_SENTINEL\n'}),step('Agent',lambda:spawn('RESUME_SECOND_PASS',resume=ids['resumable']))]
                    if is_resume:
                        plan=[step('Write',{'file_path':'resume-forbidden.txt','content':'must not write'},True),step('SubmitResult',{'output':'resume-second-complete'},check=lambda v:resumed.set())]
                    if phase['name']=='read-denied':plan=[step('ListAgentProfiles',{},True),step('Agent',spawn('SHOULD_NOT_RUN',subagent_type='reviewer'),True),step('Skill',{'skill':'fork-review'},True)]
                    if stage['index']<len(plan):
                        action=plan[stage['index']];call=f"{phase['name']}-{streamkey}-{stage['index']}";stage['index']+=1;stage['pending']=(call,action);inp=action[1]() if callable(action[1]) else action[1];block={'type':'tool_use','id':call,'name':action[0],'input':inp}
                    else:block={'type':'text','text':'profiles-journey-complete'}
                except Exception as e:errors.append(traceback.format_exc());block={'type':'text','text':'fixture-error'}
            data=sse(block,body['model']);self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    server=ThreadingHTTPServer(('127.0.0.1',0),Provider);threading.Thread(target=server.serve_forever,daemon=True).start()
    env={'HOME':str(home),'CODEX_HOME':str(home/'codex'),'PATH':'/usr/bin:/bin','NANOCODEX_COMPUTER':'off'}
    common=[str(binary),'run','--claude','--model','claude-sonnet-5-5','--thinking','medium','--claude-api-key','synthetic-key','--claude-messages-url',f'http://127.0.0.1:{server.server_port}/v1/messages','--cwd',str(workspace),'--rollouts','false','--browser=none','--mcp-defaults','false','--mcp-codex-config','false','--web-search','false','--image-generation','false','--subagents','true','--memory','false']
    outcome={'success':False}
    try:
        for name,flags in [('main',[]),('read-denied',['--claude-permissions',str(artifact/'read-deny.json')])]:
            (artifact/'read-deny.json').write_text(json.dumps({'permissions':{'defaultMode':'full-access','deny':['Read']}}));phase['name']=name;cmd=common+flags+['ROOT_HISTORY_SECRET Exercise authorized profile child journeys.'];commands.append({'argv':cmd,'environment':env});result=subprocess.run(cmd,cwd=workspace,env=env,capture_output=True,text=True,timeout=110);(artifact/(name+'.stdout')).write_text(result.stdout);(artifact/(name+'.stderr')).write_text(result.stderr);require(result.returncode==0,'CLI failed: '+result.stderr);require(not errors,'\n'.join(errors));require('profiles-journey-complete' in result.stdout,'missing final result')
        # Retain a real terminal session so deferred delegation can run after the
        # parent turn finishes. Edit the project definition before resuming: the
        # existing child must retain its admitted model, instructions and guard.
        phase['name']='resume'; local=dict(env,TERM='xterm-256color');cmd=[common[0]]+common[2:]+['--prompt','ROOT_HISTORY_SECRET exercise resume'];commands.append({'argv':cmd,'environment':local});master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',40,140,0,0));proc=subprocess.Popen(cmd,cwd=workspace,env=local,stdin=slave,stdout=slave,stderr=slave,start_new_session=True);os.close(slave);terminal=bytearray()
        def drain():
            while select.select([master],[],[],0)[0]:
                try:data=os.read(master,65536)
                except OSError:break
                if not data:break
                terminal.extend(data)
                if b'\x1b[6n' in data:os.write(master,b'\x1b[1;1R')
        try:
            deadline=time.monotonic()+35
            while time.monotonic()<deadline and not resumed.is_set() and not errors and proc.poll() is None:drain();time.sleep(.02)
            require(not errors,'\n'.join(errors));require(resumed.is_set(),'real deferred profile resume did not complete')
        finally:
            if proc.poll() is None:os.write(master,b'\x03')
            try:proc.wait(timeout=10)
            except subprocess.TimeoutExpired:proc.kill();proc.wait()
            drain();os.close(master);(artifact/'resume.terminal').write_bytes(terminal)
        checks.append('real deferred resume retains admitted profile/model after project definition edit')
        require(git('rev-parse','HEAD')==base,'parent commit moved');require((workspace/'tracked.txt').read_text()=='parent-original\n','parent file changed')
        for name in ['forbidden.txt','plan-forbidden.txt','nested-forbidden.txt','skill-forbidden.txt','resume-forbidden.txt','isolated.txt','committed.txt']:require(not(workspace/name).exists(),'effect escaped restriction/isolation: '+name)
        outcome.update(success=True,checks=checks+['profile model and instructions arrive before first child HTTP','host denied direct and descendant writes','forked skill fresh child/provenance/arguments','Read restrictions suppress discovery and indirect context','parent workspace/files/HEAD unchanged','resume rejects reprofile and closed child'])
    finally:
        server.shutdown();outcome['errors']=errors; (artifact/'outcome.json').write_text(json.dumps(outcome,indent=2));(artifact/'requests.json').write_text(json.dumps(requests,indent=2));(artifact/'receipts.json').write_text(json.dumps(receipts,indent=2));(artifact/'commands.json').write_text(json.dumps(commands,indent=2));print(artifact)
if __name__=='__main__':main()
