#!/usr/bin/env python3
"""Classic CLI PTY computer presentation journey; external inference/CUA are synthetic.

python3 scripts/tests/computer-classic-tui-journey.py --binary target/debug/nanocodex
"""
import argparse
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import pty
import select
import shlex
import shutil
import struct
import subprocess
import sys
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(path))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


fixture = load('computer_fixture', 'computer-harness-cli-journey.py')
screen_fixture = load('screen_fixture', 'claude-scheduler-monitor-cli-journey.py')
require = fixture.require


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--tool-calls', choices=['expanded', 'hidden'], default='expanded')
    parser.add_argument('--output', type=Path, default=ROOT / 'output/computer-classic-tui' / uuid4().hex)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True)
    home, workspace = out / 'home', out / 'workspace'
    home.mkdir()
    workspace.mkdir()
    binary = out / args.binary.name
    shutil.copy2(args.binary.resolve(), binary)
    launcher = out / 'provider'
    launcher.write_text('#!/bin/sh\nexec ' + shlex.join([sys.executable, str(Path(__file__).resolve()), '--mcp', str(out / 'mcp.jsonl')]) + '\n')
    launcher.chmod(0o755)
    requests = []
    errors = []
    # A single real Code Mode cell groups adjacent CUA calls and independently emits output.
    calls = [('js', 'text', 'Inspect page'), ('future__tool', 'image', 'Capture page'),
             ('js', 'error', 'Click unavailable item'), ('js_reset', 'reset', 'Reset session')]
    code = '\n'.join('try { const result = await tools.' + fixture.PREFIX + name + '(' + json.dumps({'code': action, 'title': title, 'opaque': 'RAW_ARGUMENT_MARKER'}) + '); text(result);' + (' text({label:"INDEPENDENT_LABELED_OUTPUT", result});' if action == 'reset' else '') + ' } catch (_) {}'
                     for name, action, title in calls)
    code += '\ntext("INDEPENDENT_CELL_OUTPUT");'

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['content-length'])))
            requests.append(request)
            (out / 'requests.json').write_text(json.dumps(requests, indent=2))
            try:
                require(len(requests) <= 8, 'unexpected inference retry')
                block = {'type': 'tool_use', 'id': 'computer_cell', 'name': 'exec', 'input': {'code': code}} if len(requests) == 1 else {'type': 'text', 'text': 'computer-tui-complete'}
                if len(requests) > 1:
                    import re
                    receipts = [b for m in request['messages'] for b in m.get('content', []) if isinstance(b, dict) and b.get('type') == 'tool_result']
                    cell = re.search(r'Script running with cell ID ([0-9]+)', json.dumps(receipts[-1]))
                    if cell:
                        block = {'type': 'tool_use', 'id': 'wait_' + str(len(requests)), 'name': 'wait', 'input': {'cell_id': cell[1], 'yield_time_ms': 1000}}
                response = screen_fixture.sse(block, request['model'])
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.send_header('Content-Length', str(len(response)))
                self.end_headers()
                self.wfile.write(response)
            except Exception as error:
                errors.append(str(error))

    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    command = [str(binary), '--harness', 'claude', '--model', 'claude-sonnet-5-5', '--claude-api-key', 'synthetic-key', '--claude-messages-url', f'http://127.0.0.1:{server.server_port}/v1/messages', '--cwd', str(workspace), '--rollouts', 'false', '--browser=none', '--mcp-defaults', 'false', '--mcp-codex-config', 'false', '--web-search', 'false', '--image-generation', 'false', '--subagents', 'false', '--memory', 'false', '--tool-calls', args.tool_calls, '--prompt', 'Exercise synthetic computer activity.']
    env = {'HOME': str(home), 'CODEX_HOME': str(home / 'codex'), 'PATH': '/usr/bin:/bin', 'TERM': 'xterm-256color', 'NANOCODEX_COMPUTER': str(launcher)}
    (out / 'scenario.json').write_text(json.dumps({'command': command, 'environment': env, 'calls': calls, 'expected': ['pending provider call hides raw args', 'Used computer 4 actions; 1 failed', 'screenshot marker; first-line failure', 'raw arguments/results only after Ctrl+O', 'independent cell output retained']}, indent=2))
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 65, 170, 0, 0))
    process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, cwd=workspace, env=env, start_new_session=True)
    os.close(slave)
    screen = screen_fixture.TerminalScreen(rows=65)
    transcript = bytearray()
    outcome = {'success': False}

    def wait_for(predicate, label):
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if select.select([master], [], [], .05)[0]:
                data = os.read(master, 65536)
                transcript.extend(data)
                screen.feed(data)
                (out / "latest.screen.txt").write_text(screen.text())
            if predicate(screen.text()):
                (out / (label + '.screen.txt')).write_text(screen.text())
                return screen.text()
            require(process.poll() is None, 'CLI exited before ' + label)
        raise AssertionError('Timed out: ' + label)

    try:
        pending = wait_for(lambda text: (out / 'mcp.jsonl').exists() and 'tools/call' in (out / 'mcp.jsonl').read_text(), 'running')
        require('RAW_ARGUMENT_MARKER' not in pending, 'pending activity leaked raw arguments')
        if args.tool_calls == 'hidden':
            require('Using computer' not in pending, 'hidden mode leaked pending activity')
        (out / 'release-provider').touch()
        compact = wait_for(lambda text: 'computer-tui-complete' in text, 'compact')
        if args.tool_calls == 'expanded':
            for marker in ('Used computer', '4 actions', '1 failed', 'Captured screenshot', 'Capture page', 'Failed:', 'provider-error', '2 more', 'INDEPENDENT_CELL_OUTPUT', 'INDEPENDENT_LABELED_OUTPUT'):
                require(marker in compact, 'compact screen missing ' + marker)
            for marker in ('RAW_ARGUMENT_MARKER', 'RAW_RESULT_MANUAL', 'provider-text'):
                require(marker not in compact, 'raw detail leaked: ' + marker)
            os.write(master, b'\x0f')
            wait_for(lambda text: 'Ctrl+O hide tools' in text, 'folded')
            os.write(master, b'\x0f')
            hidden = wait_for(lambda text: 'Used computer' not in text, 'hidden')
        else:
            hidden = compact
        for marker in ('Used computer', 'RAW_ARGUMENT_MARKER', 'INDEPENDENT_CELL_OUTPUT'):
            require(marker not in hidden, 'hidden mode leaked tool output: ' + marker)
        os.write(master, b'\x0f')
        expanded = wait_for(lambda text: 'RAW_ARGUMENT_MARKER' in text and 'RAW_RESULT_MANUAL' in text, 'expanded')
        require('provider-text' in expanded, 'expanded transcript lost successful result')
        os.write(master, b'\x0f')
        wait_for(lambda text: 'Used computer' in text and 'RAW_ARGUMENT_MARKER' not in text, 'recollapsed')
        require(not errors, '; '.join(errors))
        outcome['success'] = True
    except Exception as error:
        outcome['error'] = str(error)
        raise
    finally:
        process.terminate()
        process.wait(timeout=10)
        os.close(master)
        server.shutdown()
        (out / 'terminal.pty').write_bytes(transcript)
        (out / 'final.screen.txt').write_text(screen.text())
        (out / 'outcome.json').write_text(json.dumps(outcome, indent=2))
        print(json.dumps({'artifact': str(out), **outcome}))


if __name__ == '__main__':
    if len(sys.argv) == 3 and sys.argv[1] == '--mcp':
        # Delay only the external provider to make the pending public UI observable.
        original_stdout = sys.stdout
        class ResultWriter:
            def write(self, text):
                if 'provider-error' in text:
                    text = text.replace('provider-error', 'Script error: provider-error\\nRAW_RESULT_MANUAL')
                if 'provider-text' in text:
                    release = Path(sys.argv[2]).parent / 'release-provider'
                    deadline = time.monotonic() + 20
                    while not release.exists() and time.monotonic() < deadline:
                        time.sleep(.02)
                return original_stdout.write(text)
            def flush(self):
                original_stdout.flush()
        sys.stdout = ResultWriter()
        fixture.mcp(sys.argv[2])
    else:
        main()
