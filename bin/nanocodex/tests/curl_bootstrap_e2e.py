#!/usr/bin/env python3
"""Real curl | sh journeys for the public POSIX bootstrap against HTTPS fixtures.

python3 bin/nanocodex/tests/curl_bootstrap_e2e.py [--installer PATH] [OUTPUT_DIR]

Real curl, dash and bash fetch the shipped installer from https://nanocodex.paradigm.xyz
through a loopback CONNECT proxy with process-local CA trust; every other host is
rejected. The GitHub release server is the only fixture and injects real
transport faults (HTTP errors, truncated bodies, missing redirects). The
bootstrap release asset is a synthetic executable that records how the
installer invoked it; the Rust installation itself is covered by
install_network_e2e.py. Every case uses its own synthetic HOME and NANOCODEX_DIR.
--installer runs the same journeys against another script, such as the
previous release, and records which expectations it misses.
Requires Linux, Python 3, OpenSSL, curl, gzip, dash and bash.
"""
import argparse
import gzip
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import tempfile
import fcntl
import threading
import time
from urllib.parse import urlsplit

REPO = Path(__file__).resolve().parents[3]
HOSTS = ('nanocodex.paradigm.xyz', 'github.com', 'raw.githubusercontent.com', 'api.github.com')
ASSET = 'nanocodex-x86_64-unknown-linux-gnu.gz'
STABLE = 'v9.1.0'
NIGHTLY_SHA = 'abcdef0123456789abcdef0123456789abcdef01'
NIGHTLY = 'nightly-' + NIGHTLY_SHA
BOOTSTRAP = r'''#!/bin/sh
if [ "$1" = --version ]; then
  if [ -n "@@{FIXTURE_LOADER_ERROR:-}" ]; then
    printf '%s\n' "$FIXTURE_LOADER_ERROR" >&2
    exit 1
  fi
  echo 'nanocodex 9.1.0 (synthetic bootstrap)'
  exit 0
fi
mkdir -p "$NANOCODEX_DIR"
if [ -t 0 ]; then stdin=tty; else stdin=not-a-tty; fi
running="$NANOCODEX_DIR/fixture-running"
if [ -e "$running" ]; then overlap=yes; else overlap=no; fi
: > "$running"
printf '{"args":"%s","tag":"%s","stdin":"%s","overlap":"%s","pid":%s}\n' \
  "$*" "$NANOCODEX_RELEASE_TAG" "$stdin" "$overlap" "$$" >> "$FIXTURE_RECORD"
sleep "@@{FIXTURE_HOLD:-0}"
rm -f "$running"
'''.replace('@@{', '$' + '{')


def check(value, message, failures):
    if not value:
        failures.append(message)


class Origin(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *args):
        pass

    def fault(self, path):
        with self.server.lock:
            queue = self.server.fixture['faults'].get(path) or []
            return queue.pop(0) if queue else None

    def record(self, method):
        path = urlsplit(self.path).path
        event = {'case': self.server.fixture['case'], 'method': method,
                 'host': self.headers.get('Host'), 'path': path, 't': round(time.monotonic(), 3)}
        with self.server.lock:
            self.server.events.append(event)
        return path, event

    def reply(self, status, body=b'', headers=()):
        self.send_response(status)
        for name, value in headers:
            self.send_header(name, value)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        if self.command != 'HEAD':
            self.wfile.write(body)
        self.wfile.flush()

    def do_HEAD(self):
        path, event = self.record('HEAD')
        fault = self.fault(path)
        event['fault'] = fault
        if fault and fault[0] == 'status':
            return self.reply(fault[1])
        if path == '/gakonst/nanocodex/releases/latest':
            if fault and fault[0] == 'nolocation':
                return self.reply(200)
            return self.reply(302, headers=[('Location', 'https://github.com/gakonst/nanocodex/releases/tag/' + STABLE)])
        return self.reply(404)

    def do_GET(self):
        path, event = self.record('GET')
        fixture = self.server.fixture
        fault = self.fault(path)
        event['fault'] = fault
        if path == '/' and self.headers.get('Host') == 'nanocodex.paradigm.xyz':
            return self.reply(301, headers=[('Location', 'https://raw.githubusercontent.com/gakonst/nanocodex/master/install')])
        body = fixture['files'].get(path)
        if body is None:
            return self.reply(404, b'Not Found')
        if fault and fault[0] == 'status':
            return self.reply(fault[1], b'transient fixture failure')
        if fault and fault[0] == 'drip':
            # Faster than the stall detector, but never finishing.
            self.send_response(200)
            self.send_header('Content-Length', str(10 ** 7))
            self.end_headers()
            try:
                while True:
                    self.wfile.write(b'#' * fault[1])
                    self.wfile.flush()
                    time.sleep(1)
            except (BrokenPipeError, ConnectionResetError, ssl.SSLError, OSError):
                self.close_connection = True
                return
        if fault and fault[0] == 'slow':
            # A healthy but slow link: the whole body, fault[1] bytes per second.
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            try:
                for offset in range(0, len(body), fault[1]):
                    self.wfile.write(body[offset:offset + fault[1]])
                    self.wfile.flush()
                    time.sleep(1)
            except (BrokenPipeError, ConnectionResetError, ssl.SSLError, OSError):
                event['cancelled'] = True
                self.close_connection = True
            return
        if fault and fault[0] == 'truncate':
            # Promise the whole body, deliver a prefix, then drop the connection.
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body[:fault[1]])
            self.wfile.flush()
            self.close_connection = True
            return
        self.reply(200, body, [('Content-Type', 'application/octet-stream')])


class Proxy(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_CONNECT(self):
        host = self.path.rsplit(':', 1)[0]
        if host not in HOSTS or not self.path.endswith(':443'):
            with self.server.lock:
                self.server.events.append({'blocked_connect': self.path})
            self.send_error(403)
            return
        self.send_response(200, 'Connection established')
        self.end_headers()
        self.wfile.flush()
        self.close_connection = True
        try:
            with self.server.tls.wrap_socket(self.connection, server_side=True) as sock:
                Origin(sock, self.client_address, self.server)
        except (ssl.SSLError, OSError) as error:
            with self.server.lock:
                self.server.events.append({'tls_error': str(error)})


def certificates(root):
    cert = root / 'ca.pem'
    (root / 'leaf.ext').write_text(
        'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\n'
        'extendedKeyUsage=serverAuth\nsubjectAltName=' + ','.join('DNS:' + h for h in HOSTS) + '\n')
    for command in (
        ['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
         '-subj', '/CN=Nanocodex synthetic curl E2E CA', '-keyout', str(root / 'ca.key'), '-out', str(cert)],
        ['openssl', 'req', '-newkey', 'rsa:2048', '-nodes', '-subj', '/CN=github.com',
         '-keyout', str(root / 'leaf.key'), '-out', str(root / 'leaf.csr')],
        ['openssl', 'x509', '-req', '-in', str(root / 'leaf.csr'), '-CA', str(cert), '-CAkey', str(root / 'ca.key'),
         '-CAcreateserial', '-days', '1', '-extfile', str(root / 'leaf.ext'), '-out', str(root / 'leaf.pem')]):
        subprocess.run(command, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    return cert


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--installer', type=Path, default=REPO / 'install')
    parser.add_argument('output', nargs='?', type=Path, default=REPO / 'output/curl-bootstrap-e2e')
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    installer = args.installer.resolve().read_bytes()
    summary = {'command': ['python3', __file__, '--installer', str(args.installer.resolve()), str(output)],
               'installer_sha256': hashlib.sha256(installer).hexdigest(), 'cases': []}
    transcript = []
    server = None
    with tempfile.TemporaryDirectory(prefix='curl-bootstrap-') as temporary:
        root = Path(temporary)
        cert = certificates(root)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
        server.daemon_threads = True
        server.events, server.lock = [], threading.Lock()
        server.tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        server.tls.load_cert_chain(root / 'leaf.pem', root / 'leaf.key')
        threading.Thread(target=server.serve_forever, daemon=True).start()
        proxy = 'http://127.0.0.1:' + str(server.server_port)
        bootstrap = gzip.compress(BOOTSTRAP.encode(), mtime=0)
        digest = hashlib.sha256(bootstrap).hexdigest()
        stable_tagged = subprocess.run(['git', '-C', str(REPO), 'show', 'v0.6.7:install'],
                                       capture_output=True, check=False).stdout

        def files(tagged=installer, manifest_digest=digest, public=installer, payload=bootstrap):
            result = {'/gakonst/nanocodex/master/install': public,
                      '/repos/gakonst/nanocodex/releases/tags/nightly': json.dumps(
                          {'tag_name': 'nightly', 'target_commitish': NIGHTLY_SHA, 'name': 'Nightly'},
                          indent=2).encode()}
            for tag in (STABLE, NIGHTLY):
                result['/gakonst/nanocodex/refs/tags/' + tag + '/install'] = tagged
                base = '/gakonst/nanocodex/releases/download/' + tag + '/'
                result[base + 'SHA256SUMS'] = (manifest_digest + '  ' + ASSET + '\n').encode()
                result[base + ASSET] = payload
            return result

        def run_case(name, *, flags='', shell='sh', faults=None, env_extra=None, files_override=None,
                     concurrent=1, prepare=None):
            case_root = root / name
            home, store = case_root / 'home', case_root / 'store'
            home.mkdir(parents=True)
            record = case_root / 'record.jsonl'
            server.fixture = {'case': name, 'files': files_override or files(),
                              'faults': {k: list(v) for k, v in (faults or {}).items()}}
            env = {'PATH': '/usr/bin:/bin', 'HOME': str(home), 'NANOCODEX_DIR': str(store),
                   'TMPDIR': str(case_root), 'FIXTURE_RECORD': str(record), 'LC_ALL': 'C',
                   'CURL_CA_BUNDLE': str(cert), 'SSL_CERT_FILE': str(cert),
                   'HTTPS_PROXY': proxy, 'https_proxy': proxy, 'NO_PROXY': '', 'no_proxy': ''}
            env.update(env_extra or {})
            command = 'curl -fsSL https://nanocodex.paradigm.xyz | ' + shell + (' -s -- ' + flags if flags else '')
            if prepare:
                prepare(case_root, store, env, command)
            start = len(server.events)
            began = time.monotonic()
            processes = []
            for index in range(concurrent):
                processes.append(subprocess.Popen(['/bin/bash', '-c', command], cwd=case_root, env=env,
                                                  stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                                  stderr=subprocess.PIPE, text=True, start_new_session=True))
                if concurrent > 1:
                    time.sleep(0.3)
            results = []
            for process in processes:
                try:
                    stdout, stderr = process.communicate(timeout=180)
                    results.append({'exit': process.returncode, 'stdout': stdout, 'stderr': stderr})
                except subprocess.TimeoutExpired:
                    # An installer that never finishes is a failure, not a harness crash.
                    os.killpg(process.pid, 9)
                    stdout, stderr = process.communicate()
                    results.append({'exit': 'killed after 180s', 'stdout': stdout, 'stderr': stderr})
            elapsed = round(time.monotonic() - began, 2)
            events = server.events[start:]
            launches = [json.loads(line) for line in record.read_text().splitlines()] if record.exists() else []
            leftovers = sorted(str(p.relative_to(case_root)) for p in case_root.glob('nanocodex-bootstrap.*'))
            leftovers += sorted(str(p.relative_to(case_root)) for p in home.glob('.cache/nanocodex/nanocodex-bootstrap.*'))
            observation = {'case': name, 'command': command, 'flags': flags, 'faults': faults or {},
                           'env': {k: v for k, v in (env_extra or {}).items()}, 'seconds': elapsed,
                           'results': results, 'requests': [(e.get('method'), e.get('host'), e.get('path'), e.get('fault'))
                                                            for e in events if 'path' in e],
                           'launches': launches, 'temporary_leftovers': leftovers,
                           'proxy_errors': [e for e in events if 'blocked_connect' in e or 'tls_error' in e]}
            transcript.append(observation)
            return observation

        def paths(obs):
            return [p for _, _, p, _ in obs['requests']]

        def evaluate(obs, failures):
            check(not obs['proxy_errors'], 'unexpected proxy or TLS failure', failures)
            check(not obs['temporary_leftovers'], 'temporary bootstrap directory left behind', failures)
            obs['expectation_failures'] = failures
            obs['passed'] = not failures
            summary['cases'].append(obs)
            print(json.dumps({'case': obs['case'], 'passed': obs['passed'], 'exit': [r['exit'] for r in obs['results']],
                              'seconds': obs['seconds'], 'failures': failures}), flush=True)

        sums = '/gakonst/nanocodex/releases/download/' + STABLE + '/SHA256SUMS'
        asset = '/gakonst/nanocodex/releases/download/' + STABLE + '/' + ASSET
        tagged = '/gakonst/nanocodex/refs/tags/' + STABLE + '/install'
        latest = '/gakonst/nanocodex/releases/latest'
        public = '/gakonst/nanocodex/master/install'
        offline = ['/']

        # Help and typos are answered locally after fetching only the public script.
        for name, flags, code, text in (('help', '--help', 0, '--nightly'), ('unknown-option', '--nigthly', 2, "unknown option '--nigthly'")):
            obs = run_case(name, flags=flags)
            f = []
            r = obs['results'][0]
            check(r['exit'] == code, f'exit {r["exit"]}, expected {code}', f)
            check(text in r['stdout'] + r['stderr'], f'output lacks {text!r}', f)
            check(paths(obs) == offline + [public], f'downloaded beyond the public script: {paths(obs)}', f)
            check(not obs['launches'], 'bootstrap executed', f)
            evaluate(obs, f)

        # Default stable install from a piped script without a terminal, under dash and bash.
        for shell in ('sh', 'bash'):
            obs = run_case('stable-default-' + shell, flags='--no-setup --no-modify-path', shell=shell)
            f = []
            check(obs['results'][0]['exit'] == 0, 'install failed: ' + obs['results'][0]['stderr'][-400:], f)
            check(paths(obs) == offline + [public, latest, tagged, sums, asset], f'unexpected requests {paths(obs)}', f)
            check([(l['args'], l['tag'], l['stdin']) for l in obs['launches']] ==
                  [('install --no-setup --no-modify-path', STABLE, 'not-a-tty')],
                  f'bootstrap launches {obs["launches"]}', f)
            evaluate(obs, f)

        # Explicit nightly resolves the immutable tag and never forwards --nightly.
        obs = run_case('nightly-flag', flags='--nightly --no-modify-path')
        f = []
        check(obs['results'][0]['exit'] == 0, 'nightly install failed: ' + obs['results'][0]['stderr'][-400:], f)
        check(latest not in paths(obs), 'resolved latest stable despite --nightly', f)
        check('/gakonst/nanocodex/refs/tags/' + NIGHTLY + '/install' in paths(obs), 'nightly tagged installer not fetched', f)
        check([(l['args'], l['tag']) for l in obs['launches']] == [('install --no-modify-path', NIGHTLY)],
              f'bootstrap launches {obs["launches"]}', f)
        evaluate(obs, f)

        obs = run_case('nightly-conflicts-with-pin', flags='--nightly', env_extra={'NANOCODEX_RELEASE_TAG': STABLE})
        f = []
        check(obs['results'][0]['exit'] != 0 and 'conflicts' in obs['results'][0]['stderr'], 'conflict not reported', f)
        check(paths(obs) == offline + [public], f'downloaded despite conflict {paths(obs)}', f)
        evaluate(obs, f)

        # The new first stage still drives the published v0.6.7 second stage.
        if stable_tagged:
            obs = run_case('stage1-with-v0.6.7-stage2', flags='--no-setup --no-modify-path',
                           files_override=files(tagged=stable_tagged))
            f = []
            check(obs['results'][0]['exit'] == 0, 'v0.6.7 stage 2 failed: ' + obs['results'][0]['stderr'][-400:], f)
            check([(l['args'], l['tag']) for l in obs['launches']] == [('install --no-setup --no-modify-path', STABLE)],
                  f'bootstrap launches {obs["launches"]}', f)
            evaluate(obs, f)

        # One transient failure on every hop: 503, 502, a dropped manifest and a truncated asset.
        faults = {latest: [('status', 503), ('status', 429)], tagged: [('status', 502)], sums: [('truncate', 10)],
                  asset: [('truncate', len(bootstrap) // 2)]}
        obs = run_case('transient-failures-recover', flags='--no-setup', faults=faults)
        f = []
        check(obs['results'][0]['exit'] == 0, 'did not recover: ' + obs['results'][0]['stderr'][-600:], f)
        for path in faults:
            expected = len(faults[path]) + 1
            check(paths(obs).count(path) == expected, f'{path} requested {paths(obs).count(path)} times, expected {expected}', f)
        check(len(obs['launches']) == 1, 'bootstrap did not run exactly once', f)
        evaluate(obs, f)

        obs = run_case('slow-drip-metadata-times-out', flags='--no-setup', faults={sums: [('drip', 2048)]})
        f = []
        check(obs['results'][0]['exit'] == 0 and len(obs['launches']) == 1, 'drip was not bounded and retried: ' +
              obs['results'][0]['stderr'][-300:], f)
        check('curl exit 28' in obs['results'][0]['stderr'] and paths(obs).count(sums) == 2, 'expected one timeout and one retry', f)
        check(55 <= obs['seconds'] <= 120, f'unexpected duration {obs["seconds"]}s for a 60s cap', f)
        evaluate(obs, f)

        # The bootstrap has no total cap: a healthy transfer slower than 60s completes.
        padding = '# ' + os.urandom(150 * 1024).hex() + '\n'
        large = gzip.compress((BOOTSTRAP + padding).encode(), mtime=0)
        rate = 2048
        obs = run_case('slow-bootstrap-completes', flags='--no-setup',
                       files_override=files(manifest_digest=hashlib.sha256(large).hexdigest(), payload=large),
                       faults={asset: [('slow', rate)]})
        f = []
        r = obs['results'][0]
        check(r['exit'] == 0 and len(obs['launches']) == 1, 'slow bootstrap did not complete: ' + r['stderr'][-300:], f)
        check(paths(obs).count(asset) == 1 and 'retry' not in r['stderr'], 'slow bootstrap was cut off and retried', f)
        check(obs['seconds'] > 65, f'transfer took {obs["seconds"]}s; expected longer than the 60s metadata cap', f)
        obs['bootstrap_bytes'] = len(large)
        evaluate(obs, f)

        obs = run_case('permanent-404-fails-fast', flags='--no-setup',
                       files_override={k: v for k, v in files().items() if k != asset})
        f = []
        r = obs['results'][0]
        check(r['exit'] != 0 and '404' in r['stderr'], 'missing 404 diagnostic', f)
        check(paths(obs).count(asset) == 1, '404 was retried', f)
        check(not obs['launches'], 'bootstrap executed', f)
        evaluate(obs, f)

        obs = run_case('checksum-mismatch', flags='--no-setup', files_override=files(manifest_digest='0' * 64))
        f = []
        check(obs['results'][0]['exit'] != 0 and 'checksum mismatch' in obs['results'][0]['stderr'], 'mismatch not reported', f)
        check(not obs['launches'], 'unverified bootstrap executed', f)
        evaluate(obs, f)

        obs = run_case('latest-without-redirect', flags='--no-setup', faults={latest: [('nolocation',)] * 4})
        f = []
        check(obs['results'][0]['exit'] != 0 and 'latest release' in obs['results'][0]['stderr']
              and 'NANOCODEX_RELEASE_TAG' in obs['results'][0]['stderr'], 'unclear latest-release diagnostic', f)
        check(tagged not in paths(obs), 'continued without a release', f)
        evaluate(obs, f)

        # A public script cut short must never run a prefix and must not report success.
        lines = installer.splitlines(keepends=True)
        cuts = {'quarter': len(b''.join(lines[:len(lines) // 4])), 'half': len(b''.join(lines[:len(lines) // 2])),
                'before-last-line': len(installer) - len(lines[-1])}
        for label, cut in cuts.items():
            for shell in ('sh', 'bash'):
                obs = run_case(f'truncated-public-{label}-{shell}', flags='--no-setup', shell=shell,
                               faults={public: [('truncate', cut)]})
                f = []
                check(obs['results'][0]['exit'] != 0, 'truncated script reported success', f)
                check(paths(obs) == offline + [public], f'truncated script made requests {paths(obs)}', f)
                check(not obs['launches'], 'truncated script ran the bootstrap', f)
                evaluate(obs, f)

        # Two simultaneous installs into one directory: the second waits, both succeed.
        obs = run_case('concurrent-runs-serialize', flags='--no-setup', concurrent=2, env_extra={'FIXTURE_HOLD': '3'})
        f = []
        check([r['exit'] for r in obs['results']] == [0, 0], 'a concurrent install failed: ' +
              ' | '.join(r['stderr'][-300:] for r in obs['results']), f)
        check(len(obs['launches']) == 2 and all(l['overlap'] == 'no' for l in obs['launches']),
              f'native installs overlapped {obs["launches"]}', f)
        check(any('waiting' in r['stderr'] for r in obs['results']), 'second run did not explain the wait', f)
        evaluate(obs, f)

        killed = {}

        def killed_holder(case_root, store, env, command):
            # SIGKILL a whole install while its bootstrap holds the lock.
            holder = subprocess.Popen(['/bin/bash', '-c', command], cwd=case_root, env=dict(env, FIXTURE_HOLD='60'),
                                      stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                      start_new_session=True)
            record = case_root / 'record.jsonl'
            deadline = time.monotonic() + 30
            while not record.exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            os.killpg(holder.pid, 9)
            holder.wait()
            killed['launched'] = record.exists()
            record.unlink(missing_ok=True)
            (store / 'fixture-running').unlink(missing_ok=True)
            killed['lock_file'] = (store / 'bootstrap.lock').exists()
            # SIGKILL skips the holder's EXIT trap; its own temporary directory is expected.
            killed['holder_temporary_dirs'] = [str(d) for d in case_root.glob('nanocodex-bootstrap.*')]
            for directory in case_root.glob('nanocodex-bootstrap.*'):
                shutil.rmtree(directory)
        obs = run_case('killed-holder-leaves-no-stale-lock', flags='--no-setup', prepare=killed_holder)
        f = []
        check(killed.get('launched'), 'holder never reached the bootstrap', f)
        check(obs['results'][0]['exit'] == 0 and len(obs['launches']) == 1, 'install after a killed holder failed', f)
        check('waiting' not in obs['results'][0]['stderr'] and obs['seconds'] < 5, 'waited on a dead holder', f)
        obs['killed_holder'] = killed
        evaluate(obs, f)

        nightly_api = '/repos/gakonst/nanocodex/releases/tags/nightly'
        obs = run_case('nightly-rate-limited', flags='--nightly', faults={nightly_api: [('status', 403)]})
        f = []
        check(obs['results'][0]['exit'] != 0 and 'rate limit' in obs['results'][0]['stderr']
              and 'NANOCODEX_RELEASE_TAG' in obs['results'][0]['stderr'], 'rate limit not explained', f)
        check(paths(obs).count(nightly_api) == 1 and not obs['launches'], 'retried 403 or continued', f)
        evaluate(obs, f)

        broken = files()
        broken[nightly_api] = json.dumps({'tag_name': 'nightly', 'target_commitish': ''}).encode()
        obs = run_case('nightly-metadata-without-commit', flags='--nightly', files_override=broken)
        f = []
        check(obs['results'][0]['exit'] != 0 and '40-hex commit' in obs['results'][0]['stderr'], 'metadata problem not explained', f)
        check(not obs['launches'], 'continued without a nightly commit', f)
        evaluate(obs, f)

        held = {}

        def background_update(case_root, store, env, command):
            store.mkdir(parents=True)
            held['file'] = open(store / 'update.lock', 'w')
            fcntl.flock(held['file'], fcntl.LOCK_EX)
            threading.Timer(2.5, lambda: held['file'].close()).start()
        obs = run_case('waits-for-background-update', flags='--no-setup', prepare=background_update)
        f = []
        check(obs['results'][0]['exit'] == 0 and len(obs['launches']) == 1, 'install failed while update.lock was held', f)
        check('background Nanocodex update' in obs['results'][0]['stderr'], 'wait was silent', f)
        check(obs['seconds'] >= 2, 'did not wait for update.lock', f)
        evaluate(obs, f)

        # /run/lock is a world-writable noexec tmpfs on systemd Linux hosts.
        if os.access('/run/lock', os.W_OK) and 'noexec' in Path('/proc/mounts').read_text().split('/run/lock', 1)[-1].split('\n', 1)[0]:
            obs = run_case('noexec-tmpdir-falls-back', flags='--no-setup', env_extra={'TMPDIR': '/run/lock'})
            f = []
            check(obs['results'][0]['exit'] == 0 and len(obs['launches']) == 1,
                  'noexec TMPDIR broke the install: ' + obs['results'][0]['stderr'][-400:], f)
            check(not list(Path('/run/lock').glob('nanocodex-bootstrap.*')), 'left files in /run/lock', f)
            evaluate(obs, f)
        else:
            summary['skipped'] = ['noexec-tmpdir-falls-back: no writable noexec /run/lock']

        # A flock without util-linux options (BusyBox) must not block installation.
        fake = root / 'busybox-flock'
        fake.mkdir()
        (fake / 'flock').write_text('#!/bin/sh\necho "BusyBox flock: unrecognized option" >&2\nexit 1\n')
        (fake / 'flock').chmod(0o755)
        obs = run_case('limited-flock-falls-back', flags='--no-setup', env_extra={'PATH': str(fake) + ':/usr/bin:/bin'})
        f = []
        check(obs['results'][0]['exit'] == 0 and len(obs['launches']) == 1,
              'limited flock blocked the install: ' + obs['results'][0]['stderr'][-300:], f)
        evaluate(obs, f)

        obs = run_case('old-glibc-explained', flags='--no-setup',
                       env_extra={'FIXTURE_LOADER_ERROR': "nanocodex: /lib/x86_64-linux-gnu/libm.so.6: version 'GLIBC_2.35' not found (required by nanocodex)"})
        f = []
        check(obs['results'][0]['exit'] != 0 and 'needs glibc 2.35' in obs['results'][0]['stderr'], 'loader failure not explained', f)
        check(not obs['launches'], 'install continued after loader failure', f)
        evaluate(obs, f)

    server.shutdown()
    server.server_close()
    summary['passed'] = sum(c['passed'] for c in summary['cases'])
    summary['failed'] = [c['case'] for c in summary['cases'] if not c['passed']]
    summary['verdict'] = 'PASSED' if not summary['failed'] else 'FAILED'
    (output / 'transcript.json').write_text(json.dumps(transcript, indent=2))
    (output / 'summary.json').write_text(json.dumps(summary, indent=2))
    print(json.dumps({'verdict': summary['verdict'], 'passed': summary['passed'], 'failed': summary['failed'],
                      'evidence': str(output)}), flush=True)
    raise SystemExit(0 if summary['verdict'] == 'PASSED' else 1)


if __name__ == '__main__':
    main()
