#!/usr/bin/env python3
"""Shipped-CLI journey for GitHub release-metadata recovery over real HTTPS.

python3 bin/nanocodex/tests/update_metadata_recovery_e2e.py CLI [--hand HAND] [--output DIR]

Runs nanocodex install --no-setup --no-modify-path against a process-local CA
and a loopback CONNECT proxy that admits only the GitHub fixture hosts. The
release metadata endpoint replays scripted failures (5xx, 408, 429/403 with
Retry-After, exhausted quota, connection reset, 404, malformed JSON) before or
instead of a valid release. Every case uses a fresh synthetic HOME and
NANOCODEX_DIR with automatic updates disabled; it never runs setup, sudo or a
Hand restart. HAND defaults to a stripped copy of the installed Hand binary.
Exit 0 only when every case matches; evidence goes to DIR (ignored output/).
"""
import argparse
import gzip
import hashlib
import http.server
import json
import os
from pathlib import Path
import platform
import shutil
import ssl
import subprocess
import tempfile
import threading
import time
from urllib.parse import urlsplit

CLI = 'nanocodex-x86_64-unknown-linux-gnu'
HAND = 'nanocodex2-x86_64-unknown-linux-gnu'
GUEST = 'nanocodex-vm-guest-x86_64-unknown-linux-musl'
API = '/repos/gakonst/nanocodex/releases/'
NIGHTLY_SHA = 'c0ffee' + '1' * 34


def digest(data):
    return hashlib.sha256(data).hexdigest()


def file_digest(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def reply(status, headers=None, body=None):
    if body is None:
        body = json.dumps({'message': http.HTTPStatus(status).phrase,
                           'documentation_url': 'https://docs.github.com/rest'}).encode()
    return {'status': status, 'headers': headers or {}, 'body': body}


# name, NANOCODEX_RELEASE_TAG (None = latest), scripted metadata replies, expected success
def cases(now):
    quota = {'x-ratelimit-limit': '60', 'x-ratelimit-remaining': '0',
             'x-ratelimit-used': '60', 'x-ratelimit-reset': str(now + 3600)}
    return [
        ('latest-503-then-502', None, [reply(503), reply(502)], True),
        ('stable-pin-429-retry-after', 'v99.3.2', [reply(429, {'Retry-After': '1'})], True),
        ('stable-pin-connection-reset', 'v99.3.3', ['reset'], True),
        ('secondary-limit-403-retry-after', 'v99.3.4',
         [reply(403, {'Retry-After': '1', 'x-ratelimit-remaining': '42'},
                json.dumps({'message': 'You have exceeded a secondary rate limit.'}).encode())], True),
        ('nightly-full-sha-408', 'nightly-' + NIGHTLY_SHA, [reply(408)], True),
        ('quota-exhausted-403', 'v99.3.6',
         [reply(403, quota, json.dumps({'message': 'API rate limit exceeded for 127.0.0.1.'}).encode())] * 3, False),
        ('retry-after-beyond-budget', 'v99.3.7', [reply(429, {'Retry-After': '3600'})] * 3, False),
        ('not-found-pin', 'v99.9.9', [reply(404)] * 3, False),
        ('malformed-json', 'v99.3.9', [reply(200, {'Content-Type': 'application/json'}, b'{"tag_name": ')] * 3, False),
        ('persistent-503', 'v99.4.0', [reply(503)] * 10, False),
    ]


class QuietHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass


class Origin(QuietHandler):
    def do_GET(self):
        fixture = self.server.fixture
        path = urlsplit(self.path).path
        name = Path(path).name
        event = {'case': fixture['case'], 'path': path, 'at': time.monotonic()}
        self.server.events.append(event)
        if path == fixture['metadata_path']:
            event['metadata'] = True
            if fixture['script']:
                step = fixture['script'].pop(0)
                if step == 'reset':
                    event['reply'] = 'connection reset before response'
                    self.close_connection = True
                    return
                event['reply'] = step['status']
                self.send_response(step['status'])
                for key, value in step['headers'].items():
                    self.send_header(key, value)
                self.send_header('Content-Length', str(len(step['body'])))
                self.end_headers()
                self.wfile.write(step['body'])
                return
            payload = json.dumps(fixture['release']).encode()
            event['reply'] = 200
        elif path.startswith('/gakonst/nanocodex/releases/download/' + fixture['tag'] + '/'):
            payload = fixture['manifest'] if name == 'SHA256SUMS' else fixture['payloads'].get(name)
            if isinstance(payload, Path):
                payload = payload.read_bytes()
        else:
            payload = None
        if payload is None:
            event['error'] = 'unexpected request'
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        try:
            self.wfile.write(payload)
        except (BrokenPipeError, ConnectionResetError, ssl.SSLError) as error:
            event['cancelled'] = str(error)


class Proxy(QuietHandler):
    def do_CONNECT(self):
        if self.path not in ('api.github.com:443', 'github.com:443'):
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
            self.server.events.append({'tls_error': str(error)})


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('cli', type=Path)
    parser.add_argument('--hand', type=Path, default=Path('/opt/nanocodex/current/nanocodex2'))
    parser.add_argument('--output', type=Path, default=Path('output/update-metadata-recovery-e2e'))
    args = parser.parse_args()
    if platform.system() != 'Linux' or platform.machine() != 'x86_64':
        raise SystemExit('This runner requires Linux x86_64')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    transcript, results = [], []
    summary = {'command': ['python3', __file__, str(args.cli), '--hand', str(args.hand), '--output', str(output)],
               'cli': str(args.cli.resolve()), 'hand': str(args.hand.resolve()), 'cases': results}
    server = None
    try:
        with tempfile.TemporaryDirectory(prefix='metadata-recovery-', dir=output) as temporary:
            root = Path(temporary)
            cli, hand = root / 'nanocodex', root / 'nanocodex2'
            for source, target in ((args.cli, cli), (args.hand, hand)):
                # strip -o leaves the source binary (possibly the live Hand's) untouched.
                subprocess.run(['strip', '-o', str(target), str(source.resolve())], check=True)
                target.chmod(0o700)
            cli_hash, hand_hash = file_digest(cli), file_digest(hand)
            packed = {}
            for name, source in ((CLI, cli), (HAND, hand)):
                target = root / (name + '.gz')
                with source.open('rb') as src, gzip.open(target, 'wb', compresslevel=1) as dst:
                    shutil.copyfileobj(src, dst)
                packed[name + '.gz'] = target
            hashes = {name: file_digest(path) for name, path in packed.items()}
            hashes.update({CLI: cli_hash, HAND: hand_hash})
            guest = b'synthetic VM guest installation fixture\n'
            cert = root / 'ca.pem'
            (root / 'leaf.ext').write_text('basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:github.com,DNS:api.github.com\n')
            for command in (
                ['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
                 '-subj', '/CN=Nanocodex synthetic metadata E2E CA', '-keyout', str(root / 'ca.key'), '-out', str(cert)],
                ['openssl', 'req', '-newkey', 'rsa:2048', '-nodes', '-subj', '/CN=github.com',
                 '-keyout', str(root / 'leaf.key'), '-out', str(root / 'leaf.csr')],
                ['openssl', 'x509', '-req', '-in', str(root / 'leaf.csr'), '-CA', str(cert), '-CAkey', str(root / 'ca.key'),
                 '-CAcreateserial', '-days', '1', '-extfile', str(root / 'leaf.ext'), '-out', str(root / 'leaf.pem')],
            ):
                subprocess.run(command, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
            server.events = []
            server.tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            server.tls.load_cert_chain(root / 'leaf.pem', root / 'leaf.key')
            threading.Thread(target=server.serve_forever, daemon=True).start()
            proxy = 'http://127.0.0.1:' + str(server.server_port)
            base_env = {'PATH': '/usr/bin:/bin', 'NO_COLOR': '1',
                        'SSL_CERT_FILE': str(cert), 'SSL_CERT_DIR': str(root / 'no-system-certs'),
                        'HTTPS_PROXY': proxy, 'HTTP_PROXY': proxy, 'ALL_PROXY': proxy,
                        'https_proxy': proxy, 'http_proxy': proxy, 'all_proxy': proxy, 'NO_PROXY': '', 'no_proxy': ''}

            def run(command, env):
                started = time.monotonic()
                result = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True, timeout=180)
                transcript.append({'command': list(map(str, command)), 'env': {k: v for k, v in env.items()
                                   if k.startswith('NANOCODEX_')}, 'exit': result.returncode,
                                   'seconds': round(time.monotonic() - started, 3),
                                   'stdout': result.stdout, 'stderr': result.stderr})
                (output / 'transcript.json').write_text(json.dumps(transcript, indent=2))
                return result, time.monotonic() - started

            status_env = dict(base_env, HOME=str(root / 'status-home'))
            before_owner, _ = run([str(cli), 'hand', 'status'], status_env)
            summary['hand_owner_before'] = before_owner.stdout.strip()
            for name, tag, script, success in cases(int(time.time())):
                failures = []

                def check(value, message):
                    if not value:
                        failures.append(message)

                nightly = bool(tag and tag.startswith('nightly-'))
                release_tag = tag or 'v99.3.1'
                home, store = root / name / 'home', root / name / 'install'
                home.mkdir(parents=True)
                store.mkdir()
                (store / 'automatic-updates-disabled').write_text('')
                env = dict(base_env, HOME=str(home), USERPROFILE=str(home), XDG_CONFIG_HOME=str(home / '.config'),
                           NANOCODEX_DIR=str(store))
                if tag:
                    env['NANOCODEX_RELEASE_TAG'] = tag
                payloads = dict(packed)
                manifest = dict(hashes)
                if nightly:
                    payloads[GUEST] = guest
                    manifest[GUEST] = digest(guest)
                assets = ['SHA256SUMS', CLI + '.gz', HAND + '.gz', *([GUEST] if nightly else [])]
                metadata_path = API + ('tags/' + tag if tag else 'latest')
                server.fixture = {
                    'case': name, 'tag': release_tag, 'metadata_path': metadata_path, 'script': list(script),
                    'payloads': payloads,
                    'manifest': ''.join(f'{sha}  {asset}\n' for asset, sha in manifest.items()).encode(),
                    'release': {'tag_name': release_tag,
                                'target_commitish': NIGHTLY_SHA if nightly else 'synthetic-release',
                                'assets': [{'id': i + 1, 'name': asset,
                                            'browser_download_url': f'https://github.com/gakonst/nanocodex/releases/download/{release_tag}/{asset}'}
                                           for i, asset in enumerate(assets)]}}
                start = len(server.events)
                result, seconds = run([str(cli), 'install', '--no-setup', '--no-modify-path'], env)
                events = server.events[start:]
                metadata = [e for e in events if e.get('metadata')]
                other = [e for e in events if not e.get('metadata')]
                stderr = result.stderr
                versions = sorted(p.name for p in (store / 'versions').iterdir()) if (store / 'versions').is_dir() else []
                current = os.readlink(store / 'current') if (store / 'current').is_symlink() else None
                pending = (store / 'pending-update').read_text().strip() if (store / 'pending-update').is_file() else None
                check(not any(e.get('blocked_connect') or e.get('tls_error') or e.get('error') for e in events),
                      f'unexpected proxy/TLS/request failure: {events}')
                check(all(e['path'] == metadata_path for e in metadata), 'requested other metadata')
                check((result.returncode == 0) == success, f'exit {result.returncode}, expected success={success}')
                if success:
                    check(len(metadata) == len(script) + 1, f'{len(metadata)} metadata requests, expected {len(script) + 1}')
                    check('retrying 2/' in stderr, 'no retry diagnostic on stderr')
                    check(seconds < 60, f'recovery took {seconds:.1f}s')
                    if any(isinstance(step, dict) and step['headers'].get('Retry-After') == '1' for step in script):
                        gap = metadata[1]['at'] - metadata[0]['at'] if len(metadata) > 1 else None
                        check(gap is not None and gap >= 0.95, f'retry gap {gap} despite Retry-After: 1')
                    prefix = 'nightly-' + NIGHTLY_SHA + '-' if nightly else release_tag[1:]
                    installed = [v for v in versions if v == prefix or (nightly and v.startswith(prefix))]
                    check(len(installed) == 1, f'expected one installed bundle for {prefix}, saw {versions}')
                    if installed:
                        candidate = store / 'versions' / installed[0]
                        check((candidate / 'nanocodex').is_file() and file_digest(candidate / 'nanocodex') == cli_hash,
                              'installed CLI bytes differ')
                        check((candidate / 'nanocodex2').is_file() and file_digest(candidate / 'nanocodex2') == hand_hash,
                              'installed Hand bytes differ')
                        if nightly:
                            check((candidate / 'nanocodex-vm-guest').read_bytes() == guest, 'nightly guest bytes differ')
                        check(installed[0] in (Path(current or '').name, pending or ''),
                              f'bundle neither active nor staged: current={current} pending={pending}')
                    check(API + 'latest' not in [e['path'] for e in events] or tag is None, 'pinned install consulted latest')
                    check(API + 'tags/nightly' not in [e['path'] for e in events], 'consulted moving nightly pointer')
                else:
                    check(other == [], f'requested release assets after metadata failure: {[e["path"] for e in other]}')
                    # Only the running manager's own bootstrap copy may exist; no release bundle.
                    releases = [v for v in versions if not v.startswith('dev-')]
                    check(releases == [] and pending is None and (current is None or Path(current).name.startswith('dev-')),
                          f'release state changed on failure: versions={versions} current={current} pending={pending}')
                    check('Traceback' not in stderr and 'panicked' not in stderr, 'crash output')
                    if name == 'quota-exhausted-403':
                        check(len(metadata) == 1, f'quota exhaustion retried {len(metadata)} times')
                        check('rate limit' in stderr and 'resets at' in stderr, 'missing rate-limit reset diagnostic')
                        check(seconds < 15, f'quota failure took {seconds:.1f}s')
                    elif name == 'retry-after-beyond-budget':
                        check(len(metadata) == 1, f'over-budget Retry-After retried {len(metadata)} times')
                        check('60 minutes' in stderr and 'retry limit' in stderr, 'missing over-budget wait diagnostic')
                        check(seconds < 15, f'over-budget failure took {seconds:.1f}s')
                    elif name == 'not-found-pin':
                        check(len(metadata) == 1, f'404 retried {len(metadata)} times')
                        check('404' in stderr, 'missing HTTP 404 diagnostic')
                    elif name == 'malformed-json':
                        check(len(metadata) == 1, f'malformed metadata retried {len(metadata)} times')
                        check('invalid' in stderr, 'missing invalid-metadata diagnostic')
                    elif name == 'persistent-503':
                        check(len(metadata) == 5, f'{len(metadata)} attempts, expected bounded 5')
                        check('after 5 attempts' in stderr and '503' in stderr, 'missing bounded-attempt diagnostic')
                        check(seconds < 30, f'persistent failure took {seconds:.1f}s')
                observation = {'case': name, 'tag': tag, 'expected_success': success, 'exit': result.returncode,
                               'seconds': round(seconds, 3),
                               'metadata_replies': [e.get('reply') for e in metadata],
                               'asset_requests': [Path(e['path']).name for e in other],
                               'versions': versions, 'current': current, 'pending': pending,
                               'stderr_tail': stderr.strip().splitlines()[-3:],
                               'verdict': 'PASSED' if not failures else 'FAILED', 'failures': failures}
                results.append(observation)
                print(json.dumps(observation), flush=True)
                shutil.rmtree(root / name)
            after_owner, _ = run([str(cli), 'hand', 'status'], status_env)
            summary['hand_owner_after'] = after_owner.stdout.strip()
            summary['hand_owner_unchanged'] = summary['hand_owner_after'] == summary['hand_owner_before']
    except Exception as error:
        summary['error'] = repr(error)
        raise
    finally:
        if server:
            server.shutdown()
            server.server_close()
            (output / 'https-trace.json').write_text(json.dumps(server.events, indent=2))
        passed = results and all(r['verdict'] == 'PASSED' for r in results) and summary.get('hand_owner_unchanged')
        summary['verdict'] = 'PASSED' if passed and 'error' not in summary else 'FAILED'
        (output / 'summary.json').write_text(json.dumps(summary, indent=2))
        print(json.dumps({'verdict': summary['verdict'],
                          'failed_cases': [r['case'] for r in results if r['verdict'] != 'PASSED'],
                          'evidence': str(output)}), flush=True)
    raise SystemExit(0 if summary['verdict'] == 'PASSED' else 1)


if __name__ == '__main__':
    main()
