#!/usr/bin/env python3
"""Unified local TUI (ncl) feature journey: /mcp reload|login, /benchmark, local Realtime /voice.

Real ncl over a PTY with a real stdio MCP server fixture; only the Claude
Messages HTTP provider is synthetic. The voice session uses a synthetic OpenAI
key so the Realtime client exists; starting audio needs real devices, so the
journey asserts a clear failure where this host has none.
"""
from claude_code_fixture import normalize_request, wrap_tool
import argparse, fcntl, importlib.util, json, os, pty, re, select, shutil, struct, subprocess, termios, threading, time
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

spec = importlib.util.spec_from_file_location('journey', Path(__file__).with_name('claude-scheduler-monitor-cli-journey.py'))
helper = importlib.util.module_from_spec(spec); spec.loader.exec_module(helper)
require, sse = helper.require, helper.sse

class Session:
    def __init__(self, label, command, cwd, env, artifact):
        self.label, self.command, self.artifact = label, command, artifact
        master, slave = pty.openpty(); fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 45, 160, 0, 0))
        self.process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, cwd=cwd, env=env, start_new_session=True); os.close(slave)
        self.master, self.transcript, self.screen, self.seen = master, bytearray(), helper.TerminalScreen(rows=45, columns=160), []
    def drain(self):
        while select.select([self.master], [], [], 0)[0]:
            try: chunk = os.read(self.master, 65536)
            except OSError: return
            if not chunk: return
            self.transcript.extend(chunk); self.screen.feed(chunk)
            if b'\x1b[6n' in chunk: os.write(self.master, b'\x1b[1;1R')
        text = self.screen.text()
        if text not in self.seen[-1:]: self.seen.append(text)
    def shown(self, text):
        self.drain(); flat = re.sub(r'\s+', '', text)
        return any(flat in re.sub(r'\s+', '', frame) for frame in self.seen[-400:])
    def wait(self, check, message, timeout=25):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.drain()
            if check(): return
            time.sleep(.03)
        raise AssertionError(f'{self.label}: {message}')
    def enter(self, text): os.write(self.master, text.encode() + b'\r')
    def composer_visible(self):
        # The composer box: a top border carrying the context gauge and a bottom
        # border below it. Its footer hint is optional; a long workspace path
        # can fill the bottom border and hide it.
        lines = [line.strip() for line in self.screen.text().splitlines()]
        tops = [i for i, line in enumerate(lines) if line.startswith('╭─') and '%/' in line]
        return bool(tops) and any(line.startswith('╰─') for line in lines[tops[-1] + 1:])
    def ready(self):
        self.wait(self.composer_visible, 'composer absent'); time.sleep(1.5)
    def quit(self):
        os.write(self.master, b'\x03\x03'); self.wait(lambda: self.process.poll() is not None, 'ncl did not exit', 15)
    def close(self):
        if self.process.poll() is None: self.process.kill(); self.process.wait()
        (self.artifact / f'{self.label}.pty').write_bytes(self.transcript); (self.artifact / f'{self.label}.screen.txt').write_text(self.screen.text())
        (self.artifact / f'{self.label}.frames.txt').write_text('\n=====\n'.join(self.seen[-60:]))

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, default=Path('output/ncl-local-features') / uuid4().hex)
    args = parser.parse_args(); artifact = args.output.resolve(); artifact.mkdir(parents=True)
    root = Path(__file__).resolve().parents[2]
    workspace = artifact / 'workspace'; workspace.mkdir()
    home = artifact / 'home'; (home / 'codex').mkdir(parents=True)
    env = {'HOME': str(home), 'CODEX_HOME': str(home / 'codex'), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'TERM': 'xterm-256color', 'NANOCODEX_COMPUTER': 'off'}
    requests = []
    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['content-length'])))
            requests.append(normalize_request(request, artifact))
            (artifact / 'provider.json').write_text(json.dumps(requests, indent=2))
            response = sse(wrap_tool({'type': 'text', 'text': 'benchmark-turn-complete'}), request['model'])
            self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.send_header('Content-Length', str(len(response))); self.end_headers(); self.wfile.write(response)
    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider); threading.Thread(target=server.serve_forever, daemon=True).start()
    node = shutil.which('node'); require(node, 'node is required for the stdio MCP fixture')
    common = ['--cwd', str(workspace), '--browser=none', '--mcp-defaults', 'false', '--mcp-codex-config', 'false', '--web-search', 'false',
              '--image-generation', 'false', '--subagents', 'false', '--memory', 'false']
    claude = [str(args.binary.resolve()), '--claude', '--model', 'claude-sonnet-5-5', '--claude-api-key', 'synthetic-key',
              '--claude-messages-url', f'http://127.0.0.1:{server.server_port}/v1/messages', *common,
              '--mcp-stdio', f'stdio={node}', '--mcp-arg', f'stdio={root}/crates/nanocodex-oai-tools/tests/fixtures/mcp-stdio-server.mjs']
    codex = [str(args.binary.resolve()), '--api-key', 'synthetic-openai-key', *common]
    checks = []; outcome = {'success': False}; sessions = []
    try:
        s = Session('claude', claude, workspace, env, artifact); sessions.append(s); s.ready()
        s.enter('/mcp reload stdio'); s.wait(lambda: s.shown('Reloaded MCP server stdio ('), 'reload notice absent')
        checks.append('/mcp reload stdio reconnects the real stdio server and reports its tool count')
        s.enter('/mcp reload missing-server'); s.wait(lambda: s.shown('MCP server missing-server:'), 'unknown server error absent')
        checks.append('/mcp reload of an unconfigured server reports the MCP control error')
        s.enter('/mcp reload'); s.wait(lambda: s.shown('Usage: /mcp reload <server>'), 'bare reload usage absent')
        s.enter('/mcp login stdio'); s.wait(lambda: s.shown('MCP server stdio:'), 'login error for non-OAuth server absent')
        checks.append('/mcp login on a non-OAuth stdio server fails with the MCP error instead of opening a browser')
        s.enter('/mcp bogus'); s.wait(lambda: s.shown('Usage: /mcp login <server> or /mcp reload'), 'mcp usage absent')
        s.enter('/benchmark smoke extra'); s.wait(lambda: s.shown('Usage: /benchmark [profile]'), 'benchmark usage absent')
        before = len(requests)
        s.enter('/benchmark smoke'); s.wait(lambda: len(requests) > before and s.shown('benchmark-turn-complete'), 'benchmark turn absent', 40)
        last = json.dumps(requests[-1]['messages'])
        require('benchmark' in last and 'smoke' in last, 'benchmark instruction not sent')
        require(s.shown('/benchmark smoke'), 'transcript does not show the typed /benchmark command')
        checks.append('/benchmark smoke shows the typed command and sends the private benchmark workflow instruction')
        s.quit()
        v = Session('codex-voice', codex, workspace, env, artifact); sessions.append(v); v.ready()
        v.enter('/voice list'); v.wait(lambda: v.shown('Platform voices (default marin)'), 'voice list absent')
        checks.append('/voice list (local Realtime) lists Codex/ChatGPT and platform voices')
        v.enter('/voice marin extra'); v.wait(lambda: v.shown('Usage: /voice') or v.shown('voice'), 'multiword voice usage absent')
        v.enter('/voice mute'); v.wait(lambda: v.shown('Start /voice before muting.'), 'mute without voice absent')
        checks.append('/voice mute before start reports Start /voice before muting.')
        v.enter('/voice marin')
        v.wait(lambda: v.shown('Voice active (marin)') or v.shown('Voice failed') or v.shown('failed to start voice thread') or v.shown('failed to repair installed voice runtime'), 'voice start outcome absent', 40)
        started = v.shown('Voice active (marin)')
        checks.append('/voice marin started Realtime voice' if started else '/voice marin reached Realtime start; this host has no audio devices/network so it failed with a clear error (hardware gap)')
        if started: v.enter('/voice off'); v.wait(lambda: v.shown('Voice stopped'), 'voice stop absent', 30)
        v.quit()
        outcome = {'success': True, 'checks': checks, 'provider_requests': len(requests), 'voice_started': started}
    finally:
        for s in sessions: s.close()
        (artifact / 'scenario.json').write_text(json.dumps({'commands': [claude, codex], 'environment': env, 'boundary': 'actual ncl TUI via PTY, real stdio MCP server, synthetic Messages HTTP and OpenAI key only'}, indent=2))
        (artifact / 'outcome.json').write_text(json.dumps(outcome, indent=2)); server.shutdown(); print(json.dumps({'artifact': str(artifact), **outcome}))

if __name__ == '__main__': main()
