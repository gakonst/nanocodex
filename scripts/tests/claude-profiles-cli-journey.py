#!/usr/bin/env python3
"""Actual CLI profile/skill/isolation lifecycle; only external inference is synthetic."""
import argparse, json, subprocess, threading, traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from claude_code_fixture import wrap_tool, normalize_request
from pathlib import Path
from uuid import uuid4


def require(value, message):
    if not value: raise AssertionError(message)

def text_of(result):
    content = result.get('content', '')
    return content if isinstance(content, str) else ''.join(v.get('text', '') for v in content)

def sse(block, model):
    block = wrap_tool(block)
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
    write('.claude/agents/reviewer.md','---\nname: reviewer\ndescription: Restricted reviewer\nmodel: haiku\ntools: Read, spawn_agent, wait_agent\n---\nPROFILE_INSTRUCTIONS_SENTINEL\n')
    write('.claude/agents/no-read.md','---\nname: no-read\ndescription: No file context\ntools: Grep\n---\nNO_READ_PROFILE_SENTINEL\n')
    write('.claude/agents/planner.md','---\nname: planner\ndescription: Plan-only profile\npermissionMode: plan\n---\nPLAN_PROFILE_SENTINEL\n')
    write('.claude/agents/isolated.md','---\nname: isolated\ndescription: Isolated work\nisolation: worktree\ntools: Read, Write, exec_command\n---\nISOLATED_PROFILE_SENTINEL\n')
    write('.claude/agents/legacy.md','---\nname: legacy\ndescription: Removed deny alias\ndisallowedTools: Agent\n---\nNever run with weakened restrictions.\n')
    write('.claude/skills/fork-review/SKILL.md','---\nname: fork-review\ndescription: Review in a real clean child\ncontext: fork\nagent: reviewer\nmodel: haiku\n---\nSKILL_CHILD_MARKER $ARGUMENTS\n')
    write('.claude/skills/disabled/SKILL.md','---\nname: disabled\ndisable-model-invocation: true\ncontext: fork\n---\nNever run.\n')
    git('init','-q');git('config','user.email','fixture@example.invalid');git('config','user.name','Synthetic Fixture');git('add','.');git('commit','-qm','fixture base');base=git('rev-parse','HEAD')
    requests=[];receipts=[];errors=[];commands=[];checks=[];streams={};ids={};paths={};lock=threading.RLock();phase={'name':'main'}
    def save_agent(key):
        def check(value): ids[key]=value['agent_id']
        return check
    def completed(value): require('completed' in json.dumps(value),'child result missing completion: '+str(value))
    def step(name,inp,error=False,check=None):return (name,inp,error,check)
    def spawn(marker,**extra): return {'role':'Fixture child','task':marker,'output_contract':{'kind':'string'},**extra}
    def wait(key):return step('wait_agent',lambda:{'agent_ids':[ids[key]],'timeout_ms':20000},check=completed)
    def capture_path(key):
        def check(value):
            paths[key]=Path(value['output'].strip()); require(paths[key]!=workspace,'child did not isolate'); require(paths[key].is_dir(),'missing child worktree')
        return check
    def clean_closed(value): require(not paths['clean'].exists(),'unchanged child worktree survived close_agent');checks.append('unchanged worktree safely removed')
    def dirty_closed(value): require((paths['dirty']/'isolated.txt').read_text()=='child-only','dirty worktree lost');checks.append('dirty worktree retained')
    for skill,profile,marker in [('clean','isolated','CLEAN_CHILD_MARKER'),('dirty','isolated','DIRTY_CHILD_MARKER'),('no-read','no-read','NO_READ_CHILD_MARKER'),('planner','planner','PLAN_CHILD_MARKER'),('legacy','legacy','NEVER_LEGACY_CHILD')]:
        write(f'.claude/skills/{skill}/SKILL.md',f'---\nname: {skill}\ndescription: Fixture child\ncontext: fork\nagent: {profile}\n---\n{marker}\n')
    git('add','.');git('commit','-qm','skill fixtures');base=git('rev-parse','HEAD')
    root=[step('Skill',{'skill':'fork-review','args':'SKILL_ARGUMENT_LITERAL'},check=save_agent('skill')),wait('skill'),
          step('Skill',{'skill':'disabled'},True),step('Skill',{'skill':'legacy'},True),
          step('Skill',{'skill':'planner'},check=save_agent('planner')),wait('planner'),
          step('Skill',{'skill':'no-read'},check=save_agent('no-read')),wait('no-read'),
          step('Skill',{'skill':'clean'},check=save_agent('clean')),wait('clean'),
          step('close_agent',lambda:{'agent_id':ids['clean']},check=clean_closed),
          step('Skill',{'skill':'dirty'},check=save_agent('dirty')),wait('dirty'),
          step('close_agent',lambda:{'agent_id':ids['dirty']},check=dirty_closed)]
    child={
        'PLAN_CHILD_MARKER':[step('Write',{'file_path':'plan-forbidden.txt','content':'must not write'},True),step('submit_result',{'output':'plan-complete'})],
        'NESTED_CHILD_MARKER':[step('Write',{'file_path':'nested-forbidden.txt','content':'must not write'},True),step('submit_result',{'output':'nested-complete'})],
        'SKILL_CHILD_MARKER':[step('Write',{'file_path':'skill-forbidden.txt','content':'must not write'},True),
            step('spawn_agent',spawn('ILLEGAL_MODEL_OVERRIDE',model='claude-sonnet-5-5'),True),
            step('spawn_agent',spawn('ILLEGAL_CROSS_FAMILY',harness='codex'),True),
            step('spawn_agent',spawn('NESTED_CHILD_MARKER'),check=save_agent('nested')),wait('nested'),step('submit_result',{'output':'skill-complete'})],
        'NO_READ_CHILD_MARKER':[step('Grep',{'pattern':'parent-original','path':'tracked.txt'},check=lambda v:require('ROOT_CONTEXT_SECRET' not in json.dumps(v),'Grep leaked blocked project context')),step('submit_result',{'output':'no-read-complete'})],
        'CLEAN_CHILD_MARKER':[step('exec_command',{'cmd':'pwd'},check=capture_path('clean')),step('submit_result',{'output':'clean-complete'})],
        'DIRTY_CHILD_MARKER':[step('exec_command',{'cmd':'pwd'},check=capture_path('dirty')),step('Write',{'file_path':'isolated.txt','content':'child-only'}),step('submit_result',{'output':'dirty-complete'})],
    }
    class Provider(BaseHTTPRequestHandler):
        def log_message(self,*_):pass
        def do_POST(self):
            body=normalize_request(json.loads(self.rfile.read(int(self.headers['content-length']))), artifact)
            with lock:
                try:
                    first=json.dumps(body['messages'][0]); key=next((k for k in child if k in first),'root'); streamkey=key;stage=streams.setdefault((phase['name'],streamkey),{'index':0,'pending':None});requests.append({'phase':phase['name'],'stream':key,'body':body})
                    if key!='root':
                        require('ROOT_HISTORY_SECRET' not in json.dumps(body['messages']),'clean child inherited parent transcript')
                        if key in ('PROFILE_CHILD_MARKER','NESTED_CHILD_MARKER','SKILL_CHILD_MARKER','RESUME_PROFILE_MARKER'):
                            require(body['model']=='claude-haiku-5-5','profile model was not enforced');require('PROFILE_INSTRUCTIONS_SENTINEL' in json.dumps(body.get('system')),'admission lost profile instructions')
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
                    if phase['name']=='read-denied':plan=[step('Skill',{'skill':'fork-review'},True)]
                    if phase['name']=='spawn-denied':plan=[step('Skill',{'skill':'fork-review'},True),step('spawn_agent',spawn('NEVER_CHILD'),True)]
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
        for name,flags in [('main',[]),('read-denied',['--claude-permissions',str(artifact/'read-deny.json')]),('spawn-denied',['--claude-permissions',str(artifact/'spawn-deny.json')])]:
            (artifact/'spawn-deny.json').write_text(json.dumps({'permissions':{'defaultMode':'full-access','deny':['spawn_agent']}}))
            (artifact/'read-deny.json').write_text(json.dumps({'permissions':{'defaultMode':'full-access','deny':['Read']}}));phase['name']=name;cmd=common+flags+['ROOT_HISTORY_SECRET Exercise authorized profile child journeys.'];commands.append({'argv':cmd,'environment':env});result=subprocess.run(cmd,cwd=workspace,env=env,capture_output=True,text=True,timeout=110);(artifact/(name+'.stdout')).write_text(result.stdout);(artifact/(name+'.stderr')).write_text(result.stderr);require(result.returncode==0,'CLI failed: '+result.stderr);require(not errors,'\n'.join(errors));require('profiles-journey-complete' in result.stdout,'missing final result')
        require(git('rev-parse','HEAD')==base,'parent commit moved');require((workspace/'tracked.txt').read_text()=='parent-original\n','parent file changed')
        for name in ['forbidden.txt','plan-forbidden.txt','nested-forbidden.txt','skill-forbidden.txt','resume-forbidden.txt','isolated.txt','committed.txt']:require(not(workspace/name).exists(),'effect escaped restriction/isolation: '+name)
        outcome.update(success=True,checks=checks+['profile model and instructions arrive before first child HTTP','host denied direct and descendant writes','forked skill fresh child/provenance/arguments','Read restrictions suppress discovery and indirect context','parent workspace/files/HEAD unchanged'])
    finally:
        server.shutdown();outcome['errors']=errors; (artifact/'outcome.json').write_text(json.dumps(outcome,indent=2));(artifact/'requests.json').write_text(json.dumps(requests,indent=2));(artifact/'receipts.json').write_text(json.dumps(receipts,indent=2));(artifact/'commands.json').write_text(json.dumps(commands,indent=2));print(artifact)
if __name__=='__main__':main()
