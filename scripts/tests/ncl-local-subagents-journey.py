#!/usr/bin/env python3
"""Unified local TUI (ncl) native subagent journey: /agents limit, child status, parent continuation.

Real ncl over a PTY with the real Code Mode exec runtime and shared child-agent
registry; only the Claude Messages HTTP provider is synthetic. The journey
lowers the live concurrency limit to 1 through the /agents tree, then the
parent's exec spawns child A and attempts child B: the public spawn_agent
boundary rejects B while A is active. A completes after the parent turn ended
and wakes the idle parent exactly once; that continuation spawns child B (now
admitted), whose completion wakes the parent once more. The provider proves the
two children's inference never overlapped and the tree shows both completed.
"""
from claude_code_fixture import normalize_request, wrap_tool
import argparse, importlib.util, json, os, re, threading, time
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

spec = importlib.util.spec_from_file_location('features', Path(__file__).with_name('ncl-local-features-journey.py'))
features = importlib.util.module_from_spec(spec); spec.loader.exec_module(features)
Session, require, sse = features.Session, features.require, features.sse

PROMPT = 'delegate two journey children'
SPAWN = 'tools.spawn_agent({role:"journey child %s",task:"CHILD_TASK_%s",output_contract:{kind:"string"}})'
SPAWN_A_CODE = ('const a = await ' + SPAWN % ('A', 'A') + ';\n'
                'let limit = "ADMITTED";\n'
                'try { await ' + SPAWN % ('B', 'B') + '; } catch (error) { limit = "REJECTED " + String(error && error.message || error); }\n'
                'text("SPAWNED_A " + JSON.stringify(a) + " LIMIT " + limit);')
SPAWN_B_CODE = 'const b = await ' + SPAWN % ('B', 'B') + ';\ntext("SPAWNED_B " + JSON.stringify(b));'
LIMIT_ERROR = 'sub-agent concurrency limit of 1 has been reached'
# Children outlive the parent's follow-up inference, so each completes while the parent is idle.
HOLD_SECONDS = 3


def text_of(message):
    content = message.get('content')
    return content if isinstance(content, str) else json.dumps(content)


def agent_id(text, marker):
    match = re.search(marker + r' .*?\\?"agent_id\\?":\s*\\?"?(\d+)', text)
    require(match, f'{marker} result has no agent_id: {text[:400]}')
    return match.group(1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, default=Path('output/ncl-local-subagents') / uuid4().hex)
    args = parser.parse_args(); artifact = args.output.resolve(); artifact.mkdir(parents=True)
    workspace = artifact / 'workspace'; workspace.mkdir()
    home = artifact / 'home'; (home / 'codex').mkdir(parents=True)
    env = {'HOME': str(home), 'CODEX_HOME': str(home / 'codex'), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'TERM': 'xterm-256color', 'NANOCODEX_COMPUTER': 'off'}
    lock = threading.Lock()
    requests, errors, child_spans = [], [], []
    state = {'in_flight': 0, 'max_in_flight': 0, 'ids': {}, 'limit_result': None}

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_POST(self):
            body = normalize_request(json.loads(self.rfile.read(int(self.headers['content-length']))), artifact)
            system = json.dumps(body.get('system', ''))
            messages = body['messages']; last = text_of(messages[-1]) if messages else ''
            child = next((name for name in ('A', 'B') if f'CHILD_TASK_{name}' in json.dumps(messages[:1])), None) \
                if 'Act as a specialist subagent' in system + json.dumps(messages[:1]) else None
            started = time.monotonic(); completion = None
            try:
                if child:
                    with lock:
                        state['in_flight'] += 1; state['max_in_flight'] = max(state['max_in_flight'], state['in_flight'])
                    try:
                        if 'tool_result' in last:
                            block = {'type': 'text', 'text': f'child {child} submitted'}
                        else:
                            time.sleep(HOLD_SECONDS)
                            block = {'type': 'tool_use', 'id': f'submit-{child}', 'name': 'submit_result', 'input': {'output': f'CHILD_RESULT_{child}'}}
                    finally:
                        with lock: state['in_flight'] -= 1
                    child_spans.append({'child': child, 'start': started, 'end': time.monotonic(), 'final': 'tool_result' in last})
                    role = f'child-{child}'
                elif (completion := re.search(r'<subagent_completion agent_id=\\?"(\d+)\\?"', last)):
                    completion = completion.group(1)
                    if completion == state['ids'].get('A') and 'B' not in state['ids']:
                        # The woken parent delegates the previously rejected work now that capacity is free.
                        block = {'type': 'tool_use', 'id': 'spawn-b', 'name': 'exec', 'input': {'code': SPAWN_B_CODE}}
                    else:
                        block = {'type': 'text', 'text': f'PARENT_INTEGRATED_{completion}'}
                    role = 'continuation'
                elif 'SPAWNED_B' in last:
                    state['ids']['B'] = agent_id(last, 'SPAWNED_B')
                    block = {'type': 'text', 'text': f'PARENT_INTEGRATED_{state["ids"]["A"]}'}; role = 'continuation-final'
                elif 'tool_result' in last:
                    require('SPAWNED_A' in last, f'spawn exec result missing from parent request: {last[:400]}')
                    state['ids']['A'] = agent_id(last, 'SPAWNED_A')
                    state['limit_result'] = re.split(r'\\?"', last.split(' LIMIT ', 1)[1])[0] if ' LIMIT ' in last else None
                    block = {'type': 'text', 'text': 'PARENT_TURN_DONE'}; role = 'parent-final'
                else:
                    require(PROMPT in last, 'unexpected parent request')
                    block = {'type': 'tool_use', 'id': 'spawn-two', 'name': 'exec', 'input': {'code': SPAWN_A_CODE}}; role = 'parent-spawn'
            except Exception as error:
                errors.append(str(error)); block = {'type': 'text', 'text': 'fixture-error'}; role = 'error'
            with lock:
                requests.append({'role': role, 'completion': completion, 'body': body})
                (artifact / 'provider.json').write_text(json.dumps(requests, indent=2))
            response = sse(wrap_tool(block), body['model'])
            self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.send_header('Content-Length', str(len(response))); self.end_headers(); self.wfile.write(response)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider); threading.Thread(target=server.serve_forever, daemon=True).start()
    command = [str(args.binary.resolve()), '--claude', '--model', 'claude-sonnet-5-5', '--claude-api-key', 'synthetic-key',
               '--claude-messages-url', f'http://127.0.0.1:{server.server_port}/v1/messages', '--cwd', str(workspace), '--browser=none',
               '--mcp-defaults', 'false', '--mcp-codex-config', 'false', '--web-search', 'false', '--image-generation', 'false',
               '--subagents', 'true', '--memory', 'false']
    checks = []; outcome = {'success': False}; session = None
    roles = lambda role: [r for r in requests if r['role'] == role]
    concurrency = lambda active: re.search(rf'Concurrency:?\s+{active} / 1 active', session.screen.text())
    try:
        session = Session('ncl', command, workspace, env, artifact)
        # The unified composer shows the selected model in its border once the local agent is built.
        session.wait(lambda: 'claude-sonnet-5-5' in session.screen.text() and '0%/' in session.screen.text(), 'composer absent', 60); time.sleep(1.5)
        session.enter('/agents'); session.wait(lambda: session.shown('Sub-agent tree'), '/agents did not open the subagent tree')
        session.wait(lambda: session.shown('Concurrency: 0 / unlimited active'), 'tree did not report the default unlimited limit')
        os.write(session.master, b'-')
        session.wait(lambda: session.shown('Concurrency: 0 / 1 active'), 'tree did not show the lowered limit')
        # Wait for the composer to return before typing: the limit notice can cover the tree's title,
        # and prompt bytes that arrive with the bare Escape are read as Alt+key inside the tree.
        os.write(session.master, b'\x1b')
        session.wait(lambda: session.composer_visible() and 'enter inspect' not in session.screen.text(), 'tree did not close', 10)
        session.wait(lambda: session.shown('Subagent limit set to 1'), 'limit notice absent', 10)
        checks.append('/agents opens the live subagent tree; "-" lowers the native child concurrency limit to 1 with a visible notice')

        session.enter(PROMPT)
        session.wait(lambda: session.shown('PARENT_TURN_DONE'), 'parent turn did not finish after spawning', 40)
        require(not errors, f'fixture errors: {errors}')
        require(state['limit_result'] and state['limit_result'].startswith('REJECTED') and LIMIT_ERROR in state['limit_result'],
                f'second spawn was not rejected at limit 1: {state["limit_result"]}')
        session.wait(lambda: session.shown(LIMIT_ERROR), 'rejected spawn not rendered in the transcript', 10)
        checks.append('limit 1: the parent exec spawns child A; the public spawn_agent call for child B is rejected with the concurrency-limit error, shown in the transcript, and the parent turn ends')

        a = state['ids']['A']
        session.wait(lambda: 'B' in state['ids'], 'child A completion did not wake the parent to spawn child B', 40)
        b = state['ids']['B']
        session.wait(lambda: session.shown(f'PARENT_INTEGRATED_{a}') and session.shown(f'PARENT_INTEGRATED_{b}'), 'continuation replies not rendered', 40)
        session.wait(lambda: session.shown(f'<subagent_completion agent_id="{a}" />') and session.shown(f'<subagent_completion agent_id="{b}" />'),
                     'completion prompts not shown', 10)
        require(not errors, f'fixture errors: {errors}')
        time.sleep(2)  # observe any duplicate continuation before counting
        continued = [r['completion'] for r in roles('continuation')]
        require(sorted(continued) == sorted([a, b]), f'each direct child must continue the idle parent exactly once: {continued} vs {[a, b]}')
        require(len(roles('parent-spawn')) == 1 and len(roles('parent-final')) == 1 and len(roles('continuation-final')) == 1, 'a parent turn repeated')
        checks.append(f'child {a} completes after the parent turn ended and continues the idle parent once; that continuation spawns child {b} (now admitted), whose completion continues the parent once more')

        first = {c: min(s['start'] for s in child_spans if s['child'] == c) for c in 'AB'}
        last = {c: max(s['end'] for s in child_spans if s['child'] == c) for c in 'AB'}
        require(state['max_in_flight'] == 1 and first['B'] >= last['A'], f'children overlapped under limit 1: {state} {child_spans}')
        checks.append('under limit 1 the two children inference never overlapped at the provider')

        session.enter('/agents'); session.wait(lambda: session.shown('Sub-agent tree'), 'tree did not reopen')
        os.write(session.master, b'f')
        session.wait(lambda: session.shown('journey child A') and session.shown('journey child B'), 'child roles absent from tree')
        session.wait(lambda: session.shown('completed'), 'completed child status absent')
        session.wait(lambda: concurrency(0), 'tree did not keep limit 1 with no active children')
        checks.append('/agents tree (filter all) shows both delegated children as completed under the retained limit 1')
        os.write(session.master, b'\x1b')
        session.wait(lambda: session.composer_visible() and 'enter inspect' not in session.screen.text(), 'tree did not close before quitting', 10)
        session.quit()
        outcome = {'success': True, 'checks': checks, 'agent_ids': state['ids'], 'limit_result': state['limit_result'],
                   'provider_requests': [r['role'] for r in requests], 'max_child_in_flight': state['max_in_flight'], 'child_spans': child_spans}
    except Exception as error:
        outcome['error'] = str(error); outcome['fixture_errors'] = errors
        raise
    finally:
        if session: session.close()
        (artifact / 'scenario.json').write_text(json.dumps({'command': command, 'environment': env, 'expected': [
            '/agents shows unlimited, "-" sets limit 1 with notice', 'parent exec spawns A; spawn of B rejected at limit 1; parent turn ends',
            'A completion continues idle parent once; continuation spawns B', 'B completion continues parent once',
            'children never overlap at the provider', 'tree shows both children completed with limit 1'],
            'boundary': 'actual ncl TUI via PTY, real Code Mode and child registry; synthetic Messages HTTP only'}, indent=2))
        (artifact / 'outcome.json').write_text(json.dumps(outcome, indent=2)); server.shutdown(); print(json.dumps({'artifact': str(artifact), **outcome}))

if __name__ == '__main__': main()
