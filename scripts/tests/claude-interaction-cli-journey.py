#!/usr/bin/env python3
"""Native TUI/terminal interaction journey; only Messages HTTP is synthetic."""
from claude_code_fixture import normalize_request, wrap_tool
import argparse, fcntl, importlib.util, json, os, pty, re, select, signal, struct, subprocess, termios, threading, time
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

spec = importlib.util.spec_from_file_location('journey', Path(__file__).with_name('claude-scheduler-monitor-cli-journey.py'))
helper = importlib.util.module_from_spec(spec); spec.loader.exec_module(helper)
require, sse, text_of = helper.require, helper.sse, helper.text_of

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, default=Path('output/claude-interaction-cli') / uuid4().hex)
    args = parser.parse_args(); artifact = args.output.resolve(); artifact.mkdir(parents=True)
    workspace = artifact / 'workspace'; workspace.mkdir(); (workspace/'read.txt').write_text('read marker')
    home = artifact/'home'; home.mkdir(); codex_home=home/'codex'; codex_home.mkdir()
    hooks = artifact/'hooks.json'
    hooks.write_text(json.dumps({'hooks':{
        'PreToolUse':[{'matcher':'Read','hooks':[{'type':'command','command':"printf '%s' '{\"decision\":\"block\",\"reason\":\"configured-read-denial\"}'"}]}, {'matcher':'Glob','hooks':[{'type':'command','command':'printf pre >> hooks.log'}]}],
        'PostToolUse':[{'matcher':'EnterPlanMode|Glob','hooks':[{'type':'command','command':'printf post >> hooks.log'}]}]
    }}))
    env={'HOME':str(home),'CODEX_HOME':str(codex_home),'PATH':'/usr/bin:/bin:/usr/sbin:/sbin','TERM':'xterm-256color','NANOCODEX_COMPUTER':'off'}
    question={'questions':[{'question':'Choose a synthetic color','header':'Color','options':[{'label':'Blue','description':'first color'},{'label':'Green','description':'second color'}],'multiSelect':False}]}
    steps=[
        ('Bash',{'command':'printf prior > prior.txt'},False,None),
        ('EnterPlanMode',{},False,'plan'),
        ('Write',{'file_path':'blocked.txt','content':'unsafe'},True,'plan mode'),
        ('Bash',{'command':'printf unsafe > blocked-shell.txt'},True,'plan mode'),
        ('Read',{'file_path':'read.txt'},True,'configured-read-denial'),
        ('Glob',{'pattern':'*.txt'},False,'read.txt'),
        ('AskUserQuestion',question,False,'Blue'),
        ('ExitPlanMode',{},False,'false'),
        ('Write',{'file_path':'denied.txt','content':'unsafe'},True,'plan mode'),
        ('ExitPlanMode',{},False,'true'),
        ('Write',{'file_path':'approved.txt','content':'approved-effect'},False,None),
        ('EnterPlanMode',{},False,'plan'),
        ('ExitPlanMode',{},True,None),
        ('Write',{'file_path':'persistence-failed.txt','content':'unsafe'},True,'plan mode'),
        ('ExitPlanMode',{},True,'cancelled'),
        ('Write',{'file_path':'cancelled.txt','content':'unsafe'},True,'plan mode'),
    ]
    requests=[]; errors=[]; gate=threading.Event(); at_gate=threading.Event(); phase={'name':'tui','start':0,'steps':steps}
    class Provider(BaseHTTPRequestHandler):
        def log_message(self,*_): pass
        def do_POST(self):
            request=json.loads(self.rfile.read(int(self.headers['content-length']))); request = normalize_request(request, artifact); stage=len(requests)-phase['start']; requests.append(request)
            (artifact/'provider.json').write_text(json.dumps(requests,indent=2))
            try:
                current=phase['steps']
                if stage:
                    previous=current[stage-1]; call=f"{phase['name']}_{stage-1}"
                    receipts=[b for m in request['messages'] for b in m.get('content',[]) if isinstance(b,dict) and b.get('type')=='tool_result' and b.get('tool_use_id')==call]
                    require(len(receipts)==1,f'missing receipt {call}')
                    r=receipts[0]; require(bool(r.get('is_error'))==previous[2],f'wrong error {call}: {r}')
                    if previous[3]: require(previous[3] in text_of(r),f'missing {previous[3]}: {r}')
                if phase['name']=='tui' and stage==7:
                    at_gate.set(); require(gate.wait(20),'draft gate timed out')
                block={'type':'text','text':'interaction-journey-complete'} if stage==len(current) else {'type':'tool_use','id':f"{phase['name']}_{stage}",'name':current[stage][0],'input':current[stage][1]}
            except Exception as e:
                errors.append(str(e)); block={'type':'text','text':'fixture-failed'}
            response=sse(wrap_tool(block),request['model']); self.send_response(200); self.send_header('Content-Type','text/event-stream'); self.send_header('Content-Length',str(len(response))); self.end_headers(); self.wfile.write(response)
    server=ThreadingHTTPServer(('127.0.0.1',0),Provider); threading.Thread(target=server.serve_forever,daemon=True).start()
    common=[str(args.binary.resolve()),'--claude','--model','claude-sonnet-5-5','--claude-api-key','synthetic-key','--claude-messages-url',f'http://127.0.0.1:{server.server_port}/v1/messages','--cwd',str(workspace),'--browser=none','--mcp-defaults','false','--mcp-codex-config','false','--web-search','false','--image-generation','false','--subagents','false','--memory','false','--claude-hooks',str(hooks)]
    commands=[]; processes=[]; transcripts={}; screens={}
    def start(command,label):
        master,slave=pty.openpty(); fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',45,160,0,0))
        process=subprocess.Popen(command,stdin=slave,stdout=slave,stderr=slave,cwd=workspace,env=env,start_new_session=True); os.close(slave)
        processes.append(process); commands.append(command); transcripts[label]=bytearray(); screens[label]=helper.TerminalScreen(rows=45, columns=160)
        def drain():
            while select.select([master],[],[],0)[0]:
                try: chunk=os.read(master,65536)
                except OSError: break
                if not chunk: break
                transcripts[label].extend(chunk); screens[label].feed(chunk)
        return process,master,drain
    def wait(check,drain,message,timeout=20):
        end=time.monotonic()+timeout
        while time.monotonic()<end:
            drain()
            require(not errors,'; '.join(errors))
            if check(): return
            time.sleep(.025)
        raise AssertionError(message)
    def visible(label, text):
        # Observe rendered cells: unchanged letters are absent from VT deltas.
        return re.sub(r'\s+', '', text.decode()) in re.sub(r'\s+', '', screens[label].text())
    def pending(count,drain):
        end=time.monotonic()+.6
        while time.monotonic()<end: drain(); time.sleep(.02)
        require(len(requests)==count,f'provider advanced without user answer ({len(requests)} vs {count})')
    outcome={'success':False}
    try:
        p,fd,drain=start(common+['--prompt','Exercise interactive plan and question journey.'],'tui')
        wait(lambda:len(requests)==7,drain,'TUI question absent'); pending(7,drain)
        wait(lambda:visible('tui', b'Choose one number'),drain,'TUI did not render question')
        os.write(fd,b'1\r'); wait(at_gate.is_set,drain,'question answer did not arrive')
        os.write(fd,b'approve'); time.sleep(.2); drain(); gate.set()
        wait(lambda:visible('tui', b'Plan approval required'),drain,'plan approval absent'); time.sleep(.2); drain()
        os.write(fd,b'\r'); pending(8,drain) # stale draft must be cleared
        os.write(fd,b'deny\r'); wait(lambda:len(requests)==10,drain,'denial did not preserve plan'); pending(10,drain)
        os.write(fd,b'approve\r'); wait(lambda:len(requests)==13,drain,'approval did not allow write/reenter'); pending(13,drain)
        plan_dir=codex_home/'claude/plan-mode'; saved_dir=codex_home/'claude/plan-mode-saved'
        plan_dir.rename(saved_dir); plan_dir.write_text('force persistence failure')
        os.write(fd,b'approve\r'); wait(lambda:len(requests)==15,drain,'failed persistence did not preserve plan'); pending(15,drain)
        plan_dir.unlink(); saved_dir.rename(plan_dir)
        os.write(fd,b'/cancel\r'); wait(lambda:len(requests)==17,drain,'cancel did not return error'); pending(17,drain)
        wait(lambda:visible('tui', b'interaction-journey-complete'),drain,'TUI final answer absent')
        os.write(fd,b'\x03'); p.wait(timeout=10); drain(); os.close(fd)
        require((workspace/'approved.txt').read_text()=='approved-effect','approved write absent')
        require((workspace/'prior.txt').read_text()=='prior','plan entry after Bash failed')
        for name in ['blocked.txt','blocked-shell.txt','denied.txt','persistence-failed.txt','cancelled.txt']: require(not(workspace/name).exists(),f'blocked mutation exists: {name}')
        require((workspace/'hooks.log').read_text()=='postprepostpost','hooks silently skipped in plan')
        # Reopen the same default native journal headlessly: gate is installed even
        # when interactive tool schemas are unavailable.
        sessions=helper.h.durable_sessions(codex_home);require(len(sessions)==1,'session manifest missing')
        session=sessions[0]
        phase.update(name='headless',start=len(requests),steps=[('Write',{'file_path':'restart.txt','content':'unsafe'},True,'plan mode')])
        command=[common[0],'run']+common[1:]+['--rollouts','false','--local-durability',str(helper.h.durable_store(codex_home)),'--local-durability-state-id',session,'Verify saved planning gate.']; commands.append(command)
        result=subprocess.run(command,cwd=workspace,env=env,capture_output=True,timeout=30)
        (artifact/'headless.jsonl').write_bytes(result.stdout); (artifact/'headless.stderr').write_bytes(result.stderr)
        require(result.returncode==0,f'headless failed {result.stderr!r}'); require(not errors,'; '.join(errors)); require(not(workspace/'restart.txt').exists(),'restart escaped plan')
        # Actual pending terminal request cancelled by SIGINT, then same durable
        # session reopened. No fabricated answer, no subsequent mutation.
        phase.update(name='interrupt',start=len(requests),steps=[('AskUserQuestion',question,False,'unused')])
        command=[common[0],'run']+common[1:]+['--rollouts','false','--local-durability',str(helper.h.durable_store(codex_home)),'--local-durability-state-id',session,'Wait for a terminal answer.']
        p,fd,drain=start(command,'interrupt'); wait(lambda:visible('interrupt', b'Choose one number'),drain,'terminal question absent'); pending(phase['start']+1,drain)
        p.send_signal(signal.SIGINT); wait(lambda:p.poll() is not None,drain,'pending terminal request did not cancel',timeout=10); drain(); os.close(fd)
        require(p.returncode!=0,'interrupt reported success'); require(len(requests)==phase['start']+1,'cancelled question fabricated answer')
        phase.update(name='after-interrupt',start=len(requests),steps=[('Write',{'file_path':'after-interrupt.txt','content':'unsafe'},True,'plan mode')])
        command[-1]='Verify planning survives pending cancellation.'; commands.append(command)
        result=subprocess.run(command,cwd=workspace,env=env,capture_output=True,timeout=30)
        (artifact/'after-interrupt.jsonl').write_bytes(result.stdout); (artifact/'after-interrupt.stderr').write_bytes(result.stderr)
        require(result.returncode==0,f'after interrupt failed {result.stderr!r}'); require(not errors,'; '.join(errors)); require(not(workspace/'after-interrupt.txt').exists(),'pending cancellation lost plan')
        outcome={'success':True,'tui_question_waited':True,'exact_answer':'Blue','draft_approval_discarded':True,'failed_persistence_preserved_plan':True,'denial_and_cancel_preserved_plan':True,'approved_write':True,'blocked_writes_absent':True,'hooks_preserved_in_plan':True,'plan_after_prior_bash':True,'headless_restart_gate':True,'pending_sigint_cancelled':True,'provider_requests':len(requests)}
    finally:
        for p in processes:
            if p.poll() is None: p.kill(); p.wait()
        for label,data in transcripts.items():
            (artifact/f'{label}.pty').write_bytes(data)
            (artifact/f'{label}.screen.txt').write_text(screens[label].text())
        (artifact/'scenario.json').write_text(json.dumps({'commands':commands,'environment':env,'boundary':'actual CLI TUI and terminal via PTY, local filesystem and SQLite, synthetic Messages HTTP only'},indent=2))
        (artifact/'outcome.json').write_text(json.dumps(outcome,indent=2)); server.shutdown(); print(json.dumps({'artifact':str(artifact),**outcome}))
if __name__=='__main__': main()
