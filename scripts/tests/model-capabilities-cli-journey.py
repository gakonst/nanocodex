#!/usr/bin/env python3
"""Model capability gating through the shipped CLI and terminal UI; only inference is synthetic.

Each selected model offers only the thinking levels, fast processing and Pro
mode its shared capabilities accept; a deliberate model switch normalizes
retained settings; explicit unsupported selections fail before any request.

python3 scripts/tests/model-capabilities-cli-journey.py --binary target-test/debug/nanocodex  # staged as ncl
Evidence: output/model-capabilities-cli/<uuid>/ (commands, wire requests, PTY, screens, outcome).
"""
import argparse, fcntl, importlib.util, json, os, pty, select, struct, subprocess, termios, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from uuid import uuid4

# The composer omits its "Enter send" hint when the workspace path on its
# bottom border leaves no room; CI artifact paths exceed 170 columns.
COLUMNS = 240
spec = importlib.util.spec_from_file_location('screen', Path(__file__).with_name('claude-scheduler-monitor-cli-journey.py'))
h = importlib.util.module_from_spec(spec)
spec.loader.exec_module(h)
EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', type=Path, default=Path('output/model-capabilities-cli') / uuid4().hex)
    args = parser.parse_args()
    artifact = args.output.resolve()
    artifact.mkdir(parents=True)
    # The local command tree is selected by the invoked name ncl; the managed
    # tree (run, settings) by nanocodex.
    managed_binary = args.binary.resolve()
    binary = str(managed_binary)
    if managed_binary.name == 'ncl':
        (artifact / 'bin').mkdir()
        managed_binary = artifact / 'bin' / 'nanocodex'
        try:
            os.link(binary, managed_binary)
        except OSError:
            import shutil
            shutil.copy2(binary, managed_binary)
    else:
        alias_dir = artifact / 'bin'
        alias_dir.mkdir()
        alias = alias_dir / 'ncl'
        try:
            os.link(managed_binary, alias)
        except OSError:
            import shutil
            shutil.copy2(managed_binary, alias)
        hand = managed_binary.with_name('nanocodex-hand')
        if hand.is_file():
            try:
                os.link(hand, alias_dir / 'nanocodex-hand')
            except OSError:
                pass
        binary = str(alias)
    requests, errors, checks = [], [], []

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            requests.append({'family': 'account', 'path': self.path, 'method': 'GET'})
            self.send_response(503)
            self.send_header('Content-Length', '0')
            self.end_headers()

        def do_POST(self):
            if not self.path.startswith('/v1/'):
                requests.append({'family': 'account', 'path': self.path, 'method': 'POST'})
                self.send_response(503)
                self.send_header('Content-Length', '0')
                self.end_headers()
                return
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            family = 'claude' if self.path == '/v1/messages' else 'codex'
            requests.append({'family': family, 'path': self.path, 'beta': self.headers.get('anthropic-beta'), 'request': request})
            answer = 'capability-reply-' + str(len(requests))
            if family == 'claude':
                response = h.sse({'type': 'text', 'text': answer}, request['model'])
            else:
                response = ('data: ' + json.dumps({'type': 'response.completed', 'response': {
                    'id': uuid4().hex, 'status': 'completed',
                    'output': [{'type': 'message', 'role': 'assistant', 'content': [{'type': 'output_text', 'text': answer}]}],
                    'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2}}}) + '\n\n').encode()
            (artifact / 'provider.json').write_text(json.dumps(requests, indent=2))
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(response)))
            self.end_headers()
            self.wfile.write(response)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f'http://127.0.0.1:{server.server_port}/v1'
    common = ['--api-base-url', base, '--claude-messages-url', base + '/messages', '--responses-transport', 'https',
              '--websocket-warmup', 'false', '--store-responses', 'false', '--rollouts', 'false', '--browser=none',
              '--mcp-defaults', 'false', '--mcp-codex-config', 'false', '--web-search', 'false',
              '--image-generation', 'false', '--subagents', 'false', '--memory', 'false',
              '--api-key', 'synthetic-codex-key', '--claude-api-key', 'synthetic-claude-key']

    def environment(out):
        home = out / 'home'
        home.mkdir(exist_ok=True)
        return {'HOME': str(home), 'CODEX_HOME': str(home / 'codex'), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin',
                'TERM': 'xterm-256color', 'NANOCODEX_COMPUTER': 'off'}

    def tui(name, launch, steps):
        out = artifact / name
        out.mkdir()
        command = [binary, '--cwd', str(out), *common, *launch]
        (out / 'scenario.json').write_text(json.dumps({'command': command}, indent=2))
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 45, COLUMNS, 0, 0))
        process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, cwd=out, env=environment(out), start_new_session=True)
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

        def wait(check, description, timeout=30):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                drain()
                h.require(not errors, '; '.join(errors))
                if check():
                    return
                h.require(process.poll() is None, name + ': CLI exited: ' + screen.text())
                time.sleep(.025)
            raise AssertionError(name + ': ' + description + '\n' + screen.text())

        def footer():
            lines = screen.text().splitlines()
            top = max((i for i, line in enumerate(lines) if line.lstrip().startswith('╭─')), default=len(lines) - 1)
            return ' '.join(lines[top:])

        def send(text):
            os.write(master, text.encode() + b'\r')

        def picker():
            """Opens /thinking and cycles the dial; returns its offered efforts and Pro visibility."""
            send('/thinking')
            wait(lambda: 'Selected Effort:' in screen.text(), 'effort picker absent')
            seen = []
            for _ in range(len(EFFORTS) + 1):
                label = screen.text().split('Selected Effort:', 1)[1].split()[0]
                if label not in seen:
                    seen.append(label)
                previous = label
                os.write(master, b'\x1b[C')
                deadline = time.monotonic() + 2
                while time.monotonic() < deadline:
                    drain()
                    if screen.text().split('Selected Effort:', 1)[1].split()[0] != previous:
                        break
                    time.sleep(.02)
            pro = 'Pro:' in screen.text()
            (out / 'picker.txt').write_text(screen.text())
            os.write(master, b'\x1b')
            wait(lambda: 'Selected Effort:' not in screen.text(), 'picker did not close')
            return sorted(seen, key=EFFORTS.index), pro

        def prompt(text):
            start = len(requests)
            send(text)
            wait(lambda: len(requests) > start and f'capability-reply-{len(requests)}' in screen.text(), 'reply absent')
            wait(lambda: 'Working' not in footer(), 'turn did not finish')
            h.require(len(requests) == start + 1, 'unexpected request count')
            return requests[-1]

        ctx = {'wait': wait, 'send': send, 'footer': footer, 'picker': picker, 'prompt': prompt, 'screen': screen, 'requests': requests}
        try:
            wait(lambda: 'Enter send' in screen.text(), 'initial composer absent')
            result = steps(ctx)
            os.write(master, b'\x03\x03')
            wait(lambda: process.poll() is not None, 'CLI did not exit')
            checks.append({'scenario': name, 'passed': True, **result})
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            drain()
            (out / 'terminal.pty').write_bytes(transcript)
            (out / 'screen.txt').write_text(screen.text())
            os.close(master)

    def codex_xhigh_to_opus46(c):
        # A Codex session at xhigh: Sol offers every effort but none.
        efforts, _ = c['picker']()
        h.require(efforts == EFFORTS, f'Sol picker offered {efforts}')
        c['send']('/model claude-opus-4-6')
        c['wait'](lambda: 'claude-opus-4-6' in c['footer'](), 'switch to Opus 4.6 absent')
        # The retained xhigh is unsupported by Opus 4.6 and resets to its default.
        c['wait'](lambda: ' high ' in c['footer']() and ' xhigh ' not in c['footer'](), 'retained xhigh not normalized: ' + c['footer']())
        efforts, pro = c['picker']()
        h.require(efforts == ['low', 'medium', 'high', 'max'], f'Opus 4.6 picker offered {efforts}')
        h.require(not pro, 'Opus 4.6 offered Pro mode')
        sent = len(c['requests'])
        c['send']('/thinking xhigh')
        c['wait'](lambda: 'does not support xhigh effort' in c['screen'].text(), 'explicit xhigh not rejected')
        c['send']('/fast on')
        c['wait'](lambda: 'Fast mode is unavailable for this model' in c['screen'].text(), 'fast mode offered on Opus 4.6')
        h.require(len(c['requests']) == sent, 'rejected settings dispatched a request')
        wire = c['prompt']('First prompt after switching to Opus 4.6')
        h.require(wire['request']['model'] == 'claude-opus-4-6', 'wrong wire model')
        h.require(wire['request'].get('output_config', {}).get('effort') == 'high', 'wire effort is not the normalized high: ' + json.dumps(wire['request'].get('output_config')))
        h.require('speed' not in wire['request'], 'unsupported speed reached the wire')
        return {'sol_efforts': EFFORTS, 'opus46_efforts': efforts, 'wire_effort': 'high'}

    def opus55_extended(c):
        efforts, pro = c['picker']()
        h.require(efforts == EFFORTS, f'Opus 5.5 picker offered {efforts}')
        h.require(not pro, 'Opus 5.5 offered Pro mode')
        c['send']('/thinking max')
        c['wait'](lambda: ' max ' in c['footer'](), 'max not applied')
        c['send']('/fast on')
        c['wait'](lambda: 'Fast mode is unavailable' not in c['screen'].text() and ('fast' in c['footer']().lower() or '⚡' in c['footer']()), 'fast mode not applied', timeout=5)
        wire = c['prompt']('Prompt at max effort in fast mode')
        h.require(wire['request'].get('output_config', {}).get('effort') == 'max', 'max effort missing on the wire')
        h.require(wire['request'].get('speed') == 'fast', 'fast mode missing on the wire')
        return {'opus55_efforts': efforts, 'wire_effort': 'max', 'wire_speed': 'fast'}

    def haiku45_fixed(c):
        sent = len(c['requests'])
        c['send']('/thinking')
        c['wait'](lambda: 'has no adjustable effort' in c['screen'].text(), 'Haiku 4.5 opened an effort picker')
        c['send']('/thinking max')
        c['wait'](lambda: 'does not support max effort' in c['screen'].text(), 'Haiku 4.5 accepted max')
        h.require(len(c['requests']) == sent, 'rejected settings dispatched a request')
        wire = c['prompt']('Haiku 4.5 prompt')
        h.require('thinking' not in wire['request'] and 'output_config' not in wire['request'], 'Haiku 4.5 sent effort')
        return {'haiku45': 'no adjustable effort'}

    def launch_rejected(name, flags, expected):
        """An explicit unsupported launch selection fails before any request."""
        out = artifact / name
        out.mkdir()
        start = len(requests)
        command = [binary, '--cwd', str(out), *common, *flags]
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 45, COLUMNS, 0, 0))
        process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, cwd=out, env=environment(out), start_new_session=True)
        os.close(slave)
        screen, transcript = h.TerminalScreen(columns=COLUMNS), bytearray()
        deadline = time.monotonic() + 30
        try:
            while time.monotonic() < deadline:
                while select.select([master], [], [], 0.05)[0]:
                    try:
                        data = os.read(master, 65536)
                    except OSError:
                        data = b''
                    if not data:
                        break
                    transcript.extend(data)
                    screen.feed(data)
                text = screen.text() + transcript.decode(errors='replace')
                if expected in text or process.poll() is not None:
                    break
            text = screen.text() + transcript.decode(errors='replace')
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            (out / 'terminal.pty').write_bytes(transcript)
            (out / 'screen.txt').write_text(screen.text())
            (out / 'command.json').write_text(json.dumps({'command': command, 'returncode': process.returncode}, indent=2))
            os.close(master)
        h.require(expected in text, name + ': missing actionable error ' + repr(expected) + ':\n' + screen.text()[-1500:])
        h.require(len(requests) == start, name + ': invalid selection reached the provider')
        checks.append({'scenario': name, 'passed': True, 'flags': flags, 'error': expected, 'returncode': process.returncode})

    def managed_rejected(name, argv, expected):
        """The managed control-plane projection rejects unsupported settings before any account request."""
        out = artifact / name
        out.mkdir()
        command = [str(managed_binary), *argv]
        start = len(requests)
        # A synthetic account credential and a local control-plane origin: the
        # settings must be rejected before any account request is made.
        env = {**environment(out), 'NANOCODEX_API_KEY': 'ncx_live_aaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'NANOCODEX_MANAGED_URL': f'http://127.0.0.1:{server.server_port}'}
        result = subprocess.run(command, cwd=out, env=env, capture_output=True, text=True, timeout=30)
        (out / 'command.json').write_text(json.dumps({'command': command, 'returncode': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr}, indent=2))
        h.require(result.returncode != 0, name + ': unsupported managed setting succeeded')
        h.require(len(requests) == start, name + ': unsupported managed setting reached the control plane')
        h.require(expected in result.stderr + result.stdout, name + ': missing actionable error: ' + (result.stderr + result.stdout)[-800:])
        checks.append({'scenario': name, 'passed': True, 'argv': argv, 'error': expected})

    def opus55_launch_fast_xhigh(c):
        h.require(' xhigh ' in c['footer'](), 'launch xhigh not shown: ' + c['footer']())
        wire = c['prompt']('Prompt with explicit launch settings')
        h.require(wire['request'].get('speed') == 'fast', 'explicit Opus 5.5 fast mode was dropped')
        h.require(wire['request'].get('output_config', {}).get('effort') == 'xhigh', 'xhigh effort missing')
        return {'wire_effort': 'xhigh', 'wire_speed': 'fast'}

    outcome = {'success': False}
    try:
        launch_rejected('launch-sonnet46-fast', ['--claude', '--model', 'claude-sonnet-4-6', '--fast-mode', 'true'], 'Claude Sonnet 4.6 (claude-sonnet-4-6) does not support fast mode')
        launch_rejected('launch-opus46-xhigh', ['--claude', '--model', 'claude-opus-4-6', '--thinking', 'xhigh'], 'supported thinking: low, medium, high, max')
        launch_rejected('launch-sol-none', ['--model', 'gpt-6.1-sol', '--thinking', 'none'], 'gpt-6.1-sol does not support none thinking')
        launch_rejected('launch-claude-pro', ['--claude', '--model', 'opus', '--reasoning-mode', 'pro'], 'does not support pro reasoning mode')
        managed_rejected('managed-run-claude-max', ['run', '--model', 'claude-opus-5-5', '--thinking', 'max', 'Synthetic prompt'], 'on the managed service; supported thinking: low, medium, high')
        managed_rejected('managed-run-claude-fast', ['run', '--model', 'claude-opus-5-5', '--fast-mode', 'true', 'Synthetic prompt'], 'does not support fast mode on the managed service')
        tui('tui-o55-fx', ['--claude', '--model', 'opus', '--fast-mode', 'true', '--thinking', 'xhigh'], opus55_launch_fast_xhigh)
        tui('tui-sol-o46', ['--model', 'gpt-6.1-sol', '--thinking', 'xhigh'], codex_xhigh_to_opus46)
        tui('tui-o55', ['--claude', '--model', 'opus'], opus55_extended)
        tui('tui-h45', ['--claude', '--model', 'claude-haiku-4-5'], haiku45_fixed)
        outcome = {'success': True, 'checks': checks, 'provider_requests': len(requests)}
    except Exception as error:
        outcome = {'success': False, 'error': str(error), 'checks': checks}
        raise
    finally:
        (artifact / 'outcome.json').write_text(json.dumps(outcome, indent=2))
        server.shutdown()
        print(json.dumps({'artifact': str(artifact), **outcome}))


if __name__ == '__main__':
    main()

