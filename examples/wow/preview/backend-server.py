#!/usr/bin/env python3
"""Serve the real addon preview and the private CLI-backed dispatcher on loopback.

Run: python3 examples/wow/preview/backend-server.py
Open http://127.0.0.1:17843. Never tunnel this listener to a public preview.
POST /api/addon/dispatch accepts {"payload": "<addon JSON>", "id": "stable-id"}.
GET /api/addon/poll returns {"outputs": [...]} in request/event order. Optional
repeated ?id= parameters select previously journaled requests, including completed
requests after restart. Without parameters it polls this process's requests and
unfinished journal entries. Outputs replay: the browser MUST deduplicate event_id
before calling PreviewReceive(kind, value). Keep the full kind names (ack,
reply, projects, error, stream); the browser adapter does not use wire letters.
GET /api/addon/status returns only an actual account-connectivity boolean.

The dispatcher and durable event journals remain private and account/origin scoped.
No startup resume, subscriptions, or sends: status is safe before user dispatch.
Credentials are consumed exclusively by the existing DurableBackend auth methods.
"""
import argparse
import json
import mimetypes
from pathlib import Path
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlsplit

WOW_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(WOW_ROOT))
from durable_client import DurableBackend  # noqa: E402
from server import APIError, stable_id  # noqa: E402
from transport.dispatch import Dispatcher, MAX_REQUEST, parse_request, InvalidRequest  # noqa: E402

# JSON escaping can expand a serialized 16 KiB payload to six times its size.
MAX_BODY = 6 * MAX_REQUEST + 1024
MAX_POLL_IDS = 1024


class Application:
    def __init__(self, backend):
        self.backend = backend
        self.dispatcher = None
        self.lock = threading.RLock()
        self.active = {}  # Insertion order is the request admission order.

    def status(self):
        try:
            return {'connected': self.backend.handle('GET', '/api/status', {}, {}).get('connected') is True}
        except Exception:
            return {'connected': False}

    def _dispatcher(self):
        # This existing backend method opens only local storage. Unlike resume(),
        # it cannot replay pending sends during startup or a read-only status probe.
        # Rechecking also applies DurableBackend's account-change fence on replay.
        self.backend._open_store()
        if self.dispatcher is None:
            journal = self.backend.storage_dir / (self.backend.fingerprint + '.preview-dispatch.sqlite3')
            self.dispatcher = Dispatcher(self.backend, journal)
            with self.dispatcher.lock:
                rows = self.dispatcher.journal.db.execute(
                    'SELECT id, record FROM dispatch_requests ORDER BY rowid').fetchall()
                for rid, record in rows:
                    if not json.loads(record)['done']:
                        self.active[rid] = None
        return self.dispatcher

    def dispatch(self, data):
        if not isinstance(data, dict) or set(data) != {'payload', 'id'}:
            raise APIError('Expected payload and id.')
        if not isinstance(data['payload'], str):
            raise APIError('payload must be serialized addon JSON.')
        # Validate before touching the CLI account store or opening the journal.
        try:
            rid, _, _, _ = parse_request(data['payload'], data['id'])
        except InvalidRequest:
            raise APIError('Invalid addon request or stable ID.') from None
        with self.lock:
            dispatcher = self._dispatcher()
            # Register before backend IO so polling can observe its eventual
            # receipt even while this HTTP request is still running.
            self.active[rid] = None
        outputs = dispatcher.dispatch(data['payload'], request_id=rid)
        return {'outputs': outputs}

    def poll(self, ids=None):
        if ids is not None:
            if len(ids) > MAX_POLL_IDS:
                raise APIError('Too many request IDs.', 413)
            ids = list(dict.fromkeys(stable_id(rid) for rid in ids))
        with self.lock:
            dispatcher = self._dispatcher()
            selected = list(self.active) if ids is None else ids
            # An HTTP dispatch can be registered just before journal admission.
            # That small window is still pending, not an unknown request error.
            with dispatcher.lock:
                admitted = {row[0] for row in dispatcher.journal.db.execute('SELECT id FROM dispatch_requests')}
            selected = [rid for rid in selected if rid not in self.active or rid in admitted]
        # Dispatcher.poll reads EventStore.events incrementally; it never
        # substitutes a full-answer history fetch for DurableBackend streams.
        outputs = []
        for rid in selected:
            outputs.extend(dispatcher.poll(rid))
        return {'outputs': outputs}

    def close(self):
        with self.lock:
            if self.dispatcher is not None:
                self.dispatcher.close()
            self.backend.close()


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('Duplicate field')
        result[key] = value
    return result


class Handler(BaseHTTPRequestHandler):
    server_version = 'WoWAddonPreview/1'

    def setup(self):
        super().setup()
        self.connection.settimeout(15)

    def log_message(self, *args):
        pass  # No prompts, request paths, account responses, or credentials in logs.

    def guard(self, write=False):
        host = self.headers.get('Host', '')
        port = self.server.server_address[1]
        if (len(self.headers.get_all('Host', [])) != 1
                or host not in {f'127.0.0.1:{port}', f'localhost:{port}'}):
            raise APIError('Loopback Host required.', 403)
        origins = self.headers.get_all('Origin', [])
        if len(origins) > 1 or (write and len(origins) != 1):
            raise APIError('Same-origin Origin required.', 403)
        if origins and origins[0] != 'http://' + host:
            raise APIError('Same-origin Origin required.', 403)
        if self.headers.get('Sec-Fetch-Site') not in (None, 'same-origin', 'none'):
            raise APIError('Cross-site requests are forbidden.', 403)

    def reply(self, status, data, content_type='application/json; charset=utf-8'):
        body = json.dumps(data, ensure_ascii=False).encode('utf-8') if content_type.startswith('application/json') else data
        self.send_response(status)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.send_header('Referrer-Policy', 'no-referrer')
        self.send_header('Cross-Origin-Resource-Policy', 'same-origin')
        self.send_header('X-Frame-Options', 'DENY')
        self.end_headers()
        self.wfile.write(body)

    def read_json(self):
        if self.headers.get('Transfer-Encoding') or len(self.headers.get_all('Content-Length', [])) != 1:
            raise APIError('A bounded Content-Length is required.', 411)
        try:
            length = int(self.headers['Content-Length'])
        except ValueError:
            raise APIError('Invalid Content-Length.') from None
        if not 0 < length <= MAX_BODY:
            raise APIError('Request body exceeds the limit.', 413)
        if self.headers.get_content_type() != 'application/json':
            raise APIError('Use application/json.', 415)
        raw = self.rfile.read(length)
        if len(raw) != length:
            raise APIError('Incomplete request.')
        try:
            return json.loads(raw, object_pairs_hook=_unique_object,
                              parse_constant=lambda _: (_ for _ in ()).throw(ValueError()))
        except (ValueError, UnicodeError, RecursionError):
            raise APIError('Invalid JSON.') from None

    def route(self, write=False):
        try:
            self.guard(write)
            parsed = urlsplit(self.path)
            if parsed.scheme or parsed.netloc or parsed.fragment:
                raise APIError('Invalid request target.')
            if write:
                if parsed.path != '/api/addon/dispatch' or parsed.query:
                    raise APIError('Not found.', 404)
                result = self.server.application.dispatch(self.read_json())
            elif parsed.path == '/api/addon/status' and not parsed.query:
                result = self.server.application.status()
            elif parsed.path == '/api/addon/poll':
                query = parse_qs(parsed.query, keep_blank_values=True, max_num_fields=MAX_POLL_IDS)
                if set(query) - {'id'}:
                    raise APIError('Unsupported poll query.')
                result = self.server.application.poll(query.get('id'))
            elif parsed.path.startswith('/api/'):
                raise APIError('Not found.', 404)
            else:
                root = self.server.public_dir
                path = (root / unquote(parsed.path).lstrip('/')).resolve()
                if path == root:
                    path = root / 'index.html'
                if not path.is_relative_to(root) or not path.is_file():
                    raise APIError('Not found.', 404)
                content_type = mimetypes.guess_type(path.name)[0] or 'application/octet-stream'
                self.reply(200, path.read_bytes(), content_type)
                return
            self.reply(200, result)
        except APIError as exc:
            # Existing APIError messages are deliberately safe projections.
            self.reply(exc.status, {'error': exc.message})
        except (ValueError, UnicodeError):
            self.reply(400, {'error': 'Invalid request.'})
        except Exception:
            self.reply(502, {'error': 'Outcome could not be confirmed. Reconcile the same request ID before resubmitting.'})

    def do_GET(self):
        self.route()

    def do_POST(self):
        self.route(True)

    def do_OPTIONS(self):
        self.reply(403, {'error': 'Cross-origin access is forbidden.'})


class PreviewServer(ThreadingHTTPServer):
    daemon_threads = False

    def server_close(self):
        super().server_close()
        self.application.close()


def make_server(port=17843, backend=None, public_dir=None):
    server = PreviewServer(('127.0.0.1', port), Handler)
    server.application = Application(backend if backend is not None else DurableBackend())
    server.public_dir = Path(public_dir or Path(__file__).resolve().parent / 'public').resolve()
    return server


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=17843)
    args = parser.parse_args()
    server = make_server(args.port)
    print(f'WoW addon preview: http://127.0.0.1:{server.server_address[1]}', flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == '__main__':
    main()
