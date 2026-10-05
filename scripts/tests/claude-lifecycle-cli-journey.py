#!/usr/bin/env python3
"""Real CLI + command process lifecycle gates, observation and crash recovery.
Only the external Messages provider is a loopback fixture. Evidence retains
commands, HTTP bodies, hook inputs and CLI output for every scenario.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import signal
import sqlite3
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4
from importlib.util import spec_from_file_location, module_from_spec

spec = spec_from_file_location('hook_journey', Path(__file__).with_name('claude-hooks-cli-journey.py'))
helper = module_from_spec(spec)
spec.loader.exec_module(helper)
require, sse = helper.require, helper.sse


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, default=Path('output/claude-lifecycle-cli') / uuid4().hex)
    args = parser.parse_args()
    artifact = args.output.resolve()
    workspace = artifact / 'workspace'
    (workspace / 'home').mkdir(parents=True)
    script = workspace / 'hook.py'
    script.write_text('''import json, os, sys, time
p = json.load(sys.stdin)
p['pgrp'] = os.getpgrp()
mode = sys.argv[1]
p['mode'] = mode
with open('lifecycle.jsonl', 'a') as f: f.write(json.dumps(p) + '\\n')
event = p['hook_event_name']
if mode == 'uncertain' and event == 'UserPromptSubmit':
    time.sleep(30)
elif mode == 'deny' and event == 'UserPromptSubmit':
    print('user-gate-denied', file=sys.stderr); sys.exit(2)
elif mode == 'normal' and event == 'Stop':
    if not p['stop_hook_active']:
        print('finish-verification', file=sys.stderr); sys.exit(2)
    print('stop-observer-failed', file=sys.stderr); sys.exit(1)
elif mode == 'failure' and event == 'StopFailure':
    print('failure-observer-failed', file=sys.stderr); sys.exit(1)
elif event in ['SessionStart', 'UserPromptSubmit', 'SubagentStart']:
    print(json.dumps({'hookSpecificOutput': {'hookEventName': event, 'additionalContext': 'trusted-context-' + event}}))
else: print('{}')
''')
    events = ['SessionStart', 'UserPromptSubmit', 'Stop', 'StopFailure', 'SessionEnd', 'SubagentStart', 'SubagentStop']
    settings = {}
    for mode in ['normal', 'deny', 'failure', 'uncertain', 'children']:
        path = artifact / (mode + '.json')
        path.write_text(json.dumps({'hooks': {event: [{'hooks': [{'type': 'command', 'command': '/usr/bin/python3 ' + shlex.quote(str(script)) + ' ' + mode, 'timeout': 40}]}] for event in events}}))
        settings[mode] = path
    unmatched = artifact / 'unmatched.json'
    unmatched.write_text(json.dumps({'hooks': {
        event: [{'matcher': '^never-matches-this-journey$', 'hooks': [{
            'type': 'command', 'command': '/usr/bin/python3 ' + shlex.quote(str(script)) + ' unmatched'
        }]}] for event in ['SessionStart', 'Stop', 'SessionEnd', 'SubagentStart', 'SubagentStop']
    }}))
    requests, errors, commands = [], [], []
    phase = {'name': 'normal', 'calls': 0}
    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['content-length'])))
            phase['calls'] += 1
            requests.append({'phase': phase['name'], 'body': body})
            (artifact / 'provider.json').write_text(json.dumps(requests, indent=2))
            if phase['name'] == 'failure':
                response = json.dumps({'type': 'error', 'error': {'type': 'invalid_request_error', 'message': 'fixture-provider-failure'}}).encode()
                self.send_response(400)
            else:
                try:
                    content = json.dumps(body['messages'])
                    if phase['name'] == 'defaults':
                        require('trusted-context-' not in content, 'unmatched lifecycle changed model context')
                    else:
                        require('trusted-context-UserPromptSubmit' in content, 'nested Interaction lost prompt lifecycle forwarding')
                    if phase['name'] == 'normal':
                        require('trusted-context-SessionStart' in content, 'session context not sent')
                        if phase['calls'] == 2: require('finish-verification' in content and 'answer-before-stop' in content, 'Stop continuation lost response/reason')
                        require(phase['calls'] <= 2, 'Stop repeated provider loop')
                        block = {'type': 'text', 'text': 'answer-before-stop' if phase['calls'] == 1 else 'answer-after-stop'}
                    elif phase['name'] == 'defaults':
                        block = {'type': 'text', 'text': 'default-host-answer'}
                    elif phase['name'] == 'children':
                        if 'Act as a specialist subagent' in content:
                            require('trusted-context-SubagentStart' in content, 'child start context not delivered')
                            if any(b.get('type') == 'tool_result' for m in body['messages'] for b in m.get('content', [])):
                                block = {'type': 'text', 'text': 'child-lifecycle-answer'}
                            else:
                                block = {'type': 'tool_use', 'id': 'child-submit', 'name': 'SubmitResult', 'input': {'output': 'child-lifecycle-answer'}}
                        elif phase['calls'] == 1:
                            block = {'type': 'tool_use', 'id': 'child-once', 'name': 'Agent', 'input': {'description': 'lifecycle child', 'prompt': 'CHILD-LIFECYCLE-FIXTURE', 'subagent_type': 'general-purpose'}}
                        else:
                            require('child-lifecycle-answer' in content, 'child result missing')
                            block = {'type': 'text', 'text': 'parent-lifecycle-answer'}
                    else: raise AssertionError('gated prompt reached provider')
                except Exception as error:
                    errors.append(str(error)); block = {'type': 'text', 'text': 'fixture-error'}
                response = sse(block, body['model'])
                self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(response)))
            self.end_headers(); self.wfile.write(response)
    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = [str(args.binary.resolve()), 'run', '--claude', '--model', 'claude-sonnet-5-5', '--thinking', 'medium', '--claude-api-key', 'synthetic', '--claude-messages-url', f'http://127.0.0.1:{server.server_port}/v1/messages', '--cwd', str(workspace), '--rollouts', 'false', '--browser=none', '--mcp-defaults', 'false', '--mcp-codex-config', 'false', '--web-search', 'false', '--image-generation', 'false', '--subagents', 'true', '--memory', 'false']
    env = {'HOME': str(workspace/'home'), 'CODEX_HOME': str(workspace/'codex-home'), 'PATH':'/usr/bin:/bin', 'NANOCODEX_COMPUTER':'off'}
    def command(mode, durable=False):
        cmd = base + ['--claude-hooks', str(settings[mode])]
        if durable: cmd += ['--local-durability', str(artifact/(mode+'.sqlite')), '--local-durability-state-id', mode, '--request-id', mode+'-request']
        return cmd + ['LIFECYCLE-'+mode]
    def records():
        path = workspace/'lifecycle.jsonl'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []
    def run(name, cmd, success):
        commands.append({'name': name, 'argv':cmd})
        (artifact/'commands.json').write_text(json.dumps({'commands':commands,'environment':env},indent=2))
        result = subprocess.run(cmd, cwd=workspace, env=env, capture_output=True, timeout=60)
        (artifact/(name+'.stdout')).write_bytes(result.stdout)
        (artifact/(name+'.stderr')).write_bytes(result.stderr)
        require((result.returncode==0)==success, f'{name} exit {result.returncode}: {result.stderr.decode()}')
        require(not errors, '; '.join(errors))
        return result
    outcome = {'success':False}; process = None
    try:
        # Compare the actual default Interaction/checkpoint/profile host with
        # explicit lifecycle settings whose event matchers do not match.
        default_journals = []
        default_wire = []
        phase.update(name='defaults', calls=0)
        for name, hook_args in [('default-host', []), ('unmatched-host', ['--claude-hooks', str(unmatched)])]:
            database = artifact / (name + '.sqlite')
            cmd = base + hook_args + ['--local-durability', str(database), '--local-durability-state-id', name,
                                      '--request-id', 'default-host-request', 'DEFAULT-HOST-PROMPT']
            before_requests = len(requests)
            before_hooks = records()
            result = run(name, cmd, True)
            require(b'default-host-answer' in result.stdout, 'default host answer missing')
            require(records() == before_hooks, 'unmatched lifecycle executed a command')
            require(len(requests) == before_requests + 1, 'default host changed provider count')
            default_wire.append(requests[-1]['body'])
            with sqlite3.connect(database) as db:
                rows = db.execute('SELECT revision, payload FROM nanocodex_durable_states').fetchall()
            require(len(rows) == 1, 'unexpected default journal count')
            revision, payload = rows[0]
            journal = json.loads(payload)['nanocodex_durable_state']
            require('claude_lifecycle' not in json.dumps(journal), 'unmatched lifecycle admitted a durable effect')
            require(len(journal['operations']) == 1, 'unmatched SessionEnd admitted a durable operation')
            default_journals.append({'name': name, 'revision': revision,
                                     'operation_count': len(journal['operations']), 'journal': journal})
        require(default_wire[0] == default_wire[1], 'unmatched lifecycle changed provider wire')
        require(default_journals[0]['revision'] == default_journals[1]['revision'], 'unmatched lifecycle added journal writes')
        (artifact / 'default-journals.json').write_text(json.dumps(default_journals, indent=2))
        phase.update(name='normal', calls=0)
        normal = command('normal', True)
        result = run('normal', normal, True)
        require(b'answer-after-stop' in result.stdout, 'Stop observer discarded final answer')
        require(b'stop-observer-failed' in result.stdout, 'observational hook diagnostic not exposed')
        log = records(); before = len(requests)
        require([r['hook_event_name'] for r in log]==['SessionStart','UserPromptSubmit','Stop','Stop','SessionEnd'], 'wrong actual lifecycle order: '+str(log))
        require([r['stop_hook_active'] for r in log if r['hook_event_name']=='Stop']==[False,True], 'Stop active flag missing')
        for r in log:
            require(all(r.get(k) for k in ['session_id','turn_id','event_id','model','cwd']), 'hook identity incomplete')
        run('terminal-replay', normal, True)
        require(records()==log and len(requests)==before, 'terminal replay repeated hook/provider effect')
        phase.update(name='normal', calls=0)
        resumed = normal.copy()
        resumed[resumed.index('--request-id')+1] = 'normal-resumed-request'
        resumed[-1] = 'LIFECYCLE-resumed'
        run('resumed-prompt', resumed, True)
        starts = [r for r in records() if r['mode']=='normal' and r['hook_event_name']=='SessionStart']
        require([r['source'] for r in starts]==['startup','resume'], 'new resumed prompt missing SessionStart resume')
        phase.update(name='deny', calls=0)
        result=run('deny', command('deny'), False)
        require(phase['calls']==0 and b'user-gate-denied' in result.stderr, 'prompt gate did not block before model')
        phase.update(name='failure', calls=0)
        result=run('provider-failure', command('failure'), False)
        require(b'fixture-provider-failure' in result.stderr, 'StopFailure replaced original provider failure')
        require(any(r['hook_event_name']=='StopFailure' and 'fixture-provider-failure' in r['error_details'] for r in records()), 'StopFailure not delivered')
        phase.update(name='children', calls=0)
        run('children', command('children'), True)
        children=[r for r in records() if r['mode']=='children']
        starts=[r for r in children if r['hook_event_name']=='SubagentStart']
        stops=[r for r in children if r['hook_event_name']=='SubagentStop']
        require(len(starts)==len(stops)==1 and starts[0]['agent_id']==stops[0]['agent_id'], 'child lifecycle identity missing')
        phase.update(name='uncertain', calls=0)
        cmd=command('uncertain', True)
        commands.append({'name':'crash-during-hook','argv':cmd})
        (artifact/'commands.json').write_text(json.dumps({'commands':commands,'environment':env},indent=2))
        with (artifact/'crash.stdout').open('wb') as out, (artifact/'crash.stderr').open('wb') as err:
            process=subprocess.Popen(cmd,cwd=workspace,env=env,stdout=out,stderr=err)
            deadline=time.monotonic()+30
            while time.monotonic()<deadline:
                pending=[r for r in records() if r['mode']=='uncertain' and r['hook_event_name']=='UserPromptSubmit']
                if pending: break
                require(process.poll() is None, 'CLI exited before hook: '+(artifact/'crash.stderr').read_text())
                time.sleep(.05)
            require(pending, 'hook did not start')
            process.kill(); process.wait(timeout=10)
            try: os.killpg(pending[0]['pgrp'], signal.SIGKILL)
            except ProcessLookupError: pass
        result=run('uncertain-resume',cmd,False)
        require(b'outcome unknown' in result.stderr and b'hook was not repeated' in result.stderr, 'uncertain effect not reported')
        require(phase['calls']==0, 'uncertain prompt effect allowed model')
        uncertain=[r for r in records() if r['mode']=='uncertain']
        require(sum(r['hook_event_name']=='UserPromptSubmit' for r in uncertain)==1, 'uncertain external command was repeated')
        require(sum(r['hook_event_name']=='SessionStart' for r in uncertain)==1, 'committed SessionStart repeated')
        outcome={'success':True,'provider_requests':len(requests),'replay_hooks':0,'replay_provider_requests':0,'uncertain_hook_executions':1,'child_start_stop':1,'stop_continuations':1,'prompt_gate_provider_requests':0,'default_host_revisions':[j['revision'] for j in default_journals],'unmatched_lifecycle_effects':0}
    except Exception as error:
        outcome['error']=str(error); raise
    finally:
        if process and process.poll() is None: process.kill(); process.wait(timeout=10)
        server.shutdown()
        (artifact/'outcome.json').write_text(json.dumps(outcome,indent=2))
        print(json.dumps({'artifact':str(artifact),**outcome}))

if __name__=='__main__': main()
