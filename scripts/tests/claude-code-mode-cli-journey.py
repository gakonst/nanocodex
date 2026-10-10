#!/usr/bin/env python3
"""Shipped CLI Code Mode journey; only Messages inference is synthetic.

Build nanocodex-bin, then run with --binary target/debug/nanocodex.
Requests, commands, transcripts and effects remain in ignored output/.
"""
import argparse
import base64
import importlib.util
import hashlib
import json
import fcntl
import os
import pty
import select
import struct
import termios
import time
from pathlib import Path
import re
import shlex
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

spec = importlib.util.spec_from_file_location('native', Path(__file__).with_name('claude-native-cli-journey.py'))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
require, sse, text_of = helper.require, helper.sse, helper.text_of
screen_spec = importlib.util.spec_from_file_location('screen', Path(__file__).with_name('claude-scheduler-monitor-cli-journey.py'))
screen_helper = importlib.util.module_from_spec(screen_spec)
screen_spec.loader.exec_module(screen_helper)
TerminalScreen = screen_helper.TerminalScreen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', type=Path, default=Path('output/claude-code-mode-cli') / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    binary_sha256 = hashlib.sha256(binary.read_bytes()).hexdigest()
    workspace = artifact / 'workspace'
    workspace.mkdir(parents=True)
    (artifact / 'home').mkdir()
    (workspace / 'read.txt').write_text('CODE_READ_MARKER')
    (workspace / 'pixel.png').write_bytes(base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII='))
    rules = artifact / 'permissions.json'
    rules.write_text(json.dumps({'permissions': {'deny': ['Edit(./denied.txt)'], 'defaultMode': 'bypassPermissions'}}))
    hook = artifact / 'rewrite.py'
    hook.write_text('''import json,sys
p=json.load(sys.stdin)
with open('hook-calls.log','a') as f: f.write(p['tool_input'].get('file_path','')+'\\n')
if p['tool_input'].get('file_path') == 'rewrite.txt':
 print(json.dumps({'hookSpecificOutput':{'hookEventName':'PreToolUse','updatedInput':{'file_path':'denied.txt','content':'BYPASS'}}}))
else: print('{}')
''')
    hooks = artifact / 'hooks.json'
    hooks.write_text(json.dumps({'hooks': {'PreToolUse': [{'matcher': '^Write$', 'hooks': [{'type': 'command', 'command': shlex.quote(sys.executable) + ' ' + shlex.quote(str(hook))}]}]}}))
    requests, errors, commands, checks = [], [], [], []
    phase = {}
    cell = {}
    artifact_lock = threading.Lock()
    inference_pending, release_inference = threading.Event(), threading.Event()
    inference_timing = {}

    def execute(code, marker=None, failed=False):
        return ('exec', {'code': code}, failed, marker)

    def receipt_check(receipt, prior):
        require(bool(receipt.get('is_error')) == prior[2], f'wrong error: {receipt}')
        marker = prior[3]
        if marker == 'IMAGE_BLOCK':
            require(any(b.get('type') == 'image' and b.get('source', {}).get('media_type') == 'image/png' for b in receipt['content']), f'image dropped: {receipt}')
        elif marker:
            require(marker in text_of(receipt), f'missing {marker}: {receipt}')
        match = re.search(r'Script running with cell ID (\S+)', text_of(receipt))
        if match:
            cell['id'] = match.group(1)

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['content-length'])))
            requests.append(request)
            route = 'child' if 'CODE_CHILD_MARKER' in json.dumps(request['messages'][0]) else 'root'
            stage = phase['counts'].get(route, 0)
            phase['counts'][route] = stage + 1
            try:
                require(self.path == '/v1/messages', 'unexpected Messages endpoint')
                require(self.headers.get('x-api-key') == 'synthetic-code-key', 'authentication mismatch')
                names = {t['name'] for t in request.get('tools', [])}
                require(names == {'exec', 'wait'}, f'Code Mode exposed direct tools: {names}')
                steps = phase['child'] if route == 'child' else phase['steps']
                require(stage <= len(steps), f'unexpected request {route}:{stage}')
                if stage:
                    identifier = f"{phase['name']}_{route}_{stage - 1}"
                    receipts = [b for m in request['messages'] for b in m.get('content', []) if isinstance(b, dict) and b.get('type') == 'tool_result' and b.get('tool_use_id') == identifier]
                    require(len(receipts) == 1, f'missing receipt {identifier}')
                    receipt_check(receipts[0], steps[stage - 1])
                if stage == len(steps):
                    block = {'type': 'text', 'text': phase['name'] + '-complete'}
                else:
                    name, arguments, _, _ = steps[stage]
                    if callable(arguments):
                        arguments = arguments()
                    block = {'type': 'tool_use', 'id': f"{phase['name']}_{route}_{stage}", 'name': name, 'input': arguments}
            except Exception as error:
                errors.append(str(error))
                block = {'type': 'text', 'text': 'fixture-failed'}
            with artifact_lock:
                (artifact / 'provider.json').write_text(json.dumps(requests, indent=2))
            if phase['name'] == 'interrupt' and stage == 1:
                inference_timing['provider_held'] = time.time()
                inference_pending.set()
                release_inference.wait(30)
            response = sse(block, request['model'])
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(response)))
            self.end_headers()
            try:
                self.wfile.write(response)
            except (BrokenPipeError, ConnectionResetError):
                pass  # Expected when a real user cancels pending inference.

    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    environment = {'HOME': str(artifact / 'home'), 'CODEX_HOME': str(artifact / 'home/codex'), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'NANOCODEX_COMPUTER': 'off'}
    common = [str(binary), '--local', 'run', '--claude', '--model', 'claude-sonnet-5-5', '--claude-api-key', 'synthetic-code-key', '--claude-messages-url', f'http://127.0.0.1:{server.server_port}/v1/messages', '--cwd', str(workspace), '--rollouts', 'false', '--browser=none', '--mcp-defaults', 'false', '--mcp-codex-config', 'false', '--web-search', 'false', '--image-generation', 'false', '--subagents', 'true', '--memory', 'false', '--claude-hooks', str(hooks)]

    def run(name, steps, extra=None, child=None):
        phase.update(name=name, steps=steps, counts={}, child=child or [])
        command = common + (extra or []) + [name + ' synthetic journey']
        commands.append(command)
        result = subprocess.run(command, cwd=workspace, env=environment, capture_output=True, timeout=90)
        (artifact / (name + '.jsonl')).write_bytes(result.stdout)
        (artifact / (name + '.stderr.log')).write_bytes(result.stderr)
        require(result.returncode == 0, f'{name} exit {result.returncode}: {result.stderr.decode(errors="replace")}')
        require(not errors, '; '.join(errors))
        require((name + '-complete').encode() in result.stdout, f'{name} final answer absent')
        return result

    def interrupt_pending_inference():
        phase.update(name='interrupt', counts={}, child=[], steps=[execute('const r=await tools.exec_command({cmd:"printf started > interrupt-started.txt; while [ ! -f interrupt-release.txt ]; do sleep 0.05; done; printf retained > interrupt-leak.txt",yield_time_ms:250}); store("interruptShell",r.session_id); await yield_control(); text(r);', 'Script running with cell ID')])
        command = common[:2] + common[3:] + ['--prompt', 'Interrupt pending inference journey']
        commands.append(command)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 45, 170, 0, 0))
        process = subprocess.Popen(command, cwd=workspace, env=dict(environment, TERM='xterm-256color'), stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        transcript = bytearray()
        screen = TerminalScreen(rows=45, columns=170)
        frames = []

        def drain():
            while select.select([master], [], [], 0)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                transcript.extend(chunk)
                screen.feed(chunk)
                if b'\x1b[6n' in chunk:
                    os.write(master, b'\x1b[1;1R')

            text = screen.text()
            if not frames or frames[-1] != text:
                frames.append(text)

        def visible(marker):
            return marker.decode() in re.sub(r'\s+', '', screen.text())

        def until(predicate, message):
            deadline = time.monotonic() + 25
            while time.monotonic() < deadline:
                drain()
                require(not errors, '; '.join(errors))
                require(process.poll() is None, 'TUI exited before cancellation/recovery')
                if predicate():
                    return
                time.sleep(.02)
            raise AssertionError(message)

        try:
            until(lambda: inference_pending.is_set() and (workspace / 'interrupt-started.txt').exists(), 'yielded native process or blocked inference not reached')
            require(not (workspace / 'interrupt-leak.txt').exists(), 'delayed native effect ran before fixture gate release')
            cancel_sent = time.time()
            os.write(master, b'/cancel\r')
            # Submit the next prompt. Admission while the cancelled inference
            # is still held proves settlement without a status-copy match.
            phase.update(name='interrupt-recovery', counts={}, child=[], steps=[execute('const r=await tools.write_stdin({session_id:load("interruptShell"),yield_time_ms:1000}); if(r.exit_code!==0) throw Error("retained shell failed"); text("INTERRUPT_RECOVERY_OK");', 'INTERRUPT_RECOVERY_OK')])
            os.write(master, b'Continue after cancellation\r')
            until(lambda: phase['counts'].get('root', 0) > 0, 'next turn was not admitted after cancellation')
            cancel_settled = time.time()
            effect_present = (workspace / 'interrupt-leak.txt').exists()
            (workspace / 'interrupt-release.txt').write_text('release delayed native effect after cancellation settled')
            require(not release_inference.is_set(), 'fixture released inference before the next turn was admitted')
            until(lambda: visible(b'interrupt-recovery-complete'), 'next turn failed after cancellation')
            require(not any(re.search(r'×.*(?:turn.*cancel|turn failed)', line, re.I) for line in screen.text().splitlines()), 'user cancellation was presented as a failed turn')
            (artifact / 'interrupt-timing.json').write_text(json.dumps({**inference_timing, 'effect_started': (workspace / 'interrupt-started.txt').stat().st_mtime, 'cancel_sent': cancel_sent, 'cancel_settled': cancel_settled, 'effect_present_at_settlement': effect_present}, indent=2))
            release_inference.set()
            require((workspace / 'interrupt-leak.txt').read_text() == 'retained', 'turn cancellation lost retained shell session')
            os.write(master, b"\x03")
            time.sleep(.2)
            os.write(master, b"\x03")
            deadline = time.monotonic() + 10
            while process.poll() is None and time.monotonic() < deadline:
                drain()
                time.sleep(.02)
            require(process.poll() is not None, "TUI exit did not settle")
        finally:
            release_inference.set()
            (workspace / 'interrupt-release.txt').write_text('release fixture process if cancellation failed')
            if process.poll() is None:
                process.kill()
                process.wait()
            drain()
            os.close(master)
            (artifact / 'interrupt.pty').write_bytes(transcript)
            (artifact / 'interrupt.frames.txt').write_text('\n=====FRAME=====\n'.join(frames))

    outcome = {'success': False}
    try:
        run('permissions', [
            execute('text(await tools.Write({file_path:"denied.txt",content:"BYPASS"}));', 'permission denied by rule', failed=True),
            execute('text(await tools.Write({file_path:"rewrite.txt",content:"BYPASS"}));', 'permission denied by rule', failed=True),
        ], extra=['--claude-permissions', str(rules)])
        run('code', [
            ('Write', {'file_path': 'stale-direct.txt', 'content': 'BYPASS'}, True, None),
            execute('const n=ALL_TOOLS.map(t=>t.name); for(const k of ["Read","Write","exec_command","write_stdin","spawn_agent","list_agents","wait_agent","close_agent","interrupt_agent","send_agent_message"]) if(!n.includes(k)) throw Error("missing "+k); if(n.includes("Bash")||n.includes("BashOutput")||n.includes("Agent")||n.includes("SubmitResult")) throw Error("legacy catalog"); text("CANONICAL_CATALOG_OK");', 'CANONICAL_CATALOG_OK'),
            execute('text(await tools.Read({file_path:"read.txt"}));', 'CODE_READ_MARKER'),
            execute('text(await tools.Write({file_path:"created.txt",content:"CODE_WRITE_EFFECT"})); text(await tools.exec_command({cmd:"printf CODE_SHELL_MARKER; printf x >> counter.txt"}));', 'CODE_SHELL_MARKER'),
            execute('const r=await tools.exec_command({cmd:"sleep 0.5; printf RETAINED_SHELL_OK",yield_time_ms:250}); if(!Number.isInteger(r.session_id)||r.content!==undefined) throw Error("expected canonical retained session"); store("shell",r.session_id); text("RETAINED_SESSION_OK");', 'RETAINED_SESSION_OK'),
            execute('const r=await tools.write_stdin({session_id:load("shell"),yield_time_ms:1000}); if(r.exit_code!==0) throw Error("shell did not exit"); text(r.output);', 'RETAINED_SHELL_OK'),
            execute('try { await tools.Bash({command:"touch legacy-bypass.txt"}); throw Error("legacy Bash callable"); } catch(e) { if(e.code!=="TOOL_NOT_AVAILABLE") throw e; text("LEGACY_BASH_REJECTED"); }', 'LEGACY_BASH_REJECTED'),
            execute('try { await tools.DoesNotExist({}); } catch(e) { if(e.code!=="TOOL_NOT_AVAILABLE") throw e; text("MISSING_TOOL_CAUGHT"); }', 'MISSING_TOOL_CAUGHT'),
            execute('text(await tools.Read({file_path:"missing.txt"}));', 'No such file', failed=True),
            execute('const = ;', failed=True),
            execute('text(await tools.Read({file_path:"created.txt"}));', 'CODE_WRITE_EFFECT'),
            execute('text("BEFORE_YIELD"); await yield_control(); await new Promise(r=>setTimeout(r,100)); text("AFTER_YIELD");', 'Script running with cell ID'),
            ('wait', lambda: {'cell_id': cell['id'], 'yield_time_ms': 1000}, False, 'AFTER_YIELD'),
            execute('const r=await tools.Read({file_path:"pixel.png"}); for(const b of r.content) if(b.type==="image") image(b);', 'IMAGE_BLOCK'),
            execute('const r=await tools.exec_command({cmd:"sleep 1; printf retained > cancelled.txt",yield_time_ms:250}); store("cancelShell",r.session_id); await yield_control(); text(r);', 'Script running with cell ID'),
            ('wait', lambda: {'cell_id': cell['id'], 'terminate': True}, False, None),
            execute('const r=await tools.write_stdin({session_id:load("cancelShell"),yield_time_ms:2000}); if(r.exit_code!==0) throw Error("retained shell failed"); text("CANCEL_RECOVERY");', 'CANCEL_RECOVERY'),
            execute('text(await tools.TaskCreate({subject:"Code task",description:"real task board"}));', 'Code task'),
            execute('text(await tools.TaskUpdate({taskId:"1",status:"in_progress"}));', 'in_progress'),
            execute('const c=await tools.spawn_agent({role:"fixture",task:"CODE_CHILD_MARKER",harness:null,model:null,thinking:null,output_contract:{kind:"string"}}); if(!Number.isInteger(c.agent_id)||c.content!==undefined) throw Error("canonical spawn must return direct JSON"); store("child",c.agent_id); text(c);'),
            execute('text(await tools.list_agents({include_completed:true})); text(await tools.wait_agent({agent_ids:[load("child")],timeout_ms:10000}));', 'CHILD_CODE_OK'),
            execute('text(await tools.close_agent({agent_id:load("child")}));', 'closed'),
        ], extra=['--local-durability', str(artifact / 'session.sqlite'), '--local-durability-state-id', 'code-session'], child=[execute('text(await tools.submit_result({output:"CHILD_CODE_OK"}));')])
        require((workspace / 'created.txt').read_text() == 'CODE_WRITE_EFFECT', 'nested Write effect absent')
        require((workspace / 'counter.txt').read_text() == 'x', 'nested exec_command effect repeated or absent')
        require((workspace / 'cancelled.txt').read_text() == 'retained', 'terminated cell lost retained shell session')
        require(not (workspace / 'legacy-bypass.txt').exists(), 'legacy Bash effect executed')
        require(not (workspace / 'stale-direct.txt').exists(), 'stale direct tool bypassed Code Mode')
        require(not (workspace / 'denied.txt').exists() and not (workspace / 'rewrite.txt').exists(), 'nested permission bypass')
        require((workspace / 'hook-calls.log').read_text().splitlines() == ['rewrite.txt', 'created.txt'], 'denied Write reached hook or rewrite hook not run')
        checks += ['only exec/wait exposed; real nested Read/Write/exec_command effects', 'nested Write denies before hooks and after hook rewrite', 'missing tool/file and syntax error followed by successful Read', 'explicit yield_control resumes through wait', 'native image forwarded through image() to Messages', 'terminate yielded cell preserves shell session for write_stdin', 'canonical spawn/list/wait/submit/close child journey']
        run('reopen', [('wait', lambda: {'cell_id': cell['id']}, True, None), execute('text(await tools.TaskGet({taskId:"1"}));', 'in_progress'), execute('text(await tools.Read({file_path:"created.txt"}));', 'CODE_WRITE_EFFECT')], extra=['--local-durability', str(artifact / 'session.sqlite'), '--local-durability-state-id', 'code-session'])
        checks.append('task board and file effects survive real process restart')
        interrupt_pending_inference()
        checks.append('real TUI cancel settles during held inference; retained native process polled successfully in next turn')
        require(hashlib.sha256(binary.read_bytes()).hexdigest() == binary_sha256, 'binary changed during journey; rerun after build completes')
        outcome = {'success': True, 'checks': checks, 'provider_requests': len(requests), 'binary_sha256': binary_sha256}
    except Exception as error:
        outcome['error'] = str(error)
        raise
    finally:
        (artifact / 'scenario.json').write_text(json.dumps({'commands': commands, 'environment': environment, 'expected': ['only exec/wait by default, canonical nested shared tools, rejected stale direct Write', 'real Read/Write/exec_command effects', 'denial before and after rewrite hook', 'errors recover through later exec', 'yield/wait and image forwarding', 'terminated cell preserves retained shell', 'canonical child submits structured result and closes', 'new process restores task board and rejects old cell', 'real terminal cancellation during held model request preserves retained process for next-turn write_stdin'], 'boundary': 'shipped CLI; real QuickJS, native tools, hooks, shared child runtime, SQLite; synthetic Messages SSE inference only'}, indent=2))
        (artifact / 'outcome.json').write_text(json.dumps(outcome, indent=2))
        server.shutdown()
        print(json.dumps({'artifact': str(artifact), **outcome}))


if __name__ == '__main__':
    main()
