#!/usr/bin/env python3
"""Gaming-user NC1 bridge. Run with python -m transport.daemon --help.

Parent supplies observed window identity, calibration and shared session. Requires
--allow-input to run; never focuses a window or changes privileges/bindings.
One backend worker handles Dispatcher IO while the main loop captures and pumps
local bursts. SQLite persists exact requests, session/application message IDs,
carrier state and output receipts. No credentials or message bodies enter evidence.
A crash during input is reconciled only by a stable peer ACK; otherwise stopped.
"""
import argparse
import fcntl
import hashlib
import json
import os
import stat
from pathlib import Path
import sys
import threading
import time

if __package__ in (None, ''):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from transport.driver import Pump
from transport.protocol import Frame, Link
from transport.wayland import Desktop
from transport.session import desktop_session
from transport.messages import Assembler
from durable_client import DurableBackend, EventStore
from server import stable_id, APIError
from transport.dispatch import Dispatcher, wire_fragments

MAX_REQUESTS = 1024
MAX_PENDING = 64
MAX_OUTPUTS = 4096


def _probe_legacy_owner(journal_path):
    """Fail before SQLite IO if an older daemon holds the database-file flock.

    Release the probe immediately: keeping it during SQLite writes deadlocks on
    macOS. The sidecar owns new daemons; upgrades still require stopping old ones.
    """
    try:
        fd = os.open(journal_path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        return
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600 or info.st_nlink != 1:
            raise APIError("Bridge journal must be private (0600).", 503)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise APIError("Legacy bridge owns this journal; stop it before upgrading.", 503) from None
    finally:
        os.close(fd)


class Bridge:
    def __init__(self, desktop, session, dispatcher, journal_path, *, clock=time.monotonic):
        Frame(session).encode()
        self.desktop, self.session, self.dispatcher, self.clock = desktop, session, dispatcher, clock
        _probe_legacy_owner(journal_path)
        self.store = EventStore(journal_path)
        # Keep process ownership separate from SQLite's database locks. On
        # macOS flock on the database itself conflicts with SQLite writes. The
        # private sidecar remains on disk to prevent unlink/recreate lock races.
        self.guard = None
        try:
            self.guard = os.open(str(journal_path) + ".lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
            info = os.fstat(self.guard)
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600 or info.st_nlink != 1:
                raise APIError("Bridge ownership lock must be private (0600).", 503)
            fcntl.flock(self.guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BaseException:
            if self.guard is not None:
                os.close(self.guard)
            self.store.close()
            raise
        self.lock = threading.RLock()
        self.stop = threading.Event()
        self.wake = threading.Event()
        self.worker = None
        self.error = None
        self.stats = {'captures': 0, 'input_batches': 0, 'accepted_requests': 0,
                      'delivered_outputs': 0, 'account_connected': False}
        with self.store.db:
            self.store.db.executescript('''
                CREATE TABLE IF NOT EXISTS bridge_requests (
                    session INTEGER, mid INTEGER, rid TEXT NOT NULL, body BLOB NOT NULL,
                    state TEXT NOT NULL, PRIMARY KEY(session,mid));
                CREATE TABLE IF NOT EXISTS bridge_outputs (
                    session INTEGER, event TEXT, body TEXT NOT NULL, delivered INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY(session,event));
                CREATE TABLE IF NOT EXISTS bridge_state (session INTEGER PRIMARY KEY, body TEXT NOT NULL);
            ''')
        self.assembler = Assembler(self._request, accepted_kinds=('Q',))
        self.link = Link(session, self.assembler.receive)
        self.active = None
        self.next_mid = 1
        self.uncertain = False
        self.uncertain_seq = None
        row = self.store.db.execute('SELECT body FROM bridge_state WHERE session=?', (session,)).fetchone()
        self.restored = row is not None
        if row:
            state = json.loads(row[0])
            for name in ('tx', 'rx'):
                setattr(self.link, name, state[name])
            for name in ('pending', 'last'):
                setattr(self.link, name, None if state[name] is None else bytes.fromhex(state[name]))
            self.assembler.last_id = state['last_id']
            current = state['current']
            self.assembler.current = None if current is None else (*current[:3], bytes.fromhex(current[3]))
            self.active, self.next_mid = state['active'], state['next_mid']
            self.uncertain = state['uncertain']
            self.uncertain_seq = state.get('uncertain_seq')
            self.error = state.get('error')
        self.pump = Pump(self.link, desktop.foreground, None, clock, desktop.reserved,
                         interval=0, send_keys=self._send, burst_size=890 if getattr(desktop, 'key_encoding', 'octal') == 'binary' else 335, encoding=getattr(desktop, 'key_encoding', 'octal'))
        self.last_capture = None
        self.started = clock()
        self.poll_cursor = 0

    def _checkpoint(self):
        current = self.assembler.current
        state = {name: getattr(self.link, name) for name in ('tx', 'rx')}
        state.update({name: None if getattr(self.link, name) is None else getattr(self.link, name).hex()
                      for name in ('pending', 'last')})
        state.update(last_id=self.assembler.last_id,
                     current=None if current is None else [*current[:3], current[3].hex()],
                     active=self.active, next_mid=self.next_mid, uncertain=self.uncertain,
                     uncertain_seq=self.uncertain_seq, error=self.error)
        with self.store.db:
            self.store.db.execute('INSERT INTO bridge_state VALUES(?,?) ON CONFLICT(session) DO UPDATE SET body=excluded.body',
                                  (self.session, json.dumps(state)))

    def _request(self, kind, body):
        # Called only with fully assembled bytes. No backend IO on capture thread.
        mid = self.assembler.last_id + 1
        rid = 'ncw:%08x:%04x' % (self.session, mid)
        try:
            value = json.loads(body)
            if isinstance(value, dict) and 'request_id' in value:
                rid = stable_id(value['request_id'])
        except (ValueError, UnicodeError, APIError):
            pass  # Dispatcher emits a bounded schema error using the retained bytes.
        with self.lock, self.store.db:
            row = self.store.db.execute('SELECT rid,body FROM bridge_requests WHERE session=? AND mid=?',
                                        (self.session, mid)).fetchone()
            if row:
                if row != (rid, body):
                    raise ValueError('application identity conflict')
                return True
            total = self.store.db.execute('SELECT count(*) FROM bridge_requests').fetchone()[0]
            pending = self.store.db.execute("SELECT count(*) FROM bridge_requests WHERE state!='done'").fetchone()[0]
            if total >= MAX_REQUESTS or pending >= MAX_PENDING:
                return False
            self.store.db.execute('INSERT INTO bridge_requests VALUES(?,?,?,?,?)',
                                  (self.session, mid, rid, body, 'queued'))
            self.stats['accepted_requests'] += 1
        self.wake.set()
        return True

    def _send(self, keys):
        # Persist potential input before invoking wtype. Never blindly retry on restart.
        with self.lock:
            self.uncertain = True
            self.uncertain_seq = self.link.tx if self.link.pending is not None else None
            self._checkpoint()
        confirmed = self.desktop.send_keys(keys)
        if confirmed is True:
            with self.lock:
                self.uncertain = False
                self.uncertain_seq = None
                self.stats['input_batches'] += 1
                self._checkpoint()
        return confirmed

    def _output_capacity_failure(self):
        # Delivered rows are replay receipts: deleting them would re-admit old
        # Dispatcher outputs. Retain identities and all carrier state for review.
        self.error = ('bridge output journal capacity reached; reconcile pending requests '
                      'and carrier delivery, then archive journals and start a new session; '
                      'do not delete delivered receipts or resubmit uncertain requests')
        self.stop.set()
        self._checkpoint()
        return False

    def _admit_outputs(self, outputs):
        """Called under lock/transaction; admit the whole batch or retain intent."""
        new = {}
        for output in outputs:
            list(wire_fragments(output, 1))
            event = output['event_id']
            if not self.store.db.execute(
                    'SELECT 1 FROM bridge_outputs WHERE session=? AND event=?',
                    (self.session, event)).fetchone():
                new.setdefault(event, json.dumps(output))
        total = self.store.db.execute('SELECT count(*) FROM bridge_outputs').fetchone()[0]
        if total + len(new) > MAX_OUTPUTS:
            return self._output_capacity_failure()
        self.store.db.executemany('INSERT INTO bridge_outputs VALUES(?,?,?,0)',
                                 ((self.session, event, body) for event, body in new.items()))
        return True

    def work_once(self):
        """One backend operation; called by the worker (directly in hardware-free tests)."""
        with self.lock:
            if self.error or self.stop.is_set():
                return False
            # Historical capacity cannot drain: fail visibly, preserving receipts.
            if self.store.db.execute('SELECT count(*) FROM bridge_outputs').fetchone()[0] >= MAX_OUTPUTS - 4:
                return self._output_capacity_failure()
            # Undelivered output backpressure can drain through the carrier.
            pending_outputs = self.store.db.execute('SELECT count(*) FROM bridge_outputs WHERE delivered=0').fetchone()[0]
            if pending_outputs >= MAX_PENDING - 4:
                return False
            row = self.store.db.execute("SELECT mid,rid,body,state FROM bridge_requests WHERE session=? AND state!='done' ORDER BY CASE state WHEN 'queued' THEN 0 ELSE 1 END, CASE WHEN mid>? THEN 0 ELSE 1 END, mid LIMIT 1", (self.session, self.poll_cursor)).fetchone()
        if row is None:
            return False
        mid, rid, body, state = row
        # Leave fresh WS events in the durable backend while the local carrier
        # catches up. The next poll coalesces them instead of queuing one tiny
        # application message for each token. New user requests remain admitted.
        if state == 'waiting' and pending_outputs >= 2:
            return False
        self.poll_cursor = mid
        outputs = self.dispatcher.poll(rid) if state == 'waiting' else self.dispatcher.dispatch(body, rid)
        # No raw exceptions, upstream headers, or bodies enter lifecycle evidence.
        with self.lock, self.store.db:
            if not self._admit_outputs(outputs):
                return False
            waiting = state == 'waiting' or any(o['state'] in ('local_queued', 'remoteaccepted', 'streaming') for o in outputs)
            terminal = any(o['state'] in ('reply', 'completed') for o in outputs)
            # A transient poll error must not abandon an accepted request.
            terminal = terminal or any(o['state'] == 'error' and o['event_id'] != rid + ':error' for o in outputs)
            self.store.db.execute('UPDATE bridge_requests SET state=? WHERE session=? AND mid=?',
                                  ('waiting' if waiting and not terminal else 'done', self.session, mid))
        return True

    def check_status(self):
        """Real backend evidence only; failures become bounded E messages."""
        if self.error or self.stop.is_set():
            return
        code = 502
        try:
            result = self.dispatcher.backend.handle('GET', '/api/status', {}, {})
            connected = result.get('connected') is True
        except APIError as exc:
            connected, code = False, exc.status
        except Exception:
            connected = False
        self.stats['account_connected'] = connected
        if not connected:
            text = {401: 'Sign in with nanocodex2 login on this machine.',
                    403: 'Account access denied.'}.get(code, 'Backend connection not confirmed. Check companion account status.')
            output = dict(kind='error', value=text, request_id='status', state='error', event_id='status:' + str(code))
            with self.lock, self.store.db:
                self._admit_outputs([output])

    def start_worker(self):
        def run():
            try:
                self.check_status()
                status_due = self.clock() + 15
                while not self.stop.is_set():
                    if self.clock() >= status_due:
                        self.check_status()
                        status_due = self.clock() + 15
                    worked = self.work_once()
                    # Local durable-event projection stays responsive without REST
                    # polling or an artificial delay in the key burst/carrier ACK.
                    self.wake.wait(.05 if worked else .25)
                    self.wake.clear()
            except Exception:
                self.error = 'backend worker stopped; retained intent requires reconciliation'
                self.stop.set()
        self.worker = threading.Thread(target=run, daemon=True)
        self.worker.start()

    def _advance_output(self):
        if self.link.pending is not None:
            return
        if self.active is not None:
            self.active['index'] += 1
            output = self.active['output']
            chunks = list(wire_fragments(output, self.active['mid']))
            if self.active['index'] == len(chunks):
                with self.store.db:
                    self.store.db.execute('UPDATE bridge_outputs SET delivered=1 WHERE session=? AND event=?',
                                          (self.session, output['event_id']))
                self.stats['delivered_outputs'] += 1
                self.active = None
                self.wake.set()
            else:
                self.link.send(chunks[self.active['index']])
                return
        row = self.store.db.execute('SELECT body FROM bridge_outputs WHERE session=? AND delivered=0 ORDER BY rowid LIMIT 1', (self.session,)).fetchone()
        if row:
            if self.next_mid > 65535:
                raise ValueError('new application session required')
            output = json.loads(row[0])
            self.active = {'output': output, 'mid': self.next_mid, 'index': 0}
            self.next_mid += 1
            self.link.send(next(iter(wire_fragments(output, self.active['mid']))))

    def step(self):
        """One independent capture then at most one local key batch; no backend IO."""
        if self.error:
            return False
        if not self.desktop.foreground() or not self.desktop.reserved():
            self.pump.stable = self.pump.observed = None
            return False
        try:
            capture_started = self.clock()
            packet = self.desktop.capture()
            if self.clock() - capture_started > .5:
                self.pump.stable = self.pump.observed = None
                return False  # Stale capture is never used to authorize input.
            frame = Frame.decode(packet)
            if frame.session != self.session:
                raise ValueError('session changed')
            if not self.restored and (frame.ack != 0 or frame.seq not in (0, 1)):
                raise ValueError('unknown carrier history; refuse sequence adoption')
            self.stats['captures'] += 1
            self.last_capture = {'session': frame.session, 'seq': frame.seq, 'ack': frame.ack,
                                 'ready': frame.ready, 'packet_sha256': hashlib.sha256(packet).hexdigest(),
                                 'elapsed': self.clock() - self.started}
            if self.uncertain:
                # Two independent captures must prove the potentially emitted data ACK.
                if packet != self.pump.observed:
                    self.pump.observed = packet
                    return False
                if self.uncertain_seq is None or frame.ack != self.uncertain_seq:
                    raise ValueError('uncertain input lacks matching stable ACK')
                self.uncertain = False
                self.uncertain_seq = None
            with self.lock:
                self.pump.observe(packet)
                if self.pump.stable is None:
                    return False
                self.restored = True
                self._advance_output()
                self._checkpoint()  # Commit received bytes/state before emitting ACK.
                sent = self.pump.tick()
                if self.pump.error:
                    self.error = 'carrier input outcome uncertain; reconcile before restart'
                self._checkpoint()
                return sent
        except Exception:
            self.error = 'capture/session/carrier validation failed; no further input'
            with self.lock:
                self._checkpoint()
            return False

    def evidence(self):
        return {'schema': 1, 'session': self.session, 'lifecycle': 'stopped' if self.error or self.stop.is_set() else 'running',
                'error': self.error, 'stats': dict(self.stats), 'last_capture': self.last_capture,
                'tx': self.link.tx, 'rx': self.link.rx,
                'model_roundtrip_proven': False, 'carrier_ack_is_model_completion': False}

    def close(self):
        self.stop.set()
        self.wake.set()
        if self.worker:
            self.worker.join(timeout=35)
            if self.worker.is_alive():
                return  # Leave resources intact for blocked daemon worker; process exits.
        with self.lock:
            self._checkpoint()
            self.store.close()
            os.close(self.guard)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('window-address', 'window-class', 'state-dir', 'evidence'):
        parser.add_argument('--' + name, required=True)
    for name in ('left', 'top'):
        parser.add_argument('--' + name, required=True, type=int)
    parser.add_argument('--cell-size', type=float, required=True, help='calibrated physical X cell pitch in pixels')
    parser.add_argument('--cell-size-y', type=float, help='calibrated physical Y cell pitch; defaults to X')
    parser.add_argument('--output-scale', type=float, default=1, help='explicit compositor output scale, e.g. 2')
    parser.add_argument('--session', type=lambda value: int(value, 0), required=True)
    parser.add_argument('--allow-input', action='store_true')
    parser.add_argument('--input-backend', choices=('wayland', 'x11', 'native-chord'), default='wayland')
    parser.add_argument('--timeout', type=float, default=60, help='seconds without valid capture before stopping')
    parser.add_argument('--duration', type=float, default=0, help='seconds; 0 runs until stopped')
    parser.add_argument('--key-hold-ms',type=int,default=0)
    parser.add_argument('--key-encoding',choices=('octal','binary'),default='octal')
    args = parser.parse_args(argv)
    if not args.allow_input:
        parser.error('--allow-input required; parent owns live input')
    try:
        desktop_session()
    except (OSError, ValueError) as error:
        parser.error(str(error))
    if not 1 <= args.timeout <= 3600:
        parser.error('timeout must be 1..3600 seconds')
    if args.duration < 0:
        parser.error('duration must be nonnegative')
    desktop = Desktop(args.window_address, args.window_class, args.left, args.top, args.cell_size, output_scale=args.output_scale, cell_size_y=args.cell_size_y, min_margin=2, input_backend=args.input_backend,key_encoding=args.key_encoding,key_hold_ms=args.key_hold_ms)
    directory = Path(args.state_dir)
    backend = DurableBackend(storage_dir=directory / 'backend')
    dispatcher = Dispatcher(backend, directory / 'dispatch.sqlite3')
    bridge = Bridge(desktop, args.session, dispatcher, directory / 'bridge.sqlite3')
    def evidence():
        destination = Path(args.evidence)
        temporary = destination.with_suffix(destination.suffix + '.tmp')
        temporary.write_text(json.dumps(bridge.evidence(), indent=2) + '\n')
        os.replace(temporary, destination)
    try:
        bridge.start_worker()
        started = time.monotonic()
        last_report = float('-inf')
        while not bridge.error and not bridge.stop.is_set():
            bridge.step()
            now = time.monotonic()
            if now - last_report >= 1:
                evidence()
                last_report = now
            last_valid = started + bridge.last_capture['elapsed'] if bridge.last_capture else started
            if now - last_valid >= args.timeout:
                bridge.error = 'carrier timeout; retained session requires reconciliation'
                break
            if args.duration and now - started >= args.duration:
                break
            if not bridge.desktop.foreground():
                bridge.stop.wait(.05)  # idle only; no per-key or per-frame throttle
    except KeyboardInterrupt:
        pass
    finally:
        bridge.close()
        evidence()
        if bridge.worker is None or not bridge.worker.is_alive():
            dispatcher.close()
            backend.close()
    return 1 if bridge.error else 0


if __name__ == '__main__':
    raise SystemExit(main())
