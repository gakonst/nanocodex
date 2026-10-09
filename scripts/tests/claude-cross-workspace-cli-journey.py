#!/usr/bin/env python3
"""Actual mixed CLI worktree journey; only model-provider HTTP/SSE is synthetic."""
import argparse
import importlib.util
import json
from claude_code_fixture import wrap_tool, unwrap_request
from pathlib import Path
import shlex
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('native_fixture', ROOT / 'scripts/tests/claude-native-cli-journey.py')
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
require, sse = fixture.require, fixture.sse
MODELS = {'codex': 'gpt-6.1-sol', 'claude': 'claude-sonnet-5-5'}
LABELS = ['OLD_PIN', 'NEW_CODEX', 'MID_CODEX', 'LEAF_CODEX', 'WORKSPACE_PARENT', 'WORKSPACE_OUTER', 'POLICY_CHILD', 'POLICY_ROOT']


def user_label(request):
    texts = []
    for m in request.get('messages', request.get('input', [])):
        if m.get('role') != 'user':
            continue
        content = m.get('content', '')
        if isinstance(content, str):
            texts.append(content)
        else:
            texts += [b.get('text', '') for b in content if b.get('type') in ('text', 'input_text')]
    joined = '\n'.join(texts)
    return next((label for label in LABELS if label in joined), 'unknown')


def last_result(request, expected_error=False):
    if 'messages' in request:
        for m in reversed(request['messages']):
            for b in reversed(m.get('content', []) if isinstance(m.get('content'), list) else []):
                if b.get('type') == 'tool_result':
                    require(bool(b.get('is_error', False)) == expected_error, f'wrong tool error status: {b}')
                    return fixture.text_of(b)
    else:
        for m in reversed(request['input']):
            if m.get('type') in ('custom_tool_call_output', 'function_call_output'):
                return str(m.get('output'))
    return ''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', default='target/debug/nanocodex', type=Path)
    parser.add_argument('--root-family', default='claude', choices=MODELS)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    artifact = (args.output or ROOT / 'output/cross-workspace-cli' / (args.root_family + '-' + uuid4().hex)).resolve()
    workspace = artifact / 'workspace'
    workspace.mkdir(parents=True)
    (artifact / 'home').mkdir()
    (workspace / 'marker.txt').write_text('original-marker\n')
    for command in (['git', 'init', '-q'], ['git', 'add', 'marker.txt'], ['git', '-c', 'user.name=Synthetic', '-c', 'user.email=synthetic@example.invalid', 'commit', '-qm', 'synthetic baseline']):
        subprocess.run(command, cwd=workspace, check=True, capture_output=True)
    requests, errors, checks = [], [], []
    counts, state = {}, {}
    entered = threading.Event()
    guard = threading.Lock()

    def spawn(family, label, background=False):
        return ('spawn_agent', {'role': label, 'task': label, 'harness': family, 'model': MODELS[family], 'thinking': 'medium', 'output_contract':{'kind':'string'}})

    def child_code(family, label):
        return f'''const child = await tools.spawn_agent({{harness:{json.dumps(family)},model:{json.dumps(MODELS[family])},thinking:"medium",role:{json.dumps(label)},task:{json.dumps(label)},output_contract:{{kind:"string"}}}}); const done = await tools.wait_agent({{agent_ids:[child.agent_id],timeout_ms:30000}}); text(done); if (done.timed_out || done.agents[0].status.state !== "completed") throw Error("child did not complete");'''

    def action(family, label, stage, request):
        prior = last_result(request, (label == 'WORKSPACE_PARENT' and stage == 7) or (label == 'POLICY_CHILD' and stage in (1, 2))) if stage else ''
        if label == 'POLICY_ROOT':
            if stage == 0:
                return ('exec', {'code': child_code('claude', 'POLICY_CHILD')})
            return ('text', 'cross-workspace-policy-complete')
        if label == 'POLICY_CHILD':
            if stage == 0:
                return ('Write', {'file_path': 'forbidden.txt', 'content': 'must-not-write'})
            if stage == 2:
                require('cross-family delegation is unavailable' in prior, f'restricted family was not guarded: {prior}')
            if stage == 1:
                return spawn('codex', 'MUST_NOT_LAUNCH_CODEX')
            if stage == 2 and args.root_family != 'claude':
                return ('submit_result', {'output': 'policy-child-complete'})
            return ('text', 'cross-workspace-policy-complete')
        if label == 'WORKSPACE_OUTER':
            if stage == 0:
                return ('exec', {'code': child_code('claude', 'WORKSPACE_PARENT')})
            return ('text', 'cross-workspace-journey-complete')
        if label == 'WORKSPACE_PARENT':
            if stage == 0:
                return spawn('codex', 'OLD_PIN', True)
            if stage == 1:
                state['old_id'] = json.loads(prior)['agent_id']
                return ('EnterWorktree', {'name': 'mixed-propagation'})
            if stage == 2:
                state['enter_receipt'] = json.loads(prior)
                return ('exec_command', {'cmd': 'printf entered-marker > marker.txt; pwd'})
            if stage == 3:
                candidates = list((workspace / '.claude/worktrees').iterdir())
                state['entered_workspace'] = str(next(p for p in candidates if p.is_dir()))
                require(state['entered_workspace'] in prior, f'parent exec_command did not switch: {prior}')
                entered.set()
                return spawn('codex', 'NEW_CODEX')
            if stage == 4:
                return ('wait_agent', {'agent_ids':[json.loads(prior)['agent_id']], 'timeout_ms':30000})
            if stage == 5:
                require('completed' in prior, f'new Codex did not complete: {prior}')
                return ('wait_agent', {'agent_ids': [state['old_id']], 'timeout_ms': 30000})
            if stage == 6:
                require('completed' in prior, f'old child did not complete: {prior}')
                return ('ExitWorktree', {'cleanup': True})
            if stage == 7:
                require('active background or child contexts' in prior, f'child workspace lease missing: {prior}')
                state['cleanup_lease_guarded'] = True
                return ('ExitWorktree', {})
            if stage == 8:
                return ('exec_command', {'cmd': 'pwd; cat marker.txt'})
            if stage == 9:
                require('original-marker' in prior, f'exit did not restore parent: {prior}')
                if args.root_family != 'claude':
                    return ('submit_result', {'output': 'parent-journey-complete'})
                return ('text', 'cross-workspace-journey-complete')
            return ('text', 'parent-journey-complete')
        if stage == 0:
            if label == 'OLD_PIN':
                require(entered.wait(30), 'parent did not enter worktree before old child ran')
            command = f'pwd > {label}.txt; cat marker.txt >> {label}.txt; cat {label}.txt'
            if family == 'claude':
                return ('exec_command', {'cmd': command})
            return ('exec', {'code': 'text(await tools.exec_command(' + json.dumps({'cmd': command, 'max_output_tokens': 1000}) + '));'})
        if stage == 1:
            expected = str(workspace) if label == 'OLD_PIN' else state['entered_workspace']
            marker = 'original-marker' if label == 'OLD_PIN' else 'entered-marker'
            require(expected in prior and marker in prior, f'{family}/{label} workspace mismatch: {prior}')
            checks.append({'family': family, 'label': label, 'expected_workspace': expected, 'observed_receipt': prior})
            target = {'NEW_CODEX': ('codex', 'MID_CODEX'), 'MID_CODEX': ('claude', 'LEAF_CODEX')}.get(label)
            if target:
                return ('exec', {'code': child_code(*target)})
        submit_stage = 2 if label in ('NEW_CODEX', 'MID_CODEX') else 1
        if stage == submit_stage:
            if family == 'claude':
                return ('submit_result', {'output': label + '-complete'})
            return ('exec', {'code': 'text(await tools.submit_result({output:' + json.dumps(label + '-complete') + '}));'})
        return ('text', label + '-complete')

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass
        def do_POST(self):
            request = unwrap_request(json.loads(self.rfile.read(int(self.headers['content-length']))))
            family = 'claude' if self.path.endswith('/messages') else 'codex'
            label = user_label(request)
            with guard:
                stage = counts.get(label, 0)
                counts[label] = stage + 1
                row = {'family': family, 'label': label, 'stage': stage, 'request': request}
                requests.append(row)
            try:
                require(label != 'unknown', 'unrecognized synthetic user prompt')
                name, inputs = action(family, label, stage, request)
            except Exception as error:
                errors.append(str(error))
                entered.set()
                name, inputs = 'text', 'fixture-assertion-failed'
            row['reply'] = {'name': name, 'input': inputs}
            with guard:
                (artifact / 'provider.json').write_text(json.dumps(requests, indent=2))
                (artifact / 'checks.json').write_text(json.dumps(checks, indent=2))
            ident = uuid4().hex
            if family == 'claude':
                block = {'type': 'text', 'text': inputs} if name == 'text' else {'type': 'tool_use', 'id': ident, 'name': name, 'input': inputs}
                response = sse(wrap_tool(block), request['model'])
            else:
                if name == 'text':
                    output = {'type': 'message', 'role': 'assistant', 'content': [{'type': 'output_text', 'text': inputs}]}
                else:
                    output = {'type': 'custom_tool_call', 'name': name, 'call_id': ident, 'input': inputs['code']}
                response = ('data: ' + json.dumps({'type': 'response.completed', 'response': {'id': ident, 'status': 'completed', 'output': [output], 'usage': {'input_tokens': 1, 'input_tokens_details': {'cached_tokens': 0}, 'output_tokens': 1, 'output_tokens_details': {'reasoning_tokens': 0}, 'total_tokens': 2}}}) + '\n\n').encode()
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(response)))
            self.end_headers()
            self.wfile.write(response)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f'http://127.0.0.1:{server.server_port}'
    command = [str(args.binary.resolve()), 'run', '--harness', args.root_family, '--model', MODELS[args.root_family], '--thinking', 'medium', '--claude-api-key', 'synthetic-claude-key', '--claude-messages-url', base + '/v1/messages', '--api-key', 'synthetic-codex-key', '--api-base-url', base + '/v1', '--responses-transport', 'https', '--cwd', str(workspace), '--rollouts', 'false', '--browser=none', '--mcp-defaults', 'false', '--mcp-codex-config', 'false', '--web-search', 'false', '--image-generation', 'false', '--subagents', 'true', '--max-subagents', '12', '--memory', 'false', 'WORKSPACE_PARENT' if args.root_family == 'claude' else 'WORKSPACE_OUTER']
    environment = {'HOME': str(artifact / 'home'), 'CODEX_HOME': str(artifact / 'codex-home'), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'NANOCODEX_COMPUTER': 'off'}
    (artifact / 'scenario.json').write_text(json.dumps({'command': command, 'shell_command': shlex.join(command), 'environment': environment, 'expected': 'old child pinned; new Codex + same-family grandchildren + Claude descendants use entered worktree; parent exit restores original', 'boundary': 'real CLI + loopback HTTP SSE models + real Git and shell'}, indent=2))
    outcome = {'success': False}
    try:
        result = subprocess.run(command, cwd=workspace, env=environment, capture_output=True, timeout=150)
        (artifact / 'stdout.jsonl').write_bytes(result.stdout)
        (artifact / 'stderr.log').write_bytes(result.stderr)
        require(result.returncode == 0, f'exit {result.returncode}: {result.stderr.decode(errors="replace")}')
        require(not errors, '; '.join(errors))
        require(b'cross-workspace-journey-complete' in result.stdout, 'missing final answer')
        require(len(checks) == 4, f'expected four pinned-workspace observations, got {len(checks)}')
        tree = Path(state['entered_workspace'])
        require((workspace / 'OLD_PIN.txt').read_text() == str(workspace) + '\noriginal-marker\n', 'old child file changed or wrong directory')
        require(not (tree / 'OLD_PIN.txt').exists(), 'old child leaked into new tree')
        for label in LABELS[1:4]:
            require((tree / (label + '.txt')).read_text() == str(tree) + '\nentered-marker', f'{label} actual effect wrong')
            require(not (workspace / (label + '.txt')).exists(), f'{label} leaked effect into original')
        require((workspace / 'marker.txt').read_text() == 'original-marker\n', 'parent transition mutated original marker')
        require(state.get('cleanup_lease_guarded'), 'cleanup lease guard not observed')
        policy = artifact / 'permissions.json'
        policy.write_text(json.dumps({'permissions': {'defaultMode': 'full-access', 'deny': ['Write']}}))
        policy_command = command[:-1] + ['--claude-permissions', str(policy), 'POLICY_CHILD' if args.root_family == 'claude' else 'POLICY_ROOT']
        (artifact / 'policy-scenario.json').write_text(json.dumps({'command': policy_command, 'shell_command': shlex.join(policy_command), 'environment': environment, 'expected': 'Claude permissions file enforced even under Codex roots; restricted Claude parent cannot spawn unrestricted Codex'}, indent=2))
        result = subprocess.run(policy_command, cwd=workspace, env=environment, capture_output=True, timeout=90)
        (artifact / 'policy-stdout.jsonl').write_bytes(result.stdout)
        (artifact / 'policy-stderr.log').write_bytes(result.stderr)
        require(result.returncode == 0, f'policy exit {result.returncode}: {result.stderr.decode(errors="replace")}')
        require(not errors, '; '.join(errors))
        require(b'cross-workspace-policy-complete' in result.stdout, 'policy final answer absent')
        require(not (workspace / 'forbidden.txt').exists(), 'child ignored explicit root permissions file')
        outcome.update(success=True, root_family=args.root_family, workspace=str(workspace), entered_workspace=str(tree), checks=len(checks), provider_requests=len(requests), old_child_pinned=True, mixed_descendants_current=True, cleanup_child_lease_guarded=True, root_permissions_reach_claude_child=True, restricted_cross_family_denied=True)
    except Exception as error:
        outcome['error'] = str(error)
        raise
    finally:
        entered.set()
        server.shutdown()
        (artifact / 'outcome.json').write_text(json.dumps(outcome, indent=2))
        print(json.dumps({'artifact': str(artifact), **outcome}))

if __name__ == '__main__':
    main()
