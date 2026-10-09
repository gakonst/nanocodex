#!/usr/bin/env python3
"""Real terminal journey: provider progress is visible before the answer.

python3 scripts/tests/claude-progress-cli-journey.py --binary target/debug/nanocodex
Only the loopback Messages provider is synthetic. Evidence stays in output/.
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
spec.loader.exec_module(h)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', type=Path, default=Path('output/claude-progress-cli') / uuid4().hex)
    args = parser.parse_args()
    artifact = args.output.resolve()
    artifact.mkdir(parents=True)
    release = threading.Event()
    errors, requests = [], []

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                assert body['model'] == 'claude-opus-5-5'
                assert body['thinking'] == {'type': 'adaptive', 'display': 'updates'}
                assert body['output_config']['effort'] == 'high'
                assert 'thinking-display-updates-2026-08-18' in self.headers.get('anthropic-beta', '').split(',')
                requests.append({'model': body['model'], 'thinking': body['thinking'], 'effort': body['output_config']['effort']})
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.end_headers()

                def emit(event):
                    self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
                    self.wfile.flush()

                emit({'type': 'message_start', 'message': {'id': 'cli-progress', 'role': 'assistant', 'model': body['model'], 'content': [], 'usage': {'input_tokens': 3, 'output_tokens': 0}}})
                emit({'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'thinking', 'thinking': '', 'signature': ''}})
                emit({'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'thinking_delta', 'thinking': ''}})
                emit({'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'signature_delta', 'signature': 'PRIVATE_CLI_SIGNATURE'}})
                emit({'type': 'content_block_stop', 'index': 0})
                emit({'type': 'content_block_start', 'index': 1, 'content_block': {'type': 'thinking', 'thinking': '', 'signature': ''}})
                emit({'type': 'content_block_delta', 'index': 1, 'delta': {'type': 'thinking_delta', 'thinking': 'Checking the saved record.'}})
                assert release.wait(30), 'CLI never displayed the progress update'
                emit({'type': 'content_block_delta', 'index': 1, 'delta': {'type': 'signature_delta', 'signature': 'PRIVATE_CLI_UPDATE_SIGNATURE'}})
                emit({'type': 'content_block_stop', 'index': 1})
                emit({'type': 'content_block_start', 'index': 2, 'content_block': {'type': 'text', 'text': ''}})
                emit({'type': 'content_block_delta', 'index': 2, 'delta': {'type': 'text_delta', 'text': 'Progress journey completed.'}})
                emit({'type': 'content_block_stop', 'index': 2})
                emit({'type': 'message_delta', 'delta': {'stop_reason': 'end_turn'}, 'usage': {'output_tokens': 15}})
                emit({'type': 'message_stop'})
            except Exception as error:
                errors.append(str(error))

    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    environment = {**os.environ, 'TERM': 'xterm-256color', 'NANOCODEX_COMPUTER': 'off'}
    command = [str(args.binary.resolve()), '--claude', '--model', 'claude-opus-5-5', '--thinking', 'high',
               '--claude-api-key', 'synthetic-key', '--claude-messages-url', f'http://127.0.0.1:{server.server_port}/v1/messages',
               '--cwd', str(artifact), '--browser=none', '--mcp-defaults', 'false', '--mcp-codex-config', 'false',
               '--web-search', 'false', '--image-generation', 'false', '--subagents', 'false', '--memory', 'false',
               '--rollouts', 'false', '--prompt', 'Check a synthetic saved record and report progress.']
    (artifact / 'command.json').write_text(json.dumps(command, indent=2))
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 45, 170, 0, 0))
    process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, cwd=artifact, env=environment, start_new_session=True)
    os.close(slave)
    screen, transcript = h.TerminalScreen(), bytearray()

    def wait_for(text):
        deadline = time.monotonic() + 40
        while time.monotonic() < deadline:
            if select.select([master], [], [], .025)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    chunk = b''
                transcript.extend(chunk)
                screen.feed(chunk)
            assert not errors, errors
            if text in screen.text():
                return
            assert process.poll() is None, screen.text()
        raise AssertionError('Missing ' + text + '\n' + screen.text())

    try:
        wait_for('Checking the saved record.')
        assert 'Progress journey completed.' not in screen.text()
        assert 'PRIVATE_CLI' not in screen.text()
        (artifact / 'progress-screen.txt').write_text(screen.text())
        release.set()
        wait_for('Progress journey completed.')
        assert 'PRIVATE_CLI' not in transcript.decode(errors='replace')
        (artifact / 'completed-screen.txt').write_text(screen.text())
        (artifact / 'checks.json').write_text(json.dumps({'progress_before_answer': True, 'signatures_hidden': True, 'requests': requests}, indent=2))
        print(json.dumps({'passed': True, 'evidence': str(artifact), 'binary': str(args.binary.resolve())}))
    finally:
        release.set()
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        (artifact / 'terminal.raw').write_bytes(transcript)
        os.close(master)
        server.shutdown()


if __name__ == '__main__':
    main()
