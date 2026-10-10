#!/usr/bin/env python3
"""Real /model selection before the first prompt; only inference is synthetic.

python3 scripts/tests/claude-model-picker-cli-journey.py --binary target/debug/nanocodex
Evidence: output/claude-model-picker-cli/<uuid>/ (commands, wire requests, PTY, screens).
"""
import argparse
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

spec = importlib.util.spec_from_file_location('screen', Path(__file__).with_name('claude-scheduler-monitor-cli-journey.py'))
h = importlib.util.module_from_spec(spec)
COLUMNS = 240
spec.loader.exec_module(h)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', type=Path, default=Path('output/claude-model-picker-cli') / uuid4().hex)
    args = parser.parse_args()
    artifact = args.output.resolve()
    artifact.mkdir(parents=True)
    requests, errors, checks = [], [], []

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            family = 'claude' if self.path == '/v1/messages' else 'codex'
            try:
                if self.path == "/v1/messages" and {t["name"] for t in request.get("tools", [])} != {"exec", "wait"}:
                    raise AssertionError("Claude catalog must expose exactly exec/wait")
                h.require(self.path in ('/v1/messages', '/v1/responses'), 'unexpected route ' + self.path)
                if family == 'claude':
                    h.require(self.headers.get('x-api-key') == 'synthetic-claude-key', 'wrong Claude auth')
                else:
                    h.require(self.headers.get('authorization') == 'Bearer synthetic-codex-key', 'wrong Codex auth')
                requests.append({'family': family, 'request': request})
                answer = 'model-selection-reply-' + str(len(requests))
                if family == 'claude':
                    response = h.sse({'type': 'text', 'text': answer}, request['model'])
                else:
                    response = ('data: ' + json.dumps({'type': 'response.completed', 'response': {
                        'id': uuid4().hex, 'status': 'completed',
                        'output': [{'type': 'message', 'role': 'assistant', 'content': [{'type': 'output_text', 'text': answer}]}],
                        'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2}
                    }}) + '\n\n').encode()
            except Exception as error:
                errors.append(str(error))
                response = b''
            (artifact / 'provider.json').write_text(json.dumps(requests, indent=2))
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(response)))
            self.end_headers()
            self.wfile.write(response)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f'http://127.0.0.1:{server.server_port}/v1'

    def journey(name, initial, target, expected, codex_auth=True, claude_auth=True, picker=False, fail_first=False, queued=False, queued_failure=False):
        out = artifact / name
        out.mkdir()
        home = out / 'home'
        home.mkdir()
        environment = {'HOME': str(home), 'CODEX_HOME': str(home / 'codex'), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'TERM': 'xterm-256color', 'NANOCODEX_COMPUTER': 'off'}
        command = [str(args.binary.resolve()), '--cwd', str(out), '--api-base-url', base,
                   '--claude-messages-url', base + '/messages', '--responses-transport', 'https',
                   '--websocket-warmup', 'false', '--store-responses', 'false', '--rollouts', 'false',
                   '--browser=none', '--mcp-defaults', 'false', '--mcp-codex-config', 'false',
                   '--web-search', 'false', '--image-generation', 'false', '--subagents', 'false', '--memory', 'false']
        if initial == 'claude':
            command += ['--claude', '--model', 'sonnet']
        if codex_auth:
            command += ['--api-key', 'synthetic-codex-key']
        if claude_auth:
            command += ['--claude-api-key', 'synthetic-claude-key']
        (out / 'scenario.json').write_text(json.dumps({'command': command, 'environment': environment, 'expected_model': expected}, indent=2))
        master, slave = pty.openpty()
        # The composer omits its "Enter send" hint when the workspace path on
        # its bottom border leaves no room; CI artifact paths exceed 120 columns.
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 45, COLUMNS, 0, 0))
        process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, cwd=out, env=environment, start_new_session=True)
        os.close(slave)
        screen = h.TerminalScreen(columns=COLUMNS)
        transcript = bytearray()

        def drain():
            while select.select([master], [], [], 0)[0]:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    break
                if not data:
                    break
                transcript.extend(data)
                screen.feed(data)

        def wait(check, description):
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                drain()
                h.require(not errors, '; '.join(errors))
                if check():
                    return
                if process.poll() is not None:
                    drain()
                    h.require(check(), 'CLI exited: ' + screen.text())
                    return
                time.sleep(.025)
            raise AssertionError(name + ': ' + description + '\n' + screen.text())

        def send(text):
            os.write(master, text.encode() + b'\r')

        def footer():
            # The unified composer shows model and effort on its top border and
            # the status line below it; read the composer block, not one row.
            lines = screen.text().splitlines()
            top = max((i for i, line in enumerate(lines) if line.lstrip().startswith('╭─')), default=len(lines) - 1)
            return ' '.join(lines[top:])

        start = len(requests)
        try:
            wait(lambda: 'Enter send' in screen.text(), 'initial composer absent')
            if fail_first:
                if queued_failure:
                    os.write(master, b'/model sonnet\rDo not send this queued prompt to the previous provider\r')
                else:
                    send('/model sonnet')
                wait(lambda: 'nanocodex --claude auth login' in screen.text(), 'missing target-auth hint')
                if queued_failure:
                    # The unauthenticated selection is rejected before any rebuild,
                    # so the prompt typed behind it stays on the retained Codex
                    # harness: it is neither dropped nor routed to Claude.
                    wait(lambda: f'model-selection-reply-{start+1}' in screen.text(), 'prompt after rejected selection absent')
                    (out / 'rejected-selection.txt').write_text(screen.text())
                    h.require(len(requests) == start + 1, 'rejected selection duplicated inference')
                    h.require(requests[-1]['family'] == 'codex', 'rejected selection routed the prompt to Claude')
                    h.require(requests[-1]['request']['model'] == 'gpt-6.1-sol', 'rejected selection changed the Codex model')
                    wait(lambda: 'Working' not in footer() and 'Queued' not in footer(), 'turn did not finish')
                    os.write(master, b'\x03\x03')
                    wait(lambda: process.poll() is not None, 'CLI did not exit')
                    h.require(process.returncode == 0, 'CLI failed to exit cleanly')
                    checks.append({'scenario': name, 'model': 'gpt-6.1-sol', 'requests': 1, 'passed': True})
                    return
                h.require(len(requests) == start, 'failed selection dispatched inference')
            send('/model')
            wait(lambda: 'Select model' in screen.text(), 'model picker absent')
            models = ['gpt-6-astra', 'gpt-6.1-sol', 'gpt-6-luna', 'claude-opus-5-5', 'claude-sonnet-5-5', 'claude-haiku-5-5', 'claude-fable-5-1', 'claude-opus-4-6', 'claude-sonnet-4-6', 'claude-haiku-4-5']
            # The slash-command popup can briefly cover the picker's last rows.
            if not claude_auth:
                # Claude models are offered only once Claude is signed in.
                models = [model for model in models if not model.startswith('claude-')]
            # Unauthenticated, also wait for the footer that bounds the picker box.
            wait(lambda: all(model in screen.text() for model in models) and (claude_auth or 'esc cancel' in screen.text()), 'picker omitted a model or its footer')
            if not claude_auth:
                # Read only the picker box: the workspace path on the composer
                # border below it (claude-model-picker-cli/...) contains "claude-".
                picker_box = screen.text().split('Select model', 1)[-1].split('esc cancel', 1)[0]
                h.require('esc cancel' in screen.text(), 'picker footer absent')
                h.require('claude-' not in picker_box, 'picker offered unauthenticated Claude models')
            (out / 'picker.txt').write_text(screen.text())
            if picker:
                # Default Sol is second; Sonnet is fifth in the unified picker.
                os.write(master, b'\x1b[B\x1b[B\x1b[B\r')
            else:
                os.write(master, b'\x1b')
                wait(lambda: 'Select model' not in screen.text(), 'picker failed to close')
                send('/model ' + target)
                if queued:
                    send('First prompt after model selection')
            wait(lambda: expected in footer(), 'selected model missing from footer')
            if not queued:
                h.require(len(requests) == start, 'selection sent a model request')
                send('First prompt after model selection')
            wait(lambda: f'model-selection-reply-{start+1}' in screen.text(), 'first answer absent')
            h.require(len(requests) == start + 1, 'unexpected first-turn requests')
            h.require(requests[-1]['request']['model'] == expected, 'wrong first-turn wire model')
            if expected == 'claude-haiku-4-5':
                h.require('thinking' not in requests[-1]['request'], 'Haiku 4.5 retained adaptive thinking')
                # Haiku 4.5 has no effort setting: the composer border shows no effort.
                border = next(line for line in reversed(screen.text().splitlines()) if line.lstrip().startswith('╭─'))
                h.require(not any(f' {effort} ' in border for effort in ('low', 'medium', 'high', 'xhigh', 'max')), 'Haiku 4.5 shows an effort it does not send: ' + border.strip())
            if expected == 'claude-haiku-5-5':
                wire = requests[-1]['request']
                effort = wire.get('output_config', {}).get('effort')
                h.require(wire.get('thinking', {}).get('type') == 'adaptive', 'Haiku 5.5 lost adaptive thinking')
                h.require(effort is not None and f' {effort} ' in footer(), 'Haiku 5.5 effort display does not match the wire')
            # Without Claude credentials, change to another listed Codex model so
            # the rejection proves the started-thread rule, not the auth hint.
            send('/model sol' if expected.startswith('claude') else '/model sonnet' if claude_auth else '/model astra')
            wait(lambda: 'only be changed before the first prompt' in screen.text(), 'started thread allowed model change')
            h.require(expected in footer(), 'rejected change altered displayed model')
            send('Second prompt keeps the selected model')
            wait(lambda: f'model-selection-reply-{start+2}' in screen.text(), 'second answer absent')
            h.require(len(requests) == start + 2, 'rejected change dispatched a model request')
            h.require(requests[-1]['request']['model'] == expected, 'model changed after thread started')
            wait(lambda: 'Working' not in footer() and 'Queued' not in footer(), 'turn did not finish')
            # The unified TUI exits on a second Ctrl+C.
            os.write(master, b'\x03\x03')
            wait(lambda: process.poll() is not None, 'CLI did not exit')
            drain()
            h.require(process.returncode == 0, 'CLI failed to exit cleanly')
            checks.append({'scenario': name, 'model': expected, 'requests': 2, 'passed': True})
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            (out / 'terminal.pty').write_bytes(transcript)
            (out / 'screen.txt').write_text(screen.text())
            os.close(master)

    outcome = {'success': False}
    try:
        journey('default-picker-to-claude', 'codex', 'sonnet', 'claude-sonnet-5-5', picker=True)
        journey('claude-only-credentials', 'codex', 'sonnet', 'claude-sonnet-5-5', codex_auth=False)
        journey('queued-first-prompt', 'codex', 'sonnet', 'claude-sonnet-5-5', queued=True)
        journey('claude-to-codex', 'claude', 'sol', 'gpt-6.1-sol')
        journey('haiku-effort', 'claude', 'haiku', 'claude-haiku-5-5')
        journey('haiku-45-ordinary', 'claude', 'claude-haiku-4-5', 'claude-haiku-4-5')
        journey('failed-auth-retains-codex', 'codex', 'luna', 'gpt-6-luna', claude_auth=False, fail_first=True)
        journey('queued-failed-auth', 'codex', 'luna', 'gpt-6-luna', claude_auth=False, fail_first=True, queued_failure=True)
        outcome = {'success': True, 'checks': checks, 'provider_requests': len(requests)}
    finally:
        (artifact / 'outcome.json').write_text(json.dumps(outcome, indent=2))
        server.shutdown()
        print(json.dumps({'artifact': str(artifact), **outcome}))


if __name__ == '__main__':
    main()
