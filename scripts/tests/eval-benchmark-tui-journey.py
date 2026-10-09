#!/usr/bin/env python3
"""nanocodex eval benchmark journey: the workflow opens in the shared local TUI.

The real CLI runs over a PTY against a real SQLite profile (the local-smoke
recipe) and the installed VM guest runtime for host preflight; only the Claude
Messages HTTP provider is synthetic. The transcript must show /benchmark PROFILE
while the provider receives the private controller workflow, and quitting with
the board incomplete must exit retryably.

python3 scripts/tests/eval-benchmark-tui-journey.py --binary target/debug/nanocodex
"""
from claude_code_fixture import normalize_request, wrap_tool
import argparse, importlib.util, json, re, shutil, subprocess, threading
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

spec = importlib.util.spec_from_file_location('features', Path(__file__).with_name('ncl-local-features-journey.py'))
features = importlib.util.module_from_spec(spec); spec.loader.exec_module(features)
Session, require, sse = features.Session, features.require, features.sse

WORKFLOW = 'You are the neural controller.'

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--vm-guest', type=Path, default=Path.home() / '.nanocodex/current/nanocodex-vm-guest',
                        help='matching nanocodex-vm-guest for the prepared-host preflight')
    parser.add_argument('--output', type=Path, default=Path('output/eval-benchmark-tui') / uuid4().hex)
    args = parser.parse_args(); artifact = args.output.resolve(); artifact.mkdir(parents=True)
    root = Path(__file__).resolve().parents[2]; config = root / 'nanocodex.toml'
    require(args.vm_guest.is_file(), f'prepared eval host needs nanocodex-vm-guest; pass --vm-guest ({args.vm_guest} missing)')
    # The preflight resolves the guest runtime beside the running executable.
    install = artifact / 'bin'; install.mkdir()
    binary = install / 'nanocodex'; shutil.copy2(args.binary, binary); shutil.copy2(args.vm_guest, install / 'nanocodex-vm-guest')
    workspace = artifact / 'workspace'; workspace.mkdir()
    home = artifact / 'home'; (home / 'codex').mkdir(parents=True); state = artifact / 'evals'
    env = {'HOME': str(home), 'CODEX_HOME': str(home / 'codex'), 'NANOCODEX_HOME': str(artifact / 'nanocodex-home'),
           'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'TERM': 'xterm-256color', 'NANOCODEX_COMPUTER': 'off'}
    add = [str(binary), 'eval', 'add', 'smoke', '--recipe', 'local-smoke', '--config', str(config), '--state-dir', str(state)]
    added = subprocess.run(add, cwd=root, env=env, capture_output=True, text=True, timeout=120)
    (artifact / 'eval-add.log').write_text(added.stdout + added.stderr)
    require(added.returncode == 0, f'eval add failed: {added.stderr.strip()}')
    requests = []
    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['content-length'])))
            requests.append(normalize_request(request, artifact))
            (artifact / 'provider.json').write_text(json.dumps(requests, indent=2))
            response = sse(wrap_tool({'type': 'text', 'text': 'benchmark-controller-idle'}), request['model'])
            self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.send_header('Content-Length', str(len(response))); self.end_headers(); self.wfile.write(response)
    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider); threading.Thread(target=server.serve_forever, daemon=True).start()
    command = [str(binary), 'eval', 'benchmark', 'smoke', '--config', str(config), '--state-dir', str(state),
               '--claude', '--model', 'claude-sonnet-5-5', '--claude-api-key', 'synthetic-key',
               '--claude-messages-url', f'http://127.0.0.1:{server.server_port}/v1/messages',
               '--cwd', str(workspace), '--browser=none', '--mcp-defaults', 'false', '--mcp-codex-config', 'false',
               '--web-search', 'false', '--image-generation', 'false', '--subagents', 'false', '--memory', 'false']
    checks = []; outcome = {'success': False}; sessions = []
    try:
        s = Session('eval-benchmark', command, workspace, env, artifact); sessions.append(s)
        s.wait(lambda: requests and s.shown('benchmark-controller-idle'), 'benchmark workflow turn absent', 90)
        sent = json.dumps(requests[0]['messages'])
        require(WORKFLOW in sent and 'Drive the pre-materialized benchmark smoke' in sent, 'provider did not receive the benchmark workflow')
        require(str(state) in sent, 'workflow does not address the selected SQLite state directory')
        checks.append('the provider receives the private controller workflow for profile smoke and its state directory')
        require(s.shown('/benchmark smoke'), 'transcript does not show /benchmark smoke')
        require(not s.shown(WORKFLOW), 'transcript shows the private workflow instruction')
        checks.append('the shared TUI transcript shows /benchmark smoke, not the workflow instruction')
        s.quit()
        require(s.process.returncode == 75, f'incomplete board exit {s.process.returncode}, expected retryable 75')
        # Printed after the TUI exits; the final screen holds it even when the closing read fails.
        final = re.sub(r'\s+', '', s.screen.text())
        require('benchmarkboardremainsincomplete:0/8tasksfinished' in final, 'incomplete board error absent')
        checks.append('quitting with the board unfinished exits 75 with the incomplete-board error')
        outcome = {'success': True, 'checks': checks, 'provider_requests': len(requests)}
    finally:
        for s in sessions: s.close()
        (artifact / 'scenario.json').write_text(json.dumps({'setup': add, 'command': command, 'environment': env,
            'boundary': 'actual nanocodex eval benchmark TUI via PTY, real SQLite profile and VM guest preflight, synthetic Messages HTTP only'}, indent=2))
        (artifact / 'outcome.json').write_text(json.dumps(outcome, indent=2)); server.shutdown(); print(json.dumps({'artifact': str(artifact), **outcome}))

if __name__ == '__main__': main()
