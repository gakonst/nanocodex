#!/usr/bin/env python3
"""Real Linux CLI install journey; external HTTPS release server is the only fixture.

python3 bin/nanocodex/tests/install_network_e2e.py CLI HAND [OUTPUT_DIR]
Requires Python 3, OpenSSL and strip. Uses process-local CA trust and a loopback
CONNECT proxy that rejects every destination except the two GitHub fixture hosts.
Never invokes setup, enables automatic updates, or requests a Hand restart.
The voice archive is structural test data, not a working voice runtime.
"""
import argparse
import gzip
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import platform
import shutil
import ssl
import subprocess
import tarfile
import tempfile
import threading
import time
from urllib.parse import urlsplit

CLI = 'nanocodex-x86_64-unknown-linux-gnu'
HAND = 'nanocodex2-x86_64-unknown-linux-gnu'
VOICE = 'nanocodex-voice-x86_64-unknown-linux-gnu.tar.gz'


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def check(value, message):
    if not value:
        raise AssertionError(message)


class QuietHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass


class Origin(QuietHandler):
    def do_GET(self):
        fixture = self.server.fixture
        name = Path(urlsplit(self.path).path).name
        event = {'case': fixture['case'], 'path': self.path,
                 'host': self.headers.get('Host'), 'start': time.monotonic()}
        self.server.events.append(event)
        if self.path == '/repos/gakonst/nanocodex/releases/latest':
            payload = json.dumps(fixture['release']).encode()
        elif name == 'SHA256SUMS':
            payload = fixture['manifest']
        else:
            payload = fixture['payloads'].get(name)
        if payload is None:
            self.send_error(404)
            event['error'] = 'unexpected request'
            return
        size = payload.stat().st_size if isinstance(payload, Path) else len(payload)
        self.send_response(200)
        self.send_header('Content-Length', str(size))
        self.send_header('Content-Type', 'application/octet-stream')
        self.end_headers()
        stream = payload.open('rb') if isinstance(payload, Path) else io.BytesIO(payload)
        try:
            with stream:
                self.wfile.write(stream.read(1))
                self.wfile.flush()
                event['first_byte'] = time.monotonic()
                # Hold both transfers open until both have actually sent bytes.
                # A sequential installer times out here and fails the assertion.
                if fixture.get('barrier') and name in (HAND + '.gz', VOICE):
                    fixture['barrier'].wait(timeout=15)
                shutil.copyfileobj(stream, self.wfile, 256 * 1024)
                self.wfile.flush()
            event['end'] = time.monotonic()
            event['bytes'] = size
        except (BrokenPipeError, ConnectionResetError, ssl.SSLError) as error:
            event['cancelled'] = str(error)
        except threading.BrokenBarrierError:
            event['error'] = 'Hand and voice transfers did not overlap'


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
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('cli', type=Path)
    parser.add_argument('hand', type=Path)
    parser.add_argument('output', nargs='?', type=Path, default=Path('output/install-network-e2e'))
    args = parser.parse_args()
    check(platform.system() == 'Linux' and platform.machine() == 'x86_64',
          'This runner requires Linux x86_64')
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    transcript = []
    summary = {'verdict': 'FAILED', 'command': ['python3', __file__, str(args.cli), str(args.hand), str(args.output)],
               'cases': [], 'limitations': ['Synthetic structural voice archive; voice execution is not tested.',
                                          'Existing Linux Hand owners are staged without restart.']}
    server = None
    try:
        with tempfile.TemporaryDirectory(prefix='install-network-', dir=args.output) as temporary:
            root = Path(temporary)
            cli = root / 'nanocodex'
            # strip -o preserves the supplied debug binary and avoids a huge copy.
            subprocess.run(['strip', '-o', str(cli), str(args.cli.resolve())], check=True)
            cli.chmod(0o700)
            hand = root / 'nanocodex2'
            subprocess.run(['strip', '-o', str(hand), str(args.hand.resolve())], check=True)
            hand.chmod(0o700)
            check(cli.stat().st_size < 256 * 1024 * 1024, 'stripped CLI exceeds release limit')
            check(hand.stat().st_size < 256 * 1024 * 1024, 'Hand exceeds release limit')
            cli_hash, hand_hash = digest(cli), digest(hand)
            summary['binaries'] = {'cli': str(args.cli.resolve()), 'cli_sha256_after_strip': cli_hash,
                                   'hand': str(args.hand.resolve()), 'hand_sha256_after_strip': hand_hash}
            packed = {}
            for name, source in [(CLI, cli), (HAND, hand)]:
                target = root / (name + '.gz')
                with source.open('rb') as src, gzip.open(target, 'wb', compresslevel=1) as dst:
                    shutil.copyfileobj(src, dst)
                packed[name + '.gz'] = target
            voice = root / VOICE
            with tarfile.open(voice, 'w:gz') as archive:
                for name in ['bin/nanocodex-voice-host', 'runtime.json', 'NOTICE.md', 'sources.json',
                             'manifest.json', 'libwebrtc.json', 'licenses/test.txt']:
                    body = b'synthetic voice installation fixture\n'
                    entry = tarfile.TarInfo('nanocodex-resources/voice/' + name)
                    entry.size, entry.mode, entry.mtime = len(body), 0o755, 0
                    archive.addfile(entry, io.BytesIO(body))
            packed[VOICE] = voice
            hashes = {name: digest(path) for name, path in packed.items()}
            hashes.update({CLI: cli_hash, HAND: hand_hash})
            cert = root / 'ca.pem'
            commands = [
                ['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
                 '-subj', '/CN=Nanocodex synthetic E2E CA', '-keyout', str(root / 'ca.key'), '-out', str(cert)],
                ['openssl', 'req', '-newkey', 'rsa:2048', '-nodes', '-subj', '/CN=github.com',
                 '-keyout', str(root / 'leaf.key'), '-out', str(root / 'leaf.csr')],
                ['openssl', 'x509', '-req', '-in', str(root / 'leaf.csr'), '-CA', str(cert),
                 '-CAkey', str(root / 'ca.key'), '-CAcreateserial', '-days', '1',
                 '-extfile', str(root / 'leaf.ext'), '-out', str(root / 'leaf.pem')],
            ]
            (root / 'leaf.ext').write_text('basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:github.com,DNS:api.github.com\n')
            for command in commands:
                subprocess.run(command, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
            server.events = []
            server.tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            server.tls.load_cert_chain(root / 'leaf.pem', root / 'leaf.key')
            threading.Thread(target=server.serve_forever, daemon=True).start()
            home, store = root / 'home', root / 'install'
            home.mkdir()
            store.mkdir()
            (store / 'automatic-updates-disabled').write_text('')
            account = home / 'synthetic-account.json'
            account.write_text('{"fixture":true,"access_token":"synthetic-not-a-real-credential"}')
            account.chmod(0o600)
            original_account = account.read_bytes()
            proxy = 'http://127.0.0.1:' + str(server.server_port)
            env = {'PATH': '/usr/bin:/bin', 'HOME': str(home), 'USERPROFILE': str(home),
                   'XDG_CONFIG_HOME': str(home / '.config'), 'NANOCODEX_DIR': str(store),
                   'NANOCODEX_ACCOUNT_FILE': str(account), 'NO_COLOR': '1',
                   'SSL_CERT_FILE': str(cert), 'SSL_CERT_DIR': str(root / 'no-system-certs'),
                   'HTTPS_PROXY': proxy, 'HTTP_PROXY': proxy, 'ALL_PROXY': proxy,
                   'https_proxy': proxy, 'http_proxy': proxy, 'all_proxy': proxy,
                   'NO_PROXY': '', 'no_proxy': ''}

            def run(command):
                result = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True, timeout=120)
                transcript.append({'command': list(map(str, command)), 'exit': result.returncode,
                                   'stdout': result.stdout, 'stderr': result.stderr})
                (args.output / 'transcript.json').write_text(json.dumps(transcript, indent=2))
                return result

            def state():
                return {name: (os.readlink(store / name) if (store / name).is_symlink()
                               else (store / name).read_text() if (store / name).is_file() else None)
                        for name in ('current', 'pending-update')}

            for label, binary in [('cli', cli), ('hand', hand)]:
                version = run([str(binary), '--version'])
                check(version.returncode == 0, version.stderr)
                summary['binaries'][label + '_version'] = version.stdout.strip()
            before_status = run([str(cli), 'hand', 'status'])
            check(before_status.returncode == 0, before_status.stderr)
            owner = json.loads(before_status.stdout)
            summary['hand_owner_before'] = owner
            has_owner = owner['installed'] or owner['loaded']
            for index, case in enumerate(['reuse-overlap', 'raw-checksum-absent', 'raw-checksum-mismatch',
                                          'missing-hand-checksum', 'missing-voice-checksum', 'corrupt-hand', 'corrupt-voice']):
                key = '99.0.' + str(index + 1)
                manifest = dict(hashes)
                payloads = dict(packed)
                if case == 'raw-checksum-absent':
                    del manifest[CLI]
                if case == 'raw-checksum-mismatch':
                    manifest[CLI] = '0' * 64
                if case == 'missing-hand-checksum':
                    del manifest[HAND + '.gz']
                if case == 'missing-voice-checksum':
                    del manifest[VOICE]
                if case == 'corrupt-hand':
                    payloads[HAND + '.gz'] = b'corrupted synthetic hand payload'
                if case == 'corrupt-voice':
                    payloads[VOICE] = b'corrupted synthetic voice payload'
                server.fixture = {'case': case, 'payloads': payloads,
                                  'manifest': ''.join(f'{sha}  {name}\n' for name, sha in manifest.items()).encode(),
                                  'barrier': threading.Barrier(2) if case == 'reuse-overlap' else None,
                                  'release': {'tag_name': 'v' + key, 'target_commitish': 'synthetic-release',
                                              'assets': [{'id': n + 1, 'name': name,
                                                          'browser_download_url': 'https://github.com/gakonst/nanocodex/releases/download/v' + key + '/' + name}
                                                         for n, name in enumerate(['SHA256SUMS', CLI, HAND, *packed])]}}
                before = state()
                start_event = len(server.events)
                result = run([str(cli), 'install', '--no-setup', '--no-modify-path'])
                events = server.events[start_event:]
                names = [Path(urlsplit(e.get('path', '')).path).name for e in events]
                good = index < 3
                observation = {'case': case, 'expected': 'verified install/staging' if good else 'checksum rejection without activation/staging',
                               'exit': result.returncode, 'before': before, 'after': state(), 'requests': names}
                summary['cases'].append(observation)
                check((result.returncode == 0) == good, f'{case}: unexpected exit {result.returncode}: {result.stderr}')
                check('latest' in names and 'SHA256SUMS' in names, f'{case}: metadata/manifest not requested')
                check(not any(e.get('blocked_connect') or e.get('tls_error') or e.get('error') for e in events),
                      f'{case}: proxy/TLS/unexpected request failure: {events}')
                check(CLI not in names, f'{case}: fetched uncompressed CLI unexpectedly')
                if case in ('raw-checksum-absent', 'raw-checksum-mismatch'):
                    check(names.count(CLI + '.gz') == 1, f'{case}: must fetch CLI when raw checksum cannot verify it')
                else:
                    check(CLI + '.gz' not in names, f'{case}: downloaded CLI despite exact raw digest')
                candidate = store / 'versions' / key
                if good:
                    check(digest(candidate / 'nanocodex') == cli_hash, 'installed CLI bytes differ')
                    check(digest(candidate / 'nanocodex2') == hand_hash, 'installed Hand bytes differ')
                    check((candidate / 'nanocodex-voice.archive.sha256').read_text().strip() == hashes[VOICE], 'voice digest missing')
                    check((candidate / 'nanocodex-resources/voice/runtime.json').is_file(), 'voice not extracted')
                    if has_owner:
                        check(state()['pending-update'].strip() == key, 'existing Hand owner requires staging')
                        if before['current'] is not None:
                            check(state()['current'] == before['current'], 'existing Hand owner active CLI changed')
                    else:
                        check(Path(state()['current']).name == key, 'complete release not activated')
                    if case == 'reuse-overlap':
                        transfers = [next(e for e in events if Path(urlsplit(e.get('path', '')).path).name == n)
                                     for n in (HAND + '.gz', VOICE)]
                        overlap = min(e['end'] for e in transfers) - max(e['first_byte'] for e in transfers)
                        check(overlap > 0, 'Hand and voice transfers did not overlap')
                        observation['payload_overlap_seconds'] = overlap
                else:
                    diagnostic = result.stderr.lower()
                    check('checksum' in diagnostic or 'sha256sums' in diagnostic,
                          f'{case}: missing checksum/manifest diagnostic')
                    check(state() == before, f'{case}: active or pending changed on failure')
                    check(not candidate.exists(), f'{case}: corrupt candidate persisted')
                    if case.startswith('missing-'):
                        absent = HAND + '.gz' if case == 'missing-hand-checksum' else VOICE
                        check(absent not in names, f'{case}: payload requested before checksum validation')
                    else:
                        corrupted = HAND + '.gz' if case == 'corrupt-hand' else VOICE
                        check(names.count(corrupted) == 1, f'{case}: checksum failure retried transfer')
                        check(corrupted in result.stderr, f'{case}: error does not identify corrupted asset')
                check(account.read_bytes() == original_account, 'synthetic account changed')
                check((store / 'automatic-updates-disabled').is_file(), 'automatic update opt-out changed')
                check(not any(home.glob('.*rc')) and not (home / '.profile').exists(), 'shell profile changed')
                print(json.dumps(observation), flush=True)
                # Discard large completed bundles once no pointer refers to them.
                for version in (store / 'versions').iterdir():
                    if version.name not in (Path(state()['current']).name, (state()['pending-update'] or '').strip()):
                        shutil.rmtree(version)
            after_status = run([str(cli), 'hand', 'status'])
            check(after_status.returncode == 0, after_status.stderr)
            check(json.loads(after_status.stdout) == owner, 'native Hand service changed')
            summary['hand_owner_after'] = json.loads(after_status.stdout)
            summary['verdict'] = 'PASSED'
    except Exception as error:
        summary['error'] = repr(error)
        raise
    finally:
        if server:
            server.shutdown()
            server.server_close()
            (args.output / 'https-trace.json').write_text(json.dumps(server.events, indent=2))
        (args.output / 'summary.json').write_text(json.dumps(summary, indent=2))
        print(json.dumps({'verdict': summary['verdict'], 'evidence': str(args.output)}), flush=True)


if __name__ == '__main__':
    main()
