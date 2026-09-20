"""Headless managed WebSocket backend. Install websockets>=15,<16.

No credential values, upstream exception strings, or frames are logged.
"""
import argparse
import hashlib
import json
import logging
import os
from pathlib import Path
import random
import sqlite3
import stat
import threading
import time

from server import Backend, APIError, identifier, stable_id, prompt_for, make_server


class ProtocolError(Exception):
    pass


def cursor(value):
    if (not isinstance(value, str) or not value.isascii() or not value.isdecimal()
            or str(int(value)) != value or int(value) > 9223372036854775807):
        raise ProtocolError('Invalid event cursor.')
    return int(value)


class EventStore:
    """One private SQLite database per credential/origin, shared across threads."""
    def __init__(self, path):
        path = Path(path)
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        info = path.parent.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
            raise APIError('Durable storage directory must be private (0700).', 503)
        fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
        try:
            info = os.fstat(fd)
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600 or info.st_nlink != 1:
                raise APIError('Durable storage file must be private (0600).', 503)
        finally:
            os.close(fd)
        self.lock = threading.RLock()
        self.db = sqlite3.connect(path, check_same_thread=False)
        self.db.execute('PRAGMA synchronous=FULL')
        self.db.executescript('''
            CREATE TABLE IF NOT EXISTS subscriptions(thread TEXT PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS events(thread TEXT, cursor INTEGER, body TEXT NOT NULL,
                PRIMARY KEY(thread,cursor));
            CREATE TABLE IF NOT EXISTS positions(thread TEXT PRIMARY KEY, cursor INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS outbox(thread TEXT, id TEXT, body TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'pending', PRIMARY KEY(thread,id));
        ''')

    def position(self, thread):
        with self.lock:
            row = self.db.execute('SELECT cursor FROM positions WHERE thread=?', (thread,)).fetchone()
            return str(row[0]) if row else '0'

    def enqueue(self, thread, turn, text):
        body = json.dumps({'type': 'prompt', 'id': turn, 'input': text}, ensure_ascii=False)
        with self.lock, self.db:
            row = self.db.execute('SELECT body FROM outbox WHERE thread=? AND id=?', (thread, turn)).fetchone()
            if row and row[0] != body:
                raise APIError('Idempotency key already has different input.', 409)
            self.db.execute('INSERT OR IGNORE INTO outbox(thread,id,body) VALUES(?,?,?)', (thread, turn, body))

    def pending(self, thread):
        with self.lock:
            return self.db.execute("SELECT id,body FROM outbox WHERE thread=? AND state='pending' ORDER BY rowid", (thread,)).fetchall()

    def accept(self, thread, message):
        """Commit event and cursor together; receipts can acknowledge without a cursor."""
        kind = message.get('type')
        value = message.get('cursor')
        number = cursor(value) if value is not None else None
        with self.lock, self.db:
            if kind in ('turn_accepted', 'turn_completed', 'turn_failed', 'turn_cancelled', 'turn_cancelling', 'turn_retryable'):
                self.db.execute("UPDATE outbox SET state='acknowledged' WHERE thread=? AND id=?", (thread, message.get('id')))
            if number is not None and number > int(self.position(thread)):
                self.db.execute('INSERT INTO events VALUES(?,?,?)', (thread, number, json.dumps(message)))
                self.db.execute('INSERT INTO positions VALUES(?,?) ON CONFLICT(thread) DO UPDATE SET cursor=excluded.cursor', (thread, number))

    def events(self, thread, after='0', limit=256):
        boundary = cursor(after)
        if type(limit) is not int or not 1 <= limit <= 256:
            raise APIError('Event limit must be 1 to 256.')
        with self.lock:
            return [json.loads(row[0]) for row in self.db.execute('SELECT body FROM events WHERE thread=? AND cursor>? ORDER BY cursor LIMIT ?', (thread, boundary, limit))]

    def close(self):
        with self.lock:
            self.db.close()


class DurableClient:
    def __init__(self, backend, store, thread, account_fingerprint, *, idle_interval=15):
        self.backend, self.store = backend, store
        self.thread = identifier(thread)
        self.fingerprint = account_fingerprint
        self.idle_interval = idle_interval
        self.stop_event = threading.Event()
        self.status = 'stopped'
        self.worker = None
        self.socket = None

    @staticmethod
    def account_id(backend, key):
        return hashlib.sha256((backend.base_url + '\0' + key).encode()).hexdigest()

    def start(self):
        if self.worker is None:
            self.worker = threading.Thread(target=self.run, daemon=True)
            self.worker.start()

    def close(self):
        self.stop_event.set()
        if self.socket is not None:
            self.socket.close()
        if self.worker is not None:
            self.worker.join(timeout=40)

    def session(self):
        # Lazy import lets status checks work without the optional transport package.
        from websockets.sync.client import connect
        key = self.backend.credentials()
        if self.account_id(self.backend, key) != self.fingerprint:
            raise APIError('Account changed; restart the durable backend.', 401)
        url = self.backend.base_url.replace('https://', 'wss://', 1).replace('http://', 'ws://', 1)
        url += '/v1/agents/' + self.thread + '/ws?cursor=' + self.store.position(self.thread)
        # A private logger prevents even application-wide DEBUG from exposing headers.
        logger = logging.Logger('nanocodex-wow-private', level=logging.CRITICAL + 1)
        with connect(url, additional_headers={'Authorization': 'Bearer ' + key},
                     proxy=None, logger=logger, user_agent_header='Nanocodex-WoW/0.1', compression=None, max_size=4 * 1024 * 1024,
                     open_timeout=15, close_timeout=2, ping_interval=15, ping_timeout=15) as ws:
            self.socket = ws
            self.status = 'replaying'
            ready = False
            watermark = 0
            sent = set()
            opened_at = last_check = time.monotonic()
            while not self.stop_event.is_set():
                if not ready and time.monotonic() - opened_at > 15:
                    raise ConnectionError('Ready frame timeout.')
                if ready and int(self.store.position(self.thread)) >= watermark:
                    self.status = 'connected'
                    for turn, body in self.store.pending(self.thread):
                        if turn not in sent:
                            ws.send(body)
                            sent.add(turn)
                try:
                    raw = ws.recv(timeout=0.25)
                except TimeoutError:
                    raw = None
                if raw is not None:
                    if not isinstance(raw, str):
                        raise ProtocolError('Expected text frame.')
                    try:
                        message = json.loads(raw)
                    except ValueError:
                        raise ProtocolError('Invalid JSON frame.') from None
                    if not isinstance(message, dict) or not isinstance(message.get('type'), str):
                        raise ProtocolError('Invalid server frame.')
                    kind = message['type']
                    if kind == 'ready':
                        if ready:
                            raise ProtocolError('Duplicate ready frame.')
                        watermark = cursor(message.get('latest_event_cursor'))
                        ready = True  # Snapshot is never a delivered cursor.
                    elif not ready:
                        raise ProtocolError('Missing ready frame.')
                    elif kind == 'error':
                        if message.get('code') == 'event_replay_failed':
                            raise ConnectionError('Replay failed.')
                        # Errors have no request correlation: stop, retain intent, require review.
                        raise ProtocolError('Server rejected a command; pending intent retained.')
                    elif kind not in ('status', 'pong'):
                        self.store.accept(self.thread, message)
                if time.monotonic() - last_check >= self.idle_interval:
                    last_check = time.monotonic()
                    state = self.backend.request('GET', '/v1/agents/' + self.thread)
                    if state.get('agent_id') != self.thread:
                        raise ProtocolError('State identity mismatch.')
                    if cursor(state.get('latest_event_cursor')) > int(self.store.position(self.thread)):
                        # Probe indicates missing delivery; replay from committed position only.
                        raise ConnectionError('Stream is behind durable state.')
            self.socket = None

    def run(self):
        delay = 1
        while not self.stop_event.is_set():
            started = time.monotonic()
            try:
                self.session()
            except (ProtocolError, ImportError):
                self.status = 'blocked'
                return
            except APIError as exc:
                if exc.status in (400, 401, 403, 404, 409):
                    self.status = 'blocked'
                    return
            except Exception as exc:
                # Do not surface exception text: handshake exceptions contain headers.
                response = getattr(exc, 'response', None)
                if getattr(response, 'status_code', None) in (400, 401, 403, 404, 409):
                    self.status = 'blocked'
                    return
                if isinstance(exc, sqlite3.Error):
                    self.status = 'blocked'
                    return
            finally:
                self.socket = None
            if self.stop_event.is_set():
                break
            self.status = 'reconnecting'
            if time.monotonic() - started >= 30:
                delay = 1
            self.stop_event.wait(delay + random.uniform(0, delay / 4))
            delay = min(delay * 2, 30)
        self.status = 'stopped'


class DurableBackend(Backend):
    """Drop-in make_server backend; existing REST routes remain available."""
    def __init__(self, storage_dir=None):
        super().__init__()
        self.storage_dir = Path(storage_dir or Path.home() / '.local/share/nanocodex-wow/durable')
        self.lock = threading.RLock()
        self.store = None
        self.fingerprint = None
        self.clients = {}

    def credentials(self):
        # REST fallbacks (including history and stop) share the stream account
        # fence. Check before metadata access or HTTP, not after a REST mutation.
        key = super().credentials()
        if self.fingerprint is not None and DurableClient.account_id(self, key) != self.fingerprint:
            raise APIError('Account changed; restart the durable backend.', 401)
        return key

    def _open_store(self):
        key = self.credentials()
        fingerprint = DurableClient.account_id(self, key)
        with self.lock:
            if self.fingerprint is not None and fingerprint != self.fingerprint:
                raise APIError('Account changed; restart the durable backend.', 401)
            if self.store is None:
                self.store = EventStore(self.storage_dir / (fingerprint + '.sqlite3'))
                self.fingerprint = fingerprint

    def resume(self):
        self._open_store()
        with self.store.lock:
            threads = self.store.db.execute('SELECT thread FROM subscriptions UNION SELECT thread FROM outbox UNION SELECT thread FROM positions').fetchall()
        for (thread,) in threads:
            self.subscribe(thread)

    def subscribe(self, thread):
        thread = identifier(thread)
        self._open_store()
        with self.lock:
            with self.store.lock, self.store.db:
                self.store.db.execute('INSERT OR IGNORE INTO subscriptions VALUES(?)', (thread,))
            if thread not in self.clients:
                self.clients[thread] = DurableClient(self, self.store, thread, self.fingerprint)
                self.clients[thread].start()
            return self.clients[thread]

    def handle(self, method, path, query, data):
        if method == 'POST' and path == '/api/send':
            text = prompt_for(data)
            thread = identifier(data.get('thread_id'), False)
            if data.get('mode', 'agent') == 'agent':
                thread = thread or identifier(data.get('project_id'), False)
            if thread:
                turn = stable_id(data.get('idempotency_key'))
                client = self.subscribe(thread)
                if client.status == 'blocked':
                    raise APIError('Durable stream is blocked; restart after resolving connectivity.', 503)
                self.store.enqueue(thread, turn, text)
                return {'thread_id': thread, 'turn_id': turn, 'idempotency_key': turn,
                        'status': 'queued', 'transport': 'websocket'}
        result = super().handle(method, path, query, data)
        if method == 'GET' and path in ('/api/messages', '/api/thread'):
            self.subscribe(query['thread_id'][0])
        if method == 'POST' and path == '/api/send':
            self.subscribe(result['thread_id'])
        if method == 'GET' and path == '/api/status':
            result['durable_streams'] = {thread: client.status for thread, client in self.clients.items()}
        return result

    def close(self):
        for client in self.clients.values():
            client.close()
        if self.store is not None:
            self.store.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=17841)
    parser.add_argument('--thread', action='append', default=[])
    args = parser.parse_args()
    backend = DurableBackend()
    server = make_server(args.port, backend)
    try:
        try:
            backend.resume()
        except APIError as exc:
            if exc.status != 401:
                raise
            # Keep the safe status endpoint available before account provisioning.
        for thread in args.thread:
            backend.subscribe(thread)
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    except Exception:
        raise SystemExit('Durable client stopped; check safe status and local configuration.') from None
    finally:
        server.server_close()
        backend.close()


if __name__ == '__main__':
    main()
