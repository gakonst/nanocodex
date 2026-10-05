#!/usr/bin/env python3
"""Exercise explicit command hooks through the shipped CLI and real child processes.

Only the Anthropic Messages provider is synthetic. Evidence includes CLI output,
HTTP requests, exact commands, hook stdin, settings and filesystem effects.
"""
import argparse
import json
from pathlib import Path
import shlex
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def sse(block, model):
    tool = block['type'] == 'tool_use'
    start = dict(block)
    delta = {'type': 'input_json_delta', 'partial_json': json.dumps(start.pop('input'))} if tool else {'type': 'text_delta', 'text': start.pop('text')}
    start['input' if tool else 'text'] = {} if tool else ''
    events = [
        {'type': 'message_start', 'message': {'id': 'msg_' + uuid4().hex, 'type': 'message', 'role': 'assistant', 'model': model, 'content': [], 'usage': {'input_tokens': 10, 'output_tokens': 0}}},
        {'type': 'content_block_start', 'index': 0, 'content_block': start},
        {'type': 'content_block_delta', 'index': 0, 'delta': delta},
        {'type': 'content_block_stop', 'index': 0},
        {'type': 'message_delta', 'delta': {'stop_reason': 'tool_use' if tool else 'end_turn'}, 'usage': {'output_tokens': 10}},
        {'type': 'message_stop'},
    ]
    return ''.join('data: ' + json.dumps(event) + '\n\n' for event in events).encode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', type=Path, default=Path('output/claude-hooks-cli') / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    workspace = artifact / 'workspace'
    (workspace / 'home').mkdir(parents=True)
    (workspace / '.claude').mkdir()
    hook = workspace / 'hook.py'
    hook.write_text('''import json, os, subprocess, sys
p = json.load(sys.stdin)
mode = sys.argv[1]
with open('hooks.jsonl', 'a') as f:
    f.write(json.dumps(dict(p, mode=mode)) + '\\n')
event = p['hook_event_name']
command = p['tool_input'].get('command', '')
if mode == 'observer':
    print('{}')
elif event == 'PreToolUse':
    if 'case-rewrite' in command:
        print(json.dumps({'hookSpecificOutput': {'hookEventName': event, 'updatedInput': {'command': "printf rewritten > rewritten.txt; printf retained-rewrite"}}}))
    elif 'case-deny' in command:
        print(json.dumps({'hookSpecificOutput': {'hookEventName': event, 'permissionDecision': 'deny', 'permissionDecisionReason': 'synthetic denial'}}))
    elif 'case-ask' in command:
        print(json.dumps({'hookSpecificOutput': {'hookEventName': event, 'permissionDecision': 'ask'}}))
    elif 'case-timeout' in command:
        subprocess.Popen(['/bin/sh', '-c', 'sleep 0.7; printf leaked > timeout-leak.txt'])
        os._exit(0)
    elif 'case-overflow' in command:
        subprocess.Popen(['/bin/sh', '-c', 'sleep 0.7; printf leaked > overflow-leak.txt'])
        print('x' * 100000, flush=True)
    elif 'case-malformed' in command:
        print(json.dumps({'continue': 'false'}))
    elif 'case-exit2' in command:
        print('synthetic exit-two denial', file=sys.stderr)
        sys.exit(2)
    else:
        print('{}')
elif event == 'PostToolUse' and 'case-postfail' in command:
    print(json.dumps({'decision': 'block', 'reason': 'synthetic post failure'}))
else:
    print('{}')
''')
    command_hook = {'type': 'command', 'command': '/usr/bin/python3 ' + shlex.quote(str(hook)) + ' primary', 'timeout': 0.25}
    observer = dict(command_hook, command=command_hook['command'].replace(' primary', ' observer'), timeout=2)
    settings = {'hooks': {
        'PreToolUse': [
            {'matcher': '^Bash$', 'hooks': [command_hook, observer]},
            {'matcher': '^Write$', 'hooks': [{'type': 'command', 'command': 'touch matcher-leak.txt'}]},
        ],
        'PostToolUse': [{'matcher': '^Bash$', 'hooks': [observer, command_hook]}],
        'PostToolUseFailure': [{'matcher': '^Read$', 'hooks': [observer]}],
    }}
    settings_path = artifact / 'hooks.json'
    settings_path.write_text(json.dumps(settings, indent=2))
    (workspace / '.claude/settings.json').write_text(json.dumps({'hooks': {'PreToolUse': [{'hooks': [{'type': 'command', 'command': 'touch implicit-leak.txt'}]}]}}))
    steps = [
        ('Bash', {'command': 'touch original.txt # case-rewrite'}, False, 'retained-rewrite'),
        ('Bash', {'command': 'touch denied.txt # case-deny'}, True, 'synthetic denial'),
        ('Bash', {'command': 'touch ask.txt # case-ask'}, True, 'no hook approval UI'),
        ('Bash', {'command': 'touch timed.txt # case-timeout'}, True, 'timed out'),
        ('Bash', {'command': 'touch overflow.txt # case-overflow'}, True, 'exceeded 65536'),
        ('Bash', {'command': 'touch malformed.txt # case-malformed'}, True, 'continue must be a boolean'),
        ('Bash', {'command': 'touch exit2.txt # case-exit2'}, True, 'synthetic exit-two denial'),
        ('Bash', {'command': 'printf effect > post-effect.txt; printf retained-post # case-postfail'}, True, 'retained-post'),
        ('Read', {'file_path': 'missing.txt'}, True, None),
        ('Bash', {'command': 'touch input-overflow.txt #' + 'x' * 1048576}, True, 'stdin exceeded 1048576'),
        ('Bash', {'command': 'printf x >> recovered.txt; printf recovered'}, False, 'recovered'),
    ]
    requests, errors = [], []
    phase = {'name': 'explicit', 'start': 0, 'steps': steps}

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['content-length'])))
            stage = len(requests) - phase['start']
            requests.append(request)
            try:
                require(self.path == '/v1/messages', 'unexpected provider route')
                if stage:
                    expected_id = f"{phase['name']}_{stage - 1}"
                    receipts = [b for m in request['messages'] for b in m.get('content', []) if isinstance(b, dict) and b.get('type') == 'tool_result' and b.get('tool_use_id') == expected_id]
                    require(len(receipts) == 1, f'missing unique receipt {expected_id}')
                    receipt = receipts[0]
                    _, _, failed, marker = phase['steps'][stage - 1]
                    require(bool(receipt.get('is_error', False)) == failed, f'wrong error for {expected_id}: {receipt}')
                    text = json.dumps(receipt['content'])
                    require(not marker or marker in text, f'missing {marker}: {receipt}')
                    if phase['name'] == 'explicit' and stage == 8:
                        require('synthetic post failure' in text and 'does not undo' in text, 'post-hook failure erased or misrepresented effect')
                if stage < len(phase['steps']):
                    name, inputs, _, _ = phase['steps'][stage]
                    block = {'type': 'tool_use', 'id': f"{phase['name']}_{stage}", 'name': name, 'input': inputs}
                else:
                    require(stage == len(phase['steps']), 'unexpected retry')
                    block = {'type': 'text', 'text': 'hooks-cli-journey-complete'}
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
    command = [str(binary), 'run', '--claude', '--model', 'claude-sonnet-5-5', '--thinking', 'medium', '--claude-api-key', 'synthetic-claude-key', '--claude-messages-url', f'http://127.0.0.1:{server.server_port}/v1/messages', '--cwd', str(workspace), '--rollouts', 'false', '--browser=none', '--mcp-defaults', 'false', '--mcp-codex-config', 'false', '--web-search', 'false', '--image-generation', 'false', '--subagents', 'false', '--memory', 'false']
    explicit = command + ['--claude-hooks', str(settings_path), '--local-durability', str(artifact / 'session.sqlite'), '--local-durability-state-id', 'hooks-cli', '--request-id', 'hooks-operation', 'Exercise explicit hooks.']
    environment = {'HOME': str(workspace / 'home'), 'CODEX_HOME': str(workspace / 'codex-home'), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'NANOCODEX_COMPUTER': 'off'}
    commands = []
    outcome = {'success': False}

    def run(name, invocation, success=True):
        commands.append({'name': name, 'argv': invocation, 'shell': shlex.join(invocation)})
        (artifact / 'commands.json').write_text(json.dumps({'commands': commands, 'environment': environment}, indent=2))
        result = subprocess.run(invocation, cwd=workspace, env=environment, capture_output=True, timeout=60)
        (artifact / f'{name}.jsonl').write_bytes(result.stdout)
        (artifact / f'{name}.stderr.log').write_bytes(result.stderr)
        require((result.returncode == 0) == success, f'{name} unexpected exit {result.returncode}: {result.stderr.decode(errors="replace")}')
        require(not errors, '; '.join(errors))
        if success:
            require(b'hooks-cli-journey-complete' in result.stdout, f'{name} final answer missing')

    try:
        run('explicit', explicit)
        require(len(requests) == len(steps) + 1, 'wrong provider request count')
        hook_log = (workspace / 'hooks.jsonl').read_text()
        records = [json.loads(line) for line in hook_log.splitlines()]
        for record in records:
            for key in ('session_id', 'turn_id', 'tool_use_id', 'model'):
                require(record.get(key) is not None and record[key] != '', f'missing hook identity {key}')
            require('instruction_revision' in record, 'instruction revision identity field absent')
            require(record['cwd'] == str(workspace), 'hook cwd mismatch')
        rewrite = [r for r in records if r['tool_use_id'] == 'explicit_0']
        require(len({(r['session_id'], r['turn_id'], r['tool_use_id']) for r in rewrite}) == 1, 'invocation identity changed across hooks')
        require(rewrite[0]['tool_input']['command'].endswith('case-rewrite'), 'original input missing')
        require('rewritten.txt' in rewrite[1]['tool_input']['command'], 'successive pre-hook did not observe updated input')
        require(all('rewritten.txt' in r['tool_input']['command'] for r in rewrite if r['hook_event_name'] == 'PostToolUse'), 'post-hook received stale input')
        require(not any(r['hook_event_name'] != 'PreToolUse' and r['tool_use_id'] in {f'explicit_{i}' for i in range(1, 7)} for r in records), 'post hook ran after pre-hook denial')
        require(not any(r['tool_use_id'] == 'explicit_9' for r in records), 'oversized hook stdin spawned command')
        require(any(r['hook_event_name'] == 'PostToolUseFailure' and r['tool_name'] == 'Read' and r['is_error'] is True and r['error'] for r in records), 'tool failure hook missing')
        require((workspace / 'rewritten.txt').read_text() == 'rewritten', 'rewrite not executed')
        require((workspace / 'post-effect.txt').read_text() == 'effect', 'post error lost effect')
        require((workspace / 'recovered.txt').read_text() == 'x', 'recovery failed')
        prior = len(requests)
        run('replay', explicit)
        require(len(requests) == prior, 'durable replay contacted provider')
        require((workspace / 'hooks.jsonl').read_text() == hook_log, 'durable replay re-executed hooks')
        require((workspace / 'recovered.txt').read_text() == 'x', 'durable replay repeated tool effect')
        phase.update(name='implicit', start=len(requests), steps=[('Bash', {'command': 'printf implicit-optout'}, False, 'implicit-optout')])
        run('implicit', command + ['Exercise opt-out.'])
        require((workspace / 'hooks.jsonl').read_text() == hook_log, 'hooks executed without opt-in')
        prior = len(requests)
        for label, invalid in [('invalid-json', '{'), ('unsupported-event', json.dumps({'hooks': {'Notification': []}})), ('invalid-timeout', json.dumps({'hooks': {'PreToolUse': [{'hooks': [{'type': 'command', 'command': 'touch invalid.txt', 'timeout': 0}]}]}}))]:
            path = artifact / f'{label}.json'
            path.write_text(invalid)
            run(label, command + ['--claude-hooks', str(path), 'Do not start.'], success=False)
            require(len(requests) == prior, 'invalid settings contacted provider')
        time.sleep(0.9)
        for name in ('original', 'denied', 'ask', 'timed', 'overflow', 'malformed', 'exit2', 'timeout-leak', 'overflow-leak', 'implicit-leak', 'matcher-leak', 'input-overflow', 'invalid'):
            require(not (workspace / f'{name}.txt').exists(), f'forbidden effect {name}.txt')
        outcome = {'success': True, 'provider_requests': len(requests), 'hook_invocations': len(records), 'replay_provider_requests': 0, 'replay_hook_invocations': 0, 'denied_effects': 6, 'descendant_leaks': 0, 'implicit_hooks': 0, 'invalid_settings_rejected': 3}
    except Exception as error:
        outcome['error'] = str(error)
        raise
    finally:
        (artifact / 'outcome.json').write_text(json.dumps(outcome, indent=2))
        server.shutdown()
        print(json.dumps({'artifact': str(artifact), **outcome}))


if __name__ == '__main__':
    main()
