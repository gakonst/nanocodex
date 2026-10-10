#!/usr/bin/env python3
"""Shipped CLI workspace transitions, durable reopen, multi-root rewind and safe cleanup.
Only the remote Messages provider is synthetic. All Git, tools and persistence are real.
"""
import argparse
import json
import subprocess
import threading
import shlex
from claude_code_fixture import wrap_tool, normalize_request
from pathlib import Path
from uuid import uuid4
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from importlib.util import spec_from_file_location, module_from_spec
spec = spec_from_file_location('fixture', Path(__file__).with_name('claude-hooks-cli-journey.py'))
f = module_from_spec(spec)
spec.loader.exec_module(f)
require, sse = f.require, f.sse

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', type=Path, default=Path('output/claude-worktree-cli') / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    root, home = artifact / 'repo', artifact / 'home'
    root.mkdir(parents=True)
    home.mkdir()
    def git(*argv):
        return subprocess.run(['git', '-C', str(root), *argv], check=True, capture_output=True).stdout.decode().strip()
    git('init', '-q')
    git('config', 'user.email', 'fixture@example.invalid')
    git('config', 'user.name', 'Synthetic fixture')
    (root / '.gitignore').write_text('.claude/worktrees/\n')
    (root / 'same.txt').write_text('base\n')
    (root / 'CLAUDE.md').write_text('ORIGINAL_CONTEXT_MARKER\n')
    skills = root / '.claude/skills/location'
    skills.mkdir(parents=True)
    (skills / 'SKILL.md').write_text('ORIGINAL_SKILL_MARKER\n')
    git('add', '.')
    git('commit', '-qm', 'fixture base')
    wt = root / '.claude/worktrees/isolation'
    steps = [
        ('Write', {'file_path': 'same.txt', 'content': 'parent changed\n'}, False, None),
        ('EnterWorktree', {'path': str(artifact / 'external')}, True, 'unsupported'),
        ('EnterWorktree', {'name': 'isolation'}, False, str(wt)),
        ('exec_command', {'cmd': 'pwd'}, False, str(wt)),
        ('Read', {'file_path': 'same.txt'}, False, 'base'),
        ('Write', {'file_path': 'same.txt', 'content': 'child changed\n'}, False, None),
        ('Write', {'file_path': '.claude/skills/location/SKILL.md', 'content': 'WORKTREE_SKILL_MARKER\n'}, False, None),
        ('Write', {'file_path': 'CLAUDE.md', 'content': 'WORKTREE_CONTEXT_MARKER\n'}, False, None),
        ('Skill', {'skill': 'location'}, False, 'WORKTREE_SKILL_MARKER'),
        ('ProjectContext', {'path': 'same.txt'}, False, 'WORKTREE_CONTEXT_MARKER'),
    ]
    phase = {'name': 'first', 'steps': steps, 'start': 0}
    requests, errors, commands, jobs = [], [], [], {}
    child_requests, child_counts = [], {}
    parent_exited = threading.Event()
    def text_of(block):
        content = block.get('content', '')
        return content if isinstance(content, str) else '\n'.join(b.get('text', '') for b in content)
    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_POST(self):
            request = normalize_request(json.loads(self.rfile.read(int(self.headers['content-length']))), artifact)
            user_text = '\n'.join(text_of(m) for m in request['messages'] if m['role'] == 'user')
            child = next((name for name in ('PINNED_CHILD_EXEC', 'FRESH_CHILD_EXEC', 'CLEAN_PIN_CHILD_EXEC') if name in user_text), None)
            if child:
                stage = child_counts.get(child, 0)
                child_counts[child] = stage + 1
                child_requests.append({'child': child, 'request': request})
                try:
                    expected_root = root / '.claude/worktrees/close-clean' if child == 'CLEAN_PIN_CHILD_EXEC' else root / '.claude/worktrees/pinned' if child == 'PINNED_CHILD_EXEC' else root
                    if stage == 0:
                        if child == 'PINNED_CHILD_EXEC':
                            require(parent_exited.wait(15), 'parent blocked before KEEP exit while child pending')
                        block = {'type': 'tool_use', 'id': child + '_pwd', 'name': 'exec_command', 'input': {'cmd': 'pwd'}}
                    elif stage == 1:
                        receipt = request['messages'][-1]['content'][-1]
                        require(not receipt.get('is_error', False) and str(expected_root) in text_of(receipt), f'child workspace not pinned: {receipt}')
                        block = {'type':'tool_use','id':child+'_result','name':'submit_result','input':{'output':child+' complete'}} if child == 'CLEAN_PIN_CHILD_EXEC' else {'type': 'tool_use', 'id': child + '_write', 'name': 'Write', 'input': {'file_path': child + '.txt', 'content': 'child effect'}}
                    elif stage == 2 and child != 'CLEAN_PIN_CHILD_EXEC':
                        require(not request['messages'][-1]['content'][-1].get('is_error', False), 'child Write failed')
                        block = {'type': 'tool_use', 'id': child + '_result', 'name': 'submit_result', 'input': {'output': child + ' complete'}}
                    else:
                        block = {'type': 'text', 'text': 'Child complete'}
                except Exception as error:
                    errors.append(str(error))
                    block = {'type': 'text', 'text': 'fixture-failed'}
                body = sse(wrap_tool(block), request['model'])
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            stage = len(requests) - phase['start']
            requests.append(request)
            if phase['name'] == 'children' and stage >= 4:
                parent_exited.set()
            try:
                require(self.path == '/v1/messages', 'wrong route')
                if stage:
                    cid = f"{phase['name']}_{stage-1}"
                    receipts = [b for m in request['messages'] for b in m.get('content', []) if b.get('type') == 'tool_result' and b.get('tool_use_id') == cid]
                    require(len(receipts) == 1, 'missing unique receipt ' + cid)
                    receipt = receipts[0]
                    prior = phase['steps'][stage-1]
                    require(bool(receipt.get('is_error', False)) == prior[2], f'wrong error {cid}: {receipt}')
                    if prior[3]: require(prior[3] in text_of(receipt), f'missing expected {prior[3]} in {receipt}')
                    if prior[0] == 'exec_command' and prior[1].get('yield_time_ms') == 1:
                        jobs['__job__'] = json.loads(text_of(receipt))['session_id']
                    if prior[0] == 'write_stdin':
                        require(json.loads(text_of(receipt)).get('exit_code') is not None, 'explicit PTY Ctrl-C did not stop process before cleanup')
                if stage < len(phase['steps']):
                    name, arguments, _, _ = phase['steps'][stage]
                    arguments = {k: jobs.get(v, v) if isinstance(v, str) else v for k, v in arguments.items()}
                    block = {'type': 'tool_use', 'id': f"{phase['name']}_{stage}", 'name': name, 'input': arguments}
                else:
                    require(stage == len(phase['steps']), 'unexpected retry')
                    block = {'type': 'text', 'text': 'worktree-journey-complete'}
            except Exception as error:
                errors.append(str(error))
                block = {'type': 'text', 'text': 'fixture-failed'}
            (artifact / 'provider.json').write_text(json.dumps(requests, indent=2))
            body = sse(wrap_tool(block), request['model'])
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    env = {'HOME': str(home), 'CODEX_HOME': str(home / 'codex'), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'NANOCODEX_COMPUTER': 'off'}
    command = [str(binary), 'run', '--claude', '--model', 'claude-sonnet-5-5', '--thinking', 'medium', '--claude-api-key', 'synthetic-key', '--claude-messages-url', f'http://127.0.0.1:{server.server_port}/v1/messages', '--cwd', str(root), '--rollouts', 'false', '--browser=none', '--mcp-defaults', 'false', '--mcp-codex-config', 'false', '--web-search', 'false', '--image-generation', 'false', '--subagents', 'false', '--memory', 'false', '--local-durability', str(artifact / 'session.sqlite'), '--local-durability-state-id', 'worktree-session']
    def run(name, argv):
        commands.append({'name': name, 'argv': argv, 'shell': shlex.join(argv)})
        (artifact / 'commands.json').write_text(json.dumps({'commands': commands, 'environment': env}, indent=2))
        result = subprocess.run(argv, cwd=root, env=env, capture_output=True, timeout=90)
        (artifact / f'{name}.stdout').write_bytes(result.stdout)
        (artifact / f'{name}.stderr').write_bytes(result.stderr)
        require(result.returncode == 0, f'{name}: {result.stderr.decode(errors="replace")}')
        require(not errors, '; '.join(errors))
        return result
    outcome = {'success': False}
    try:
        first = command + ['--request-id', 'first', 'Change files in an isolated worktree']
        run('first', first)
        require((root / 'same.txt').read_text() == 'parent changed\n', 'parent root mutation lost')
        require((wt / 'same.txt').read_text() == 'child changed\n', 'worktree write misrouted')
        require(not (artifact / 'external').exists(), 'external path rejection mutated destination')
        replay_requests = len(requests)
        run('replay', first)
        require(len(requests) == replay_requests, 'terminal replay called provider')
        require(git('worktree', 'list', '--porcelain').count('worktree ') == 2, 'duplicate worktree replay')
        phase.update(name='resume', start=len(requests), steps=[
            ('exec_command', {'cmd': 'pwd'}, False, str(wt)),
            ('Read', {'file_path': 'same.txt'}, False, 'child changed'),
            ('Skill', {'skill': 'location'}, False, 'WORKTREE_SKILL_MARKER'),
            ('ExitWorktree', {'cleanup': True}, True, 'cleanup refused'),
            ('ExitWorktree', {}, False, '"kept":true'),
            ('Read', {'file_path': 'same.txt'}, False, 'parent changed'),
            ('Skill', {'skill': 'location'}, False, 'ORIGINAL_SKILL_MARKER'),
            ('exec_command', {'cmd': 'pwd'}, False, str(root)),
        ])
        run('resume', command + ['--request-id', 'resume', '/location'])
        require('WORKTREE_SKILL_MARKER' in json.dumps(requests[phase['start']]['messages'][-1]), 'user slash skill did not resolve saved active workspace')
        require('WORKTREE_CONTEXT_MARKER' in json.dumps(requests[phase['start']].get('system')), 'resume system did not use saved current root')
        require(wt.is_dir(), 'KEEP exit deleted worktree')
        rewind = [str(binary), 'rewind', 'worktree-session']
        preview = json.loads(run('preview', rewind).stdout)
        roots = {entry['workspace'] for turn in preview['checkpoints'] for entry in turn['files']}
        require(roots == {str(root), str(wt)}, f'checkpoint roots lost: {roots}')
        first_turn = preview['checkpoints'][0]['checkpoint']
        restore = json.loads(run('restore', rewind + ['--before', first_turn, '--restore']).stdout)
        require(restore['restored'], 'multi-root restore not confirmed')
        require((root / 'same.txt').read_text() == (wt / 'same.txt').read_text() == 'base\n', 'multi-root same-path before-images not restored')
        # Retained shell sessions pin their worktree until write_stdin observes exit.
        # Explicit PTY input stops this process. Cancelling a turn/cell retains
        # shared Codex shell sessions (covered by the Code Mode journey).
        phase.update(name='cleanup', start=len(requests), steps=[
            ('EnterWorktree', {'name': 'clean'}, False, None),
            ('exec_command', {'cmd': 'sleep 30', 'tty': True, 'yield_time_ms': 1}, False, 'session_id'),
            ('ExitWorktree', {'cleanup': True}, True, 'active background or child'),
            ('write_stdin', {'session_id': '__job__', 'chars': '\u0003', 'yield_time_ms': 1000}, False, None),
            ('ExitWorktree', {'cleanup': True}, False, '"kept":false'),
        ])
        run('cleanup', command + ['--request-id', 'cleanup', 'Exercise explicit clean worktree cleanup'])
        require(not (root / '.claude/worktrees/clean').exists(), 'explicit clean cleanup left directory')
        require('claude/clean' not in git('branch', '--list'), 'cleanup left branch')
        phase.update(name='children', start=len(requests), steps=[
            ('EnterWorktree', {'name': 'pinned'}, False, None),
            ('spawn_agent', {'role': 'Pinned worktree child', 'task': 'PINNED_CHILD_EXEC verify retained workspace', 'output_contract': {'kind':'string'}}, False, 'agent_id'),
            ('ExitWorktree', {'cleanup': True}, True, 'active background or child'),
            ('ExitWorktree', {}, False, '"kept":true'),
            ('exec_command', {'cmd': 'pwd'}, False, str(root)),
            ('wait_agent', {'agent_ids': [1], 'timeout_ms': 20000}, False, 'PINNED_CHILD_EXEC complete'),
            ('spawn_agent', {'role': 'Fresh original workspace', 'task': 'FRESH_CHILD_EXEC verify new child workspace', 'output_contract': {'kind':'string'}}, False, 'agent_id'),
            ('wait_agent', {'agent_ids': [2], 'timeout_ms': 20000}, False, 'FRESH_CHILD_EXEC complete'),
            ('close_agent', {'agent_id': 1}, False, 'closed'),
            ('close_agent', {'agent_id': 2}, False, 'closed'),
            ('EnterWorktree', {'name':'close-clean'}, False, None),
            ('spawn_agent', {'role':'clean child','task':'CLEAN_PIN_CHILD_EXEC return a result without writing files','output_contract':{'kind':'string'}}, False, 'agent_id'),
            ('wait_agent', {'agent_ids':[3], 'timeout_ms':20000}, False, 'completed'),
            ('close_agent', {'agent_id':3}, False, 'closed'),
            ('ExitWorktree', {'cleanup':True}, False, '"kept":false'),
        ])
        child_command = list(command)
        child_command[child_command.index('--subagents') + 1] = 'true'
        run('children', child_command + ['--request-id', 'children', 'Verify child workspaces remain pinned'])
        require((root / '.claude/worktrees/pinned/PINNED_CHILD_EXEC.txt').read_text() == 'child effect', 'child write escaped pinned workspace')
        require((root / 'FRESH_CHILD_EXEC.txt').read_text() == 'child effect', 'new child did not snapshot current parent workspace')
        require(not (root / 'PINNED_CHILD_EXEC.txt').exists(), 'child retargeted with parent')
        outcome.update(success=True, cli_processes=len(commands), provider_requests=len(requests), child_provider_requests=len(child_requests), replay_provider_requests=0, multiroot_rewind=True, retained_shell_cleanup_refused=True, explicit_pty_ctrl_c_before_cleanup=True, child_cleanup_refused=True, evidence=str(artifact))
    finally:
        server.shutdown()
        (artifact / 'child-provider.json').write_text(json.dumps(child_requests, indent=2))
        (artifact / 'outcome.json').write_text(json.dumps(outcome, indent=2))
        print(json.dumps(outcome, indent=2))
if __name__ == '__main__': main()
