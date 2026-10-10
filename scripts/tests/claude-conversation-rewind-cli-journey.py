#!/usr/bin/env python3
"""Shipped CLI, SQLite and PTY rewind acceptance. Only external inference is synthetic."""
from claude_code_fixture import normalize_request, wrap_tool
import argparse
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import sqlite3
import struct
import subprocess
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from importlib.util import module_from_spec, spec_from_file_location
from uuid import uuid4

spec = spec_from_file_location('resume_fixture', Path(__file__).with_name('claude-resume-cli-journey.py'))
fixture = module_from_spec(spec)
spec.loader.exec_module(fixture)
require, sse = fixture.require, fixture.sse


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', type=Path, default=Path('output/claude-conversation-rewind') / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    workspace, home = artifact / 'workspace', artifact / 'home'
    workspace.mkdir(parents=True)
    home.mkdir()
    (workspace / 'edited.txt').write_text('original')
    requests, errors, commands, checks = [], [], [], []
    phase = {'name': 'first', 'start': 0}
    steps = {
        'first': [('Write', {'file_path': 'edited.txt', 'content': 'first'}), ('exec_command', {'cmd': 'printf x >> counter.txt'})],
        'second': [],
        'third': [('Write', {'file_path': 'edited.txt', 'content': 'third'}), ('exec_command', {'cmd': 'printf x >> counter.txt'})],
        'branch': [], 'parent': [], 'pending': [], 'old-id': [], 'empty': [],
        'deny-root': [('Write', {'file_path': 'denied-policy.txt', 'content': 'BAD'})],
        'deny-branch': [('Write', {'file_path': 'denied-policy.txt', 'content': 'BAD'})],
        'plan-root': [('Write', {'file_path': 'denied-plan.txt', 'content': 'BAD'})],
        'plan-branch': [('Write', {'file_path': 'denied-plan.txt', 'content': 'BAD'})],
        'work-root': [('EnterWorktree', {'name': 'rewind-owned'})],
        'work-branch': [('Write', {'file_path': 'rewind-workspace.txt', 'content': 'saved workspace'}), ('ExitWorktree', {})],
    }
    blocked = threading.Event()
    release = threading.Event()

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['content-length']))); request = normalize_request(request, artifact)
            name, stage = phase['name'], len(requests) - phase['start']
            requests.append({'phase': name, 'request': request})
            try:
                require(self.path == '/v1/messages', 'unexpected provider path')
                if name == 'pending':
                    blocked.set()
                    release.wait(20)
                history = json.dumps(request['messages'])
                if name in ('branch', 'old-id') and stage == 0:
                    require('first-prompt-marker' in history and 'first-complete' in history, 'branch lost retained first turn')
                    require('second-prompt-marker' not in history and 'third-prompt-marker' not in history, 'branch retained discarded turns')
                    checks.append(name + ': exact before-turn transcript')
                if name == 'empty':
                    require(all(marker not in history for marker in ('first-prompt-marker', 'second-prompt-marker', 'third-prompt-marker')), 'first-turn rewind retained old history')
                    checks.append('first-turn rewind starts with empty history')
                if name == 'parent':
                    require(all(marker in history for marker in ('first-prompt-marker', 'second-prompt-marker', 'third-prompt-marker')), 'original history changed')
                    checks.append('parent original transcript continued')
                if stage and stage <= len(steps[name]):
                    call = f'{name}_{stage - 1}'
                    receipts = [b for m in request['messages'] for b in m.get('content', []) if isinstance(b, dict) and b.get('type') == 'tool_result' and b.get('tool_use_id') == call]
                    require(len(receipts) == 1 and bool(receipts[0].get('is_error', False)) == (name.startswith(('deny-', 'plan-')) or (name == 'work-branch' and stage == 2)), f'wrong receipt {call}: {receipts}')
                if name == 'old-id':
                    # This identity existed only in the discarded third turn.
                    block = {'type': 'tool_use', 'id': 'third_1', 'name': 'exec_command', 'input': {'cmd': 'printf BAD >> counter.txt'}}
                elif stage < len(steps[name]):
                    tool, inputs = steps[name][stage]
                    block = {'type': 'tool_use', 'id': f'{name}_{stage}', 'name': tool, 'input': inputs}
                else:
                    block = {'type': 'text', 'text': name + '-complete'}
            except Exception as error:
                errors.append(str(error))
                block = {'type': 'text', 'text': 'fixture-assertion-failed'}
            (artifact / 'provider.json').write_text(json.dumps(requests, indent=2))
            response = sse(wrap_tool(block), request['model'])
            try:
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.send_header('Content-Length', str(len(response)))
                self.end_headers()
                self.wfile.write(response)
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    env = {'HOME': str(home), 'CODEX_HOME': str(home / 'codex'), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'TERM': 'xterm-256color', 'NANOCODEX_COMPUTER': 'off'}
    common = ['--claude', '--claude-api-key', 'synthetic-rewind-key', '--claude-messages-url', f'http://127.0.0.1:{server.server_port}/v1/messages', '--browser=none', '--mcp-defaults', 'false', '--mcp-codex-config', 'false', '--web-search', 'false', '--image-generation', 'false', '--subagents', 'false', '--memory', 'false']
    database = home / 'codex/claude/sessions.sqlite'
    source = 'rewind-source'

    def run_command(name, command, success=True):
        commands.append({'name': name, 'argv': command, 'environment': env, 'cwd': str(workspace)})
        (artifact / 'commands.json').write_text(json.dumps(commands, indent=2))
        result = subprocess.run(command, cwd=workspace, env=env, capture_output=True, timeout=45)
        (artifact / (name + '.stdout')).write_bytes(result.stdout)
        (artifact / (name + '.stderr')).write_bytes(result.stderr)
        require((result.returncode == 0) == success, f'{name}: exit {result.returncode}: {result.stderr.decode(errors="replace")}')
        require(not errors, '; '.join(errors))
        return result

    def turn_command(name, session=source):
        return [str(binary), 'run', *common, '--model', 'claude-sonnet-5-5', '--thinking', 'medium', '--cwd', str(workspace), '--rollouts', 'false', '--local-durability', str(database), '--local-durability-state-id', session, '--request-id', name, name + '-prompt-marker']

    def turn(name, session=source, success=True):
        phase.update(name=name, start=len(requests))
        return run_command(name, turn_command(name, session), success)

    def rewind(name, mode='conversation', checkpoint=None, restore=False, session=source, success=True):
        command = [str(binary), 'rewind', session, '--mode', mode]
        if checkpoint:
            command.extend(['--checkpoint', checkpoint])
        if restore:
            command.append('--restore')
        return run_command(name, command, success)

    def rows():
        # Inspection only: all state writes occur through the shipped CLI/API.
        with sqlite3.connect(f'file:{database}?mode=ro', uri=True) as db:
            return db.execute('SELECT state_id, payload FROM nanocodex_durable_states ORDER BY state_id').fetchall()

    def pty_resume(branch, name="branch", cwd=None):
        phase.update(name=name, start=len(requests))
        command = [str(binary), 'resume', branch, *common, '--prompt', name + '-followup-marker']
        commands.append({'name': 'branch-pty', 'argv': command, 'environment': env})
        (artifact / 'commands.json').write_text(json.dumps(commands, indent=2))
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 45, 180, 0, 0))
        child = subprocess.Popen(command, cwd=cwd or workspace, env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        transcript = bytearray()
        deadline, exited = time.monotonic() + 40, 0
        try:
            while time.monotonic() < deadline:
                if select.select([master], [], [], .1)[0]:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            break
                        raise
                    transcript.extend(chunk)
                    if b'\x1b[6n' in chunk:
                        os.write(master, b'\x1b[1;1R')
                # Streaming text can precede durable settlement. Do not exit and
                # accidentally cancel the turn just because its text is visible.
                state = json.loads(dict(rows())[branch])['nanocodex_durable_state']
                settled = bool(state['operations']) and all(isinstance(op['status'], dict) and 'completed' in op['status'] for op in state['operations'].values())
                if (name + '-complete').encode() in transcript and settled and time.monotonic() - exited > .5:
                    os.write(master, b'\x03\x03')
                    exited = time.monotonic()
                if child.poll() is not None:
                    break
            require((name + '-complete').encode() in transcript, 'PTY branch response missing')
            child.wait(timeout=3)
            require(child.returncode == 0, 'PTY branch resume failed')
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            os.close(master)
            (artifact / (name + '.pty.log')).write_bytes(transcript)
        require(not errors, '; '.join(errors))
        checks.append(name + ': new branch resumed through real PTY')

    outcome = {'success': False, 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest()}
    try:
        for name in ('first', 'second', 'third'):
            turn(name)
        require((workspace / 'counter.txt').read_text() == 'xx', 'initial shell count wrong')
        original_rows = rows()
        preview = json.loads(rewind('preview').stdout)
        require([v['checkpoint'] for v in preview['checkpoints']] == ['first', 'second', 'third'], 'preview order wrong')
        require(rows() == original_rows, 'preview changed journal')
        selected = json.loads(rewind('selected-preview', 'files-and-conversation', 'second').stdout)
        require(selected['discarded_turns'] == ['second', 'third'], 'selected preview boundary wrong')
        require(selected['files']['changes'][0]['path'] == 'edited.txt', 'combined preview missed later file edit')
        require(rows() == original_rows and (workspace / 'edited.txt').read_text() == 'third', 'selected preview mutated state')
        rewind('unknown-preview-selection', checkpoint='absent', success=False)
        rewind('missing-selection', restore=True, success=False)
        rewind('unknown-selection', checkpoint='absent', restore=True, success=False)
        require(rows() == original_rows, 'invalid selection changed journal payload')
        before = len(requests)
        result = json.loads(rewind('conversation', checkpoint='second', restore=True).stdout)
        branch = result['branch_session']
        require(branch != source and len(branch) == 36, 'branch identity not fresh UUID')
        require((workspace / 'edited.txt').read_text() == 'third', 'conversation-only changed file')
        require(dict(rows())[source] == dict(original_rows)[source], 'parent payload changed')
        require(len(requests) == before, 'rewind contacted provider')
        pty_resume(branch)
        # Discarded tool IDs must not be executable even in the new branch.
        count_before_stale = len(requests)
        stale = turn('old-id', branch, success=False)
        require(len(requests) == count_before_stale + 1, 'stale-ID fixture never reached provider')
        require(b'reused an admitted tool_use id' in stale.stderr, 'stale identity failure missing')
        require((workspace / 'counter.txt').read_text() == 'xx', 'discarded tool identity re-executed')
        checks.append('discarded tool ID refused; external effects remain exactly twice')
        # Conflict is checked before any branch is published or files changed.
        count = len(rows())
        (workspace / 'edited.txt').write_text('external')
        conflict = rewind('file-conflict', 'files-and-conversation', 'second', True, success=False)
        require(b'external modification' in conflict.stderr and len(rows()) == count, 'conflict published branch')
        require((workspace / 'edited.txt').read_text() == 'external', 'conflict overwrote external file')
        (workspace / 'edited.txt').write_text('third')
        result = json.loads(rewind('combined', 'files-and-conversation', 'second', True).stdout)
        require(result['files']['restored'] and (workspace / 'edited.txt').read_text() == 'first', 'combined failed to find later file turn')
        require(result['branch_session'] != branch, 'combined reused branch')
        require((workspace / 'counter.txt').read_text() == 'xx', 'combined replayed or undid exec_command')
        checks.append('combined restored earliest affected later file turn; exec_command unchanged')
        empty = json.loads(rewind('before-first', checkpoint='first', restore=True).stdout)['branch_session']
        turn('empty', empty)
        require((workspace / 'counter.txt').read_text() == 'xx', 'first-turn rewind replayed exec_command')
        turn('parent')
        # Branching must retain current host permission/planning restrictions,
        # which are session-keyed state outside the provider transcript.
        permissions = artifact / 'permissions.json'
        permissions.write_text(json.dumps({'permissions': {'deny': ['Write'], 'defaultMode': 'bypassPermissions'}}))
        for prefix, flags, forbidden in (
            ('deny', ['--claude-permissions', str(permissions)], 'denied-policy.txt'),
            ('plan', ['--permission-mode', 'plan'], 'denied-plan.txt'),
        ):
            root_name, restricted_source = prefix + '-root', prefix + '-source'
            phase.update(name=root_name, start=len(requests))
            run_command(root_name, turn_command(root_name, restricted_source) + flags)
            require(not (workspace / forbidden).exists(), prefix + ': source restriction failed')
            restricted_branch = json.loads(rewind(prefix + '-rewind', checkpoint=root_name, restore=True, session=restricted_source).stdout)['branch_session']
            pty_resume(restricted_branch, prefix + '-branch')
            require(not (workspace / forbidden).exists(), prefix + ': rewind branch lost source restriction')
            checks.append(prefix + ': current restrictive state survives rewind and PTY resume')
        # Build source workspace state only through public EnterWorktree/Git.
        def git(*arguments):
            subprocess.run(['git', '-C', str(workspace), *arguments], check=True, capture_output=True)
        git('init', '-q')
        git('config', 'user.email', 'fixture@example.invalid')
        git('config', 'user.name', 'Synthetic rewind fixture')
        (workspace / '.gitignore').write_text('.claude/worktrees/\n')
        git('add', '.')
        git('commit', '-qm', 'synthetic rewind base')
        turn('work-root', 'work-source')
        owned_workspace = workspace / '.claude/worktrees/rewind-owned'
        require(owned_workspace.is_dir(), 'source owned worktree missing')
        work_branch = json.loads(rewind('work-rewind', checkpoint='work-root', restore=True, session='work-source').stdout)['branch_session']
        alternate = artifact / 'alternate-workspace'
        alternate.mkdir()
        request_count = len(requests)
        mismatch = run_command('workspace-mismatch', [str(binary), 'resume', work_branch, *common, '--cwd', str(alternate)], success=False)
        require(b'resumed Claude workspace' in mismatch.stderr and len(requests) == request_count, 'alternate cwd was not refused before provider admission')
        pty_resume(work_branch, 'work-branch', alternate)
        require((owned_workspace / 'rewind-workspace.txt').read_text() == 'saved workspace', 'rewind branch lost source workspace binding')
        require(not (alternate / 'rewind-workspace.txt').exists() and owned_workspace.is_dir(), 'branch retargeted workspace or adopted source cleanup ownership')
        checks.append('alternate cwd refused; current workspace survives rewind; source retains Git cleanup ownership')
        phase.update(name='pending', start=len(requests))
        process = subprocess.Popen(turn_command('pending'), cwd=workspace, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            require(blocked.wait(15), 'pending provider request not reached')
            process.kill()
            out, err = process.communicate(timeout=5)
            (artifact / 'pending.stdout').write_bytes(out)
            (artifact / 'pending.stderr').write_bytes(err)
            count = len(rows())
            pending = rewind('reject-pending', checkpoint='second', restore=True, success=False)
            require(b'pending operations' in pending.stderr and len(rows()) == count, 'pending operation accepted')
            checks.append('killed process pending operation rejected without branch')
        finally:
            release.set()
            if process.poll() is None:
                process.kill()
                process.wait()
        outcome.update(success=True, checks=checks, provider_requests=len(requests), branch_session=branch, source_session=source)
    except Exception as error:
        outcome.update(error=str(error), provider_errors=errors, checks=checks)
        raise
    finally:
        release.set()
        (artifact / 'outcome.json').write_text(json.dumps(outcome, indent=2))
        server.shutdown()
        print(json.dumps({'artifact': str(artifact), **outcome}, indent=2))


if __name__ == '__main__':
    main()
