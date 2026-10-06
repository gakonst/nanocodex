#!/usr/bin/env python3
"""Real CLI cross-process native file capture, preview, conflicts and restore.
Only the external Messages/SSE provider is synthetic. Retain commands, requests,
stdout, stderr and outcome in output/ so this journey is independently replayable.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4
from importlib.util import module_from_spec, spec_from_file_location

spec = spec_from_file_location('hooks_fixture', Path(__file__).with_name('claude-hooks-cli-journey.py'))
fixture = module_from_spec(spec)
spec.loader.exec_module(fixture)
require, sse = fixture.require, fixture.sse


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', type=Path, default=Path('output/claude-checkpoints-cli') / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    workspace = artifact / 'workspace'
    workspace.mkdir(parents=True)
    (workspace / 'home').mkdir()
    original = b'original existing\n'
    (workspace / 'existing.txt').write_bytes(original)
    (workspace / 'existing.txt').chmod(0o640)
    notebook = json.dumps({'nbformat': 4, 'nbformat_minor': 5, 'metadata': {}, 'cells': [{'id': 'first', 'cell_type': 'code', 'metadata': {}, 'execution_count': None, 'outputs': [], 'source': ['print(1)\n']}]}).encode()
    (workspace / 'demo.ipynb').write_bytes(notebook)
    (workspace / 'large.txt').write_bytes(b'x' * (8 * 1024 * 1024 + 1))
    (workspace / 'linked.txt').symlink_to(workspace / 'existing.txt')
    os.mkfifo(workspace / 'pipe.txt')
    hook = workspace / 'hook.py'
    hook.write_text('''import json, sys
p = json.load(sys.stdin)
i = p['tool_input']
if i.get('file_path') == 'unapproved.txt':
    print(json.dumps({'hookSpecificOutput': {'hookEventName': 'PreToolUse', 'updatedInput': {'file_path': 'created.txt', 'content': 'approved content\\n'}}}))
elif i.get('file_path') == 'denied.txt':
    print(json.dumps({'hookSpecificOutput': {'hookEventName': 'PreToolUse', 'permissionDecision': 'deny', 'permissionDecisionReason': 'fixture denial'}}))
else:
    print('{}')
''')
    settings = artifact / 'hooks.json'
    settings.write_text(json.dumps({'hooks': {'PreToolUse': [{'matcher': '^Write$', 'hooks': [{'type': 'command', 'command': '/usr/bin/python3 ' + shlex.quote(str(hook))}]}]}}))
    steps1 = [
        ('Write', {'file_path': 'unapproved.txt', 'content': 'unapproved'}, False),
        ('Edit', {'file_path': 'existing.txt', 'old_string': 'original', 'new_string': 'first'}, False),
        ('NotebookEdit', {'notebook_path': 'demo.ipynb', 'cell_id': 'first', 'new_source': 'print(2)\n'}, False),
        ('Write', {'file_path': 'denied.txt', 'content': 'denied'}, True),
        ('Edit', {'file_path': 'existing.txt', 'old_string': 'absent', 'new_string': 'bad'}, True),
        ('Write', {'file_path': 'large.txt', 'content': 'bad'}, True),
        ('Write', {'file_path': 'linked.txt', 'content': 'bad'}, True),
        ('Write', {'file_path': 'pipe.txt', 'content': 'bad'}, True),
        ('Bash', {'command': 'printf unmanaged > bash.txt'}, False),
    ]
    steps2 = [
        ('Edit', {'file_path': 'existing.txt', 'old_string': 'first', 'new_string': 'second'}, False),
        ('Write', {'file_path': 'nested/new.txt', 'content': 'second turn\n'}, False),
    ]
    requests, errors, commands = [], [], []
    phase = {'name': 'first', 'steps': steps1, 'start': 0}

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['content-length'])))
            stage = len(requests) - phase['start']
            requests.append(request)
            try:
                require(self.path == '/v1/messages', 'wrong provider route')
                if stage:
                    call_id = f"{phase['name']}_{stage - 1}"
                    receipts = [b for m in request['messages'] for b in m.get('content', []) if isinstance(b, dict) and b.get('type') == 'tool_result' and b.get('tool_use_id') == call_id]
                    require(len(receipts) == 1, f'missing unique receipt {call_id}')
                    require(bool(receipts[0].get('is_error', False)) == phase['steps'][stage - 1][2], f'wrong error {call_id}: {receipts[0]}')
                if stage < len(phase['steps']):
                    name, inputs, _ = phase['steps'][stage]
                    block = {'type': 'tool_use', 'id': f"{phase['name']}_{stage}", 'name': name, 'input': inputs}
                else:
                    require(stage == len(phase['steps']), 'unexpected retry')
                    block = {'type': 'text', 'text': 'checkpoints-cli-journey-complete'}
            except Exception as error:
                errors.append(str(error))
                block = {'type': 'text', 'text': 'fixture-assertion-failed'}
            (artifact / 'provider.json').write_text(json.dumps(requests, indent=2))
            response = sse(block, request['model'])
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(response)))
            self.end_headers()
            self.wfile.write(response)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    environment = {'HOME': str(workspace / 'home'), 'CODEX_HOME': str(workspace / 'codex-home'), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'NANOCODEX_COMPUTER': 'off'}
    command = [str(binary), 'run', '--claude', '--model', 'claude-sonnet-5-5', '--thinking', 'medium', '--claude-api-key', 'synthetic-key', '--claude-messages-url', f'http://127.0.0.1:{server.server_port}/v1/messages', '--cwd', str(workspace), '--rollouts', 'false', '--browser=none', '--mcp-defaults', 'false', '--mcp-codex-config', 'false', '--web-search', 'false', '--image-generation', 'false', '--subagents', 'false', '--memory', 'false', '--claude-hooks', str(settings), '--local-durability', str(artifact / 'session.sqlite'), '--local-durability-state-id', 'checkpoint-session']

    def run(name, invocation, success=True):
        commands.append({'name': name, 'argv': invocation, 'shell': shlex.join(invocation)})
        (artifact / 'commands.json').write_text(json.dumps({'commands': commands, 'environment': environment}, indent=2))
        result = subprocess.run(invocation, cwd=workspace, env=environment, capture_output=True, timeout=90)
        (artifact / f'{name}.stdout').write_bytes(result.stdout)
        (artifact / f'{name}.stderr').write_bytes(result.stderr)
        require((result.returncode == 0) == success, f'{name}: unexpected exit {result.returncode}: {result.stderr.decode(errors="replace")}')
        require(not errors, '; '.join(errors))
        return result

    rewind = [str(binary), 'rewind', 'checkpoint-session']
    outcome = {'success': False}
    try:
        first = command + ['--request-id', 'first-operation', 'Make first changes']
        run('first', first)
        require((workspace / 'created.txt').read_text() == 'approved content\n', 'effective pre-hook input not used')
        require(not (workspace / 'unapproved.txt').exists() and not (workspace / 'denied.txt').exists(), 'unapproved effect')
        require((workspace / 'existing.txt').read_text() == 'first existing\n', 'first edit missing')
        before_replay = len(requests)
        run('replay', first)
        require(len(requests) == before_replay, 'committed turn replay called provider')
        preview1 = json.loads(run('preview-first', rewind).stdout)
        require(len(preview1['checkpoints']) == 1, 'expected single first turn')
        turn1 = preview1['checkpoints'][0]['checkpoint']
        files = preview1['checkpoints'][0]['files']
        require(len(files) == 4, f'wrong checkpoints (denied/bounded/symlink/Bash must be excluded): {files}')
        require(not any(f['path'] == 'unapproved.txt' for f in files), 'checkpoint captured original rather than effective input')
        run('missing-selection', rewind + ['--restore'], False)
        run('unknown-checkpoint', rewind + ['--checkpoint', 'unknown', '--restore'], False)
        run('unknown-session', [str(binary), 'rewind', 'missing-session'], False)
        phase.update(name='second', steps=steps2, start=len(requests))
        run('second', command + ['--request-id', 'second-operation', 'Make second changes'])
        preview2 = json.loads(run('preview-second', rewind).stdout)
        require(len(preview2['checkpoints']) == 2, 'second process did not retain first checkpoint')
        turn2 = preview2['checkpoints'][1]['checkpoint']
        (workspace / 'existing.txt').write_text('external user content\n')
        conflict = run('external-conflict', rewind + ['--checkpoint', turn1, '--restore'], False)
        require(b'external modification' in conflict.stderr, 'external conflict not explained')
        require((workspace / 'created.txt').exists() and (workspace / 'nested/new.txt').exists(), 'conflict partially restored another file')
        require((workspace / 'existing.txt').read_text() == 'external user content\n', 'external edit overwritten')
        (workspace / 'existing.txt').write_text('second existing\n')
        preview = json.loads(run('selected-preview', rewind + ['--checkpoint', turn2]).stdout)
        require(not preview['restored'] and (workspace / 'nested/new.txt').exists(), 'preview changed files')
        restored = json.loads(run('restore-second', rewind + ['--checkpoint', turn2, '--restore']).stdout)
        require(restored['restored'], 'restore not confirmed')
        require((workspace / 'existing.txt').read_text() == 'first existing\n' and not (workspace / 'nested/new.txt').exists(), 'selected second checkpoint wrong')
        require((workspace / 'created.txt').exists(), 'second restore changed first turn file')
        run('duplicate-restore', rewind + ['--checkpoint', turn2, '--restore'], False)
        run('restore-first', rewind + ['--checkpoint', turn1, '--restore'])
        require((workspace / 'existing.txt').read_bytes() == original, 'existing before-image not restored')
        require((workspace / 'existing.txt').stat().st_mode & 0o777 == 0o640, 'original mode not restored')
        require((workspace / 'demo.ipynb').read_bytes() == notebook, 'notebook before-image not restored byte-for-byte')
        require(not (workspace / 'created.txt').exists(), 'new file not removed')
        require((workspace / 'bash.txt').read_text() == 'unmanaged', 'rewind incorrectly claims arbitrary Bash effects')
        journals = list((workspace / 'codex-home/claude/checkpoints').glob('*.json'))
        require(len(journals) == 1, 'missing persistent journal')
        require(journals[0].stat().st_mode & 0o777 == 0o600, 'private before-images have wrong permissions')
        require(journals[0].parent.stat().st_mode & 0o777 == 0o700, 'private directory has wrong permissions')
        outcome.update(success=True, checkpoint1=turn1, checkpoint2=turn2, provider_requests=len(requests), cli_processes=len(commands), evidence=str(artifact))
    finally:
        server.shutdown()
        (artifact / 'outcome.json').write_text(json.dumps(outcome, indent=2))
        print(json.dumps(outcome, indent=2))


if __name__ == '__main__':
    main()
