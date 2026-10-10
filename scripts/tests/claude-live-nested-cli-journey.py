#!/usr/bin/env python3
"""Shipped CLI journey: Claude Code Mode nested tools are published live.

A nested exec_command blocks until this journey observes its live tool.call
event and the yielded parent exec result. Without live publication the start
is only reported after completion, so the nested command can never be
released and the journey fails. The cell then finishes through wait; every
nested call must have exactly one start and one result, with parent identity,
and the persisted checkpoint must keep a bounded, output-free summary.

  python3 scripts/tests/claude-live-nested-cli-journey.py --binary target/debug/nanocodex

Only Messages inference is synthetic; QuickJS, native tools and SQLite are real.
Artifacts (commands, provider requests, JSONL events, timing) go to output/.
"""
import argparse
import hashlib
import importlib.util
import json
import re
import sqlite3
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from uuid import uuid4

spec = importlib.util.spec_from_file_location('native', Path(__file__).with_name('claude-native-cli-journey.py'))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
require, sse, text_of = helper.require, helper.sse, helper.text_of

EXEC_ID, WAIT_ID = 'live_exec', 'live_wait'
CELL = '''// @exec: {"yield_time_ms": 400}
const fast = await tools.exec_command({cmd: "printf LIVE_FAST", yield_time_ms: 5000});
let failure = "none";
try { await tools.exec_command({}); } catch (error) { failure = "caught"; }
const slow = await tools.exec_command({cmd: "i=0; while [ ! -f live-release.txt ]; do i=$((i+1)); [ $i -gt 400 ] && exit 9; sleep 0.05; done; printf LIVE_RELEASED", yield_time_ms: 30000});
text(["LIVE_SUMMARY", fast.output, failure, slow.exit_code, slow.output].join(" "));'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', type=Path, default=Path('output/claude-live-nested-cli') / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    binary_sha256 = hashlib.sha256(binary.read_bytes()).hexdigest()
    workspace = artifact / 'workspace'
    workspace.mkdir(parents=True)
    (artifact / 'home').mkdir()
    requests, errors, cell = [], [], {}

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['content-length'])))
            requests.append(request)
            stage = len(requests) - 1
            try:
                require(self.headers.get('x-api-key') == 'synthetic-live-key', 'authentication mismatch')
                results = {b.get('tool_use_id'): b for m in request['messages'] for b in m.get('content', [])
                           if isinstance(b, dict) and b.get('type') == 'tool_result'}
                if stage == 0:
                    block = {'type': 'tool_use', 'id': EXEC_ID, 'name': 'exec', 'input': {'code': CELL}}
                elif stage == 1:
                    match = re.search(r'Script running with cell ID (\S+)', text_of(results[EXEC_ID]))
                    require(match, f'exec did not yield while the nested call was blocked: {results[EXEC_ID]}')
                    cell['id'] = match.group(1)
                    block = {'type': 'tool_use', 'id': WAIT_ID, 'name': 'wait', 'input': {'cell_id': cell['id'], 'yield_time_ms': 30000}}
                elif stage == 2:
                    receipt = text_of(results[WAIT_ID])
                    require('LIVE_SUMMARY LIVE_FAST caught 0 LIVE_RELEASED' in receipt, f'unexpected wait receipt: {receipt}')
                    block = {'type': 'text', 'text': 'live-nested-complete'}
                else:
                    raise AssertionError(f'unexpected request {stage}')
            except Exception as error:
                errors.append(str(error))
                block = {'type': 'text', 'text': 'fixture-failed'}
            (artifact / 'provider.json').write_text(json.dumps(requests, indent=2))
            response = sse(block, request['model'])
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(response)))
            self.end_headers()
            self.wfile.write(response)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    environment = {'HOME': str(artifact / 'home'), 'CODEX_HOME': str(artifact / 'home/codex'), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'NANOCODEX_COMPUTER': 'off'}
    database = artifact / 'session.sqlite'
    command = [str(binary), '--local', 'run', '--claude', '--model', 'claude-sonnet-5-5', '--claude-api-key', 'synthetic-live-key',
               '--claude-messages-url', f'http://127.0.0.1:{server.server_port}/v1/messages', '--cwd', str(workspace),
               '--rollouts', 'false', '--browser=none', '--mcp-defaults', 'false', '--mcp-codex-config', 'false',
               '--web-search', 'false', '--image-generation', 'false', '--subagents', 'false', '--memory', 'false',
               '--local-durability', str(database), '--local-durability-state-id', 'live-session',
               'Live nested Code Mode journey']
    events, timeline = [], []
    outcome = {'success': False}
    stderr_log = open(artifact / 'stderr.log', 'wb')
    process = subprocess.Popen(command, cwd=workspace, env=environment, stdout=subprocess.PIPE, stderr=stderr_log)

    def read():
        for line in process.stdout:
            try:
                event = json.loads(line)
            except ValueError:
                continue
            events.append((time.monotonic(), event))

    reader = threading.Thread(target=read, daemon=True)
    reader.start()

    def find(kind, predicate):
        for index, (_, event) in enumerate(events):
            if event.get('type') == kind and predicate(event.get('payload', {})):
                return index
        return None

    def slow(payload):
        # Only the nested exec_command, never the parent exec whose source names it.
        return (payload.get('call_id', '').startswith(EXEC_ID + '/code-')
                and 'live-release.txt' in json.dumps(payload.get('arguments', '')))

    try:
        deadline = time.monotonic() + 15
        start = parent = None
        while time.monotonic() < deadline and (start is None or parent is None):
            require(not errors, '; '.join(errors))
            require(process.poll() is None, 'CLI exited before the nested call was released')
            start = find('tool.call', slow)
            parent = find('tool.result', lambda p: p.get('call_id') == EXEC_ID)
            time.sleep(0.02)
        require(start is not None, 'blocked nested exec_command was never announced live (start only after completion)')
        require(parent is not None, 'parent exec did not yield its result while the nested call was blocked')
        require(start < parent, 'nested start must precede the yielded parent result')
        require(find('tool.result', slow) is None, 'blocked nested call reported a result before release')
        timeline.append({'observed_live_start_and_yield': time.time()})
        (workspace / 'live-release.txt').write_text('released by the journey after observing the live start')
        process.wait(timeout=60)
        reader.join(10)
        stderr_log.flush()
        stderr = (artifact / 'stderr.log').read_bytes()
        (artifact / 'events.jsonl').write_text(''.join(json.dumps(e) + '\n' for _, e in events))
        require(process.returncode == 0, f'exit {process.returncode}: {stderr.decode(errors="replace")}')
        require(not errors, '; '.join(errors))
        require(any(e.get('type') == 'assistant.message' and 'live-nested-complete' in json.dumps(e) for _, e in events), 'final answer absent')

        nested = {}
        for index, (_, event) in enumerate(events):
            payload = event.get('payload', {})
            call_id = payload.get('call_id', '')
            if event.get('type') in ('tool.call', 'tool.result') and call_id.startswith(EXEC_ID + '/code-'):
                nested.setdefault(call_id, []).append((index, event['type'], payload))
        require(len(nested) == 3, f'expected fast, failing and slow nested calls: {sorted(nested)}')
        for call_id, records in nested.items():
            kinds = [kind for _, kind, _ in records]
            require(kinds == ['tool.call', 'tool.result'], f'{call_id} must have exactly one start then one result: {kinds}')
            require(all(p.get('parent_call_id') == EXEC_ID for _, _, p in records), f'{call_id} lost its parent exec identity')
        statuses = {call_id: records[1][2].get('status') for call_id, records in nested.items()}
        require(sorted(statuses.values()) == ['completed', 'completed', 'failed'], f'nested outcomes lost: {statuses}')
        slow_id = next(c for c, r in nested.items() if slow(r[0][2]))
        slow_result = nested[slow_id][1][0]
        wait_result = find('tool.result', lambda p: p.get('call_id') == WAIT_ID)
        require(parent < slow_result < wait_result, 'slow nested result must follow the yield and precede the wait result')
        require('LIVE_RELEASED' in json.dumps(nested[slow_id][1][2]), 'slow nested result content missing')

        connection = sqlite3.connect(database)
        # The durable checkpoint is split across the state row and its records.
        rows = [row[0] for row in connection.execute('SELECT payload FROM nanocodex_durable_states')]
        rows += [row[0] for row in connection.execute('SELECT value FROM nanocodex_durable_records')]
        connection.close()

        def summaries(value):
            if isinstance(value, dict):
                if 'code_calls' in value:
                    yield value['code_calls']
                for item in value.values():
                    yield from summaries(item)
            elif isinstance(value, list):
                for item in value:
                    yield from summaries(item)
            elif isinstance(value, str) and value.lstrip('=')[:1] in '{[':
                try:
                    # Durable record values carry a one-character encoding tag.
                    yield from summaries(json.loads(value.lstrip('=')))
                except ValueError:
                    pass

        rows = [row.decode(errors='replace') if isinstance(row, bytes) else row for row in rows]
        persisted = [s for row in rows for s in summaries(row)]
        require(persisted, 'checkpoint lacks the nested call summary')
        latest = max(persisted, key=lambda rounds: sum(len(round_['calls']) for round_ in rounds))
        (artifact / 'code_calls.json').write_text(json.dumps(latest, indent=2))
        retained = {call['call_id']: call for round_ in latest for call in round_['calls']}
        require(set(retained) == set(nested), f'persisted summary calls differ: {sorted(retained)}')
        require({c['status'] for c in retained.values()} == {'completed', 'failed'}, 'persisted statuses lost')
        require(not any(key in call for call in retained.values() for key in ('output', 'structured_result', 'result')), 'summary must not retain outputs')
        # Inputs may name markers; output envelope fields must never appear.
        require(not any(field in json.dumps(latest) for field in ('chunk_id', 'exit_code', 'wall_time_seconds')), 'summary leaked nested output')
        require(hashlib.sha256(binary.read_bytes()).hexdigest() == binary_sha256, 'binary changed during journey; rerun after build completes')
        outcome = {'success': True, 'nested_calls': statuses, 'event_count': len(events), 'provider_requests': len(requests), 'binary_sha256': binary_sha256}
    except Exception as error:
        outcome['error'] = str(error)
        raise
    finally:
        if process.poll() is None:
            (workspace / 'live-release.txt').write_text('release fixture process after failure')
            process.kill()
            process.wait()
        stderr_log.close()
        (artifact / 'events.jsonl').write_text(''.join(json.dumps(e) + '\n' for _, e in events))
        (artifact / 'scenario.json').write_text(json.dumps({'command': command, 'environment': environment, 'cell': CELL, 'timeline': timeline,
            'expected': ['nested start published live before the yielded parent result while the nested command is blocked', 'release only after that observation; wait reports the slow result', 'exactly one start and one result per nested call with parent_call_id', 'failed nested call keeps failed status', 'checkpoint keeps an output-free nested summary'],
            'boundary': 'shipped CLI run; real QuickJS, native exec_command and SQLite; synthetic Messages SSE inference only'}, indent=2))
        (artifact / 'outcome.json').write_text(json.dumps(outcome, indent=2))
        server.shutdown()
        print(json.dumps({'artifact': str(artifact), **outcome}))


if __name__ == '__main__':
    main()
