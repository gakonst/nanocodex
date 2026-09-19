#!/usr/bin/env python3
"""Discover the foreground WoW carrier, then reuse the journaled native bridge.

Run as the logged-in desktop user. --probe-only captures twice without input or
backend access. --first-live-test --allow-input runs a finite carrier handshake
and retains incoming requests for the normal worker; it never fabricates an
application request. Normal --allow-input runs until stopped (--duration bounds
it). No window/session/pixel coordinates or credential values are accepted.

Discovery is bounded to eight attempts, 30 seconds, a 320x320 logical top-left
crop at explicit grim scale 2, and physical cell pitches 2..16. Unsupported or
ambiguous geometry fails closed. Continuous normal mode retries bounded discovery
until a ready carrier appears; a started bridge is never automatically replayed.
Screenshots and payloads stay out of evidence.
"""
import argparse
from dataclasses import dataclass
import fcntl
import hashlib
import io
import json
import math
import os
from pathlib import Path
import stat
import subprocess
import sys
import time

if __package__ in (None, ''):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from PIL import Image
from transport.calibrate import decode_grid
from transport.protocol import Frame
from transport.session import desktop_session
from transport.wayland import Desktop

TITLE = 'World of Warcraft'
SCALE = 2
CROP = 320
MAX_ORIGIN = 192
NATIVE_MANIFEST = {'schema': 1, 'modifiers': ['CTRL', 'SHIFT'],
                   'keys': ['F' + str(n) for n in (1, 2, 3, 5, 6, 7, 8, 9, 10, 11)]}
# Boundaries in NC1's first 24 MSB-first bits. The initial black run may merge
# with the background, so anchor at its first 0->1 transition instead.
MAGIC_EDGES = (1, 2, 4, 7, 9, 10, 14, 16, 18, 20, 23, 24)


class DiscoveryError(ValueError):
    pass


@dataclass(frozen=True)
class Window:
    address: str
    window_class: str
    pid: int
    x: int
    y: int
    width: int
    height: int

    @classmethod
    def parse(cls, value):
        if not isinstance(value, dict) or value.get('title') != TITLE:
            raise DiscoveryError('foreground is not exactly World of Warcraft')
        address, name, pid = (value.get(k) for k in ('address', 'class', 'pid'))
        if (not isinstance(address, str) or not address or
                not isinstance(name, str) or not name or type(pid) is not int or pid <= 0):
            raise DiscoveryError('foreground identity incomplete')
        at, size = value.get('at'), value.get('size')
        if (not isinstance(at, list) or not isinstance(size, list) or
                len(at) != 2 or len(size) != 2 or
                any(type(v) is not int for v in at + size) or
                not all(64 <= v <= 32768 for v in size) or
                any(abs(v) > 32768 for v in at)):
            raise DiscoveryError('foreground geometry invalid')
        return cls(address, name, pid, *at, *size)

    @property
    def geometry(self):
        return f'{self.x},{self.y} {min(CROP, self.width)}x{min(CROP, self.height)}'

    @property
    def image_size(self):
        return min(CROP, self.width) * SCALE, min(CROP, self.height) * SCALE


@dataclass(frozen=True)
class Located:
    left: float
    top: float
    pitch_x: float
    pitch_y: float
    packet: bytes
    confidence: dict

    def decode(self, image):
        rgb = image.convert('RGB')
        return decode_grid(lambda x, y: rgb.getpixel((x, y)), self.left, self.top, self.pitch_x,
                           self.pitch_y, min_margin=2)

    def evidence(self):
        f = Frame.decode(self.packet)
        return {'origin_in_crop': [self.left, self.top],
                'cell_size_x': self.pitch_x, 'cell_size_y': self.pitch_y,
                'session': f.session, 'seq': f.seq, 'ack': f.ack, 'ready': f.ready,
                'packet_sha256': hashlib.sha256(self.packet).hexdigest(),
                'confidence': self.confidence}


def _x_fits(edges):
    low, high = 2., 16.
    for i in range(len(edges)):
        for j in range(i + 1, len(edges)):
            span = MAGIC_EDGES[j] - MAGIC_EDGES[i]
            low = max(low, (edges[j] - edges[i] - 1) / span)
            high = min(high, (edges[j] - edges[i] + 1) / span)
    if low > high:
        return []
    result = []
    for fraction in (.5, .25, .75, .05, .95):
        pitch = low + (high - low) * fraction
        a = max(p - 1 - n * pitch for n, p in zip(MAGIC_EDGES, edges))
        b = min(p - n * pitch for n, p in zip(MAGIC_EDGES, edges))
        if a <= b + 1e-8:
            left = max(0., (a + b) / 2)
            if left <= MAX_ORIGIN:
                result.append((left, pitch))
    return result


def locate(image, *, deadline=None, clock=time.monotonic):
    """Offline NC1 locator. Infer X from magic edges and Y independently.

    At most 32 candidate magic bands and 160k sampling grids. Every accepted
    grid validates all 1024 bits, packet semantics, CRC, and zero padding.
    Pixel quantization is used only to nominate candidates, never guess bits.
    """
    if not 64 <= image.width <= CROP * SCALE or not 64 <= image.height <= CROP * SCALE:
        raise DiscoveryError('bounded crop dimensions required')
    deadline = clock() + 3 if deadline is None else deadline
    rgb = image.convert('RGB')
    pixels = list(rgb.getdata())
    width, height = image.size
    binary = []
    strict = []
    for r, g, b in pixels:
        mean = (r + g + b) / 3
        mono = max(r, g, b) - min(r, g, b) <= 40
        bit = int(mean > 128)
        binary.append(bit if mono else 2)
        strict.append(bit if mono and abs(mean - 128) >= 2 else 2)
    bands = {}
    for y in range(min(MAX_ORIGIN + 17, height - 63)):
        if clock() >= deadline:
            raise DiscoveryError('discovery search budget exhausted')
        row = binary[y * width:(y + 1) * width]
        runs = [(row[0], 0)]
        runs.extend((v, x) for x, v in enumerate(row[1:], 1) if v != row[x - 1])
        for start in range(len(runs) - 11):
            group = runs[start:start + 12]
            if any(v != (1 - i % 2) for i, (v, _) in enumerate(group)):
                continue
            edges = tuple(x for _, x in group)
            fits = _x_fits(edges)
            if not fits:
                continue
            # Rows of the first carrier cell form one contiguous magic band.
            key = edges
            rows = bands.setdefault(key, [])
            if rows and rows[-1][1] == y - 1:
                rows[-1][1] = y
            else:
                rows.append([y, y])
            if sum(len(v) for v in bands.values()) > 32:
                raise DiscoveryError('ambiguous magic search exceeds bound')
    found = []
    grids = 0
    for edges, ranges in bands.items():
        for start, end in ranges:
            if start > MAX_ORIGIN:
                continue
            matches = []
            for left, cx in _x_fits(edges):
                columns = tuple(int(left + (x + .5) * cx) for x in range(32))
                if columns[-1] >= width or left + 32 * cx > width + .01:
                    continue
                packed = []
                for y in range(height):
                    bits = [strict[y * width + x] for x in columns]
                    packed.append(None if 2 in bits else bytes(
                        sum(bits[i + j] << (7 - j) for j in range(8)) for i in (0, 8, 16, 24)))
                # A sampled band can span multiple rows when the next row also
                # begins with NC1. Search the bounded independent Y range.
                top = max(0., start - .5)
                upper = min(16., (height - top) / 32, end - start + 2.)
                seen = set()
                for n in range(129, math.floor(upper * 64) + 1):
                    cy = n / 64
                    ys = tuple(int(top + (y + .5) * cy) for y in range(32))
                    if ys in seen:
                        continue
                    seen.add(ys)
                    grids += 1
                    if grids > 160000 or clock() >= deadline:
                        raise DiscoveryError('discovery search budget exhausted')
                    rows = [packed[y] for y in ys]
                    if any(row is None for row in rows):
                        continue
                    raw = b''.join(rows)
                    if raw[:3] != b'NC1' or raw[3] > 1 or raw[12] > 96:
                        continue
                    size = raw[12] + 15
                    try:
                        Frame.decode(raw[:size])
                    except ValueError:
                        continue
                    if any(raw[size:]):
                        raise DiscoveryError('CRC-valid geometry has nonzero padding; refuse sampling ambiguity')
                    matches.append((left, top, cx, cy, raw[:size]))
            if matches:
                if len({m[4] for m in matches}) != 1:
                    raise DiscoveryError('ambiguous valid packets')
                # Center the valid pitch interval rather than choosing its edge.
                middle_y = (min(m[3] for m in matches) + max(m[3] for m in matches)) / 2
                middle_x = (min(m[2] for m in matches) + max(m[2] for m in matches)) / 2
                best = min(matches, key=lambda m: abs(m[3] - middle_y) + abs(m[2] - middle_x))
                left, top, cx, cy, packet = best
                packet2, detail = decode_grid(lambda x, y: rgb.getpixel((x, y)), left, top, cx, cy, details=True)
                assert packet == packet2
                found.append(Located(left, top, cx, cy, packet, detail))
    if not found:
        raise DiscoveryError('no full CRC and padding valid NC1 carrier in top-left crop')
    # Multiple row-edge aliases of the same physical grid are acceptable;
    # a distinct carrier or packet is not an authorization to choose one.
    first = found[0]
    if any(f.packet != first.packet or abs(f.left - first.left) > 1 or
           abs(f.top - first.top) > 1 for f in found[1:]):
        raise DiscoveryError('multiple distinct carriers')
    return first


class Discovery:
    def __init__(self, run=subprocess.run, *, clock=time.monotonic, pause=time.sleep):
        self.run, self.clock, self.pause = run, clock, pause
        self.captures = 0

    def window(self):
        result = self.run(['hyprctl', '-j', 'activewindow'], stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=1, check=True)
        return Window.parse(json.loads(result.stdout))

    def capture(self, window):
        if self.window() != window:
            raise DiscoveryError('foreground identity or geometry changed')
        result = self.run(['grim', '-s', str(SCALE), '-g', window.geometry, '-t', 'png', '-'],
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=2, check=True)
        self.captures += 1
        if self.window() != window:
            raise DiscoveryError('foreground changed during capture')
        if len(result.stdout) > 4 * 1024 * 1024:
            raise DiscoveryError('capture exceeds PNG bound')
        with Image.open(io.BytesIO(result.stdout)) as image:
            if image.format != 'PNG' or image.size != window.image_size:
                raise DiscoveryError('capture scale or dimensions differ')
            return image.convert('RGB')

    def discover(self, *, timeout=30, attempts=8, require_ready=True):
        if not 1 <= timeout <= 60 or not 1 <= attempts <= 16:
            raise ValueError('discovery timeout/attempt bounds')
        deadline = self.clock() + timeout
        reason = 'no valid capture'
        for _ in range(attempts):
            if self.clock() >= deadline:
                break
            try:
                window = self.window()
                first = self.capture(window)
                located = locate(first, deadline=min(deadline, self.clock() + 3), clock=self.clock)
                self.pause(.02)
                second = self.capture(window)  # New grim process, never reuse the image.
                if located.decode(second) != located.packet:
                    raise DiscoveryError('independent captures disagree')
                if require_ready and not Frame.decode(located.packet).ready:
                    raise DiscoveryError('validated carrier not ready')
                if self.clock() >= deadline:
                    raise DiscoveryError('discovery deadline reached')
                return window, located
            except (ValueError, OSError, subprocess.SubprocessError):
                # No upstream stderr, image, payload, or raw exception in evidence.
                reason = 'foreground/carrier not stable, ready, or fully validated'
            if self.clock() < deadline:
                self.pause(min(.25, deadline - self.clock()))
        raise DiscoveryError(reason + '; bounded discovery stopped')


class ConnectedDesktop(Desktop):
    """Reuse native input API, adding exact title/PID/geometry and fresh capture."""
    def __init__(self, discovery, window, located, *, key_hold_ms=0):
        super().__init__(window.address, window.window_class, 0, 0, located.pitch_x,
                         run=discovery.run, output_scale=SCALE, cell_size_y=located.pitch_y,
                         input_backend='native-chord', key_encoding='octal', key_hold_ms=key_hold_ms)
        self.discovery, self.window_identity, self.located = discovery, window, located
        self.preflight_ok = False
        self.input_attempts = 0

    def foreground(self):
        try:
            return self.discovery.window() == self.window_identity
        except (ValueError, OSError, subprocess.SubprocessError):
            return False

    def preflight(self):
        spec = json.loads(self._text([str(Path(__file__).parent / 'native' / 'carrier-keys'), '--describe']))
        if spec != NATIVE_MANIFEST:
            raise DiscoveryError('native helper manifest differs from approved chord alphabet')
        if not self.foreground() or not self.reserved():
            raise DiscoveryError('foreground identity or native chord reservation unavailable')
        self.preflight_ok = True
        self._native_verified = True

    def capture(self):
        image = self.discovery.capture(self.window_identity)
        try:
            packet = self.located.decode(image)
        except ValueError:
            # Short idle frames constrain Y less than full data frames. A changed
            # packet may refine sampling, but must keep the same carrier/session.
            refined = locate(image, deadline=self.discovery.clock() + .4, clock=self.discovery.clock)
            if (Frame.decode(refined.packet).session != Frame.decode(self.located.packet).session or
                    abs(refined.left - self.located.left) > 2 or abs(refined.top - self.located.top) > 2):
                raise DiscoveryError('carrier identity changed')
            self.located = refined
            packet = refined.packet
        if Frame.decode(packet).session != Frame.decode(self.located.packet).session:
            raise DiscoveryError('carrier session changed')
        return packet

    def send_keys(self, keys):
        if not self.preflight_ok:
            raise DiscoveryError('native preflight required before input')
        self.input_attempts += 1
        return super().send_keys(keys)


def private_directory(path):
    path = Path(path).expanduser().absolute()
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o077:
        raise DiscoveryError('state directory must be private and owned by current user')
    return path


def journal_directory(state, session):
    """Preserve an existing manual daemon's flat journals, otherwise key by session."""
    if (state / 'bridge.sqlite3').exists() or (state / 'dispatch.sqlite3').exists():
        return state
    return private_directory(state / 'sessions' / ('%08x' % session))


def write_evidence(path, receipt):
    path = Path(path)
    temporary = path.with_name(path.name + '.tmp')
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as stream:
        json.dump(receipt, stream, indent=2)
        stream.write('\n')
    os.replace(temporary, path)


def serve(desktop, state, *, first_live_test=False, duration=0, max_input_batches=4,
          report=lambda value: None, clock=time.monotonic, pause=time.sleep):
    from durable_client import DurableBackend
    from transport.dispatch import Dispatcher
    from transport.daemon import Bridge
    if not desktop.preflight_ok:
        raise DiscoveryError('native preflight required before opening bridge')
    frame = Frame.decode(desktop.located.packet)
    if not frame.ready:
        raise DiscoveryError('validated ready frame required')
    if first_live_test and (not 1 <= duration <= 30 or not 1 <= max_input_batches <= 8):
        raise ValueError('first live test requires duration1..30 and batches1..8')
    directory = journal_directory(state, frame.session)
    backend = DurableBackend(storage_dir=state / 'backend')
    dispatcher = Dispatcher(backend, directory / 'dispatch.sqlite3')
    bridge = None
    try:
        bridge = Bridge(desktop, frame.session, dispatcher, directory / 'bridge.sqlite3', clock=clock)
        if not bridge.restored and (frame.ack != 0 or frame.seq not in (0, 1)):
            bridge.error = 'unknown carrier history; existing session journal required'
        # One ordinary ACK-only packet establishes peer presence on an idle
        # session. Pump still requires TWO fresh captures and all readiness gates.
        bridge.pump.sent_ack = -1
        started = clock()
        last_report = float('-inf')
        worker_started = False
        last_tick = started
        last_capture = None
        foreground_previous = desktop.foreground()
        foreground_without_capture = 0.
        while not bridge.error and not bridge.stop.is_set():
            now = clock()
            if duration and now - started >= duration:
                break
            if first_live_test and desktop.input_attempts >= max_input_batches:
                break
            bridge.step()
            # Test mode makes no backend calls. Any received application bytes
            # remain journaled exactly once for the normal worker's later resume.
            if not bridge.error and not first_live_test and not worker_started and bridge.pump.stable and not bridge.uncertain:
                bridge.start_worker()
                worker_started = True
            if now - last_report >= 1:
                report(bridge.evidence())
                last_report = now
            foreground = desktop.foreground()
            tick = clock()
            if bridge.last_capture is not last_capture:
                last_capture = bridge.last_capture
                foreground_without_capture = 0.
            elif foreground_previous and foreground:
                foreground_without_capture += tick - last_tick
            # Alt-Tab pauses the timeout without reopening journals or adopting
            # a new identity. Bridge resets its two-capture gate while unfocused.
            last_tick, foreground_previous = tick, foreground
            if not bridge.error and foreground_without_capture > 30:
                bridge.error = 'carrier timeout; retained session requires reconciliation'
                break
            pause(.01 if foreground else .1)
        return bridge.evidence()
    finally:
        if bridge:
            bridge.close()
            report(bridge.evidence())
        if bridge is None or bridge.worker is None or not bridge.worker.is_alive():
            dispatcher.close()
            backend.close()


def wait_for_discovery(discovery, *, timeout, attempts, require_ready, continuous,
                       report, clock=time.monotonic, wall_clock=time.time, pause=time.sleep):
    """Retry only read-only discovery; never restart a bridge or replay input."""
    started = clock()
    round_number = 0
    while True:
        round_number += 1
        report({'lifecycle': 'discovering', 'discovery_round': round_number,
                'updated_at': wall_clock(), 'discovery_elapsed': clock() - started,
                'capture_calls': discovery.captures})
        try:
            return discovery.discover(timeout=timeout, attempts=attempts,
                                      require_ready=require_ready)
        except DiscoveryError:
            report({'lifecycle': 'waiting', 'discovery_round': round_number,
                    'updated_at': wall_clock(), 'discovery_elapsed': clock() - started,
                    'capture_calls': discovery.captures,
                    'waiting_reason': 'foreground/carrier not stable, ready, or fully validated'})
            if not continuous:
                raise
            pause(1)  # Bounded discovery already has attempt/deadline limits.


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument('--probe-only', action='store_true', help='two independent captures; no input/backend')
    modes.add_argument('--first-live-test', action='store_true', help='finite handshake; retains requests without backend IO')
    parser.add_argument('--allow-input', action='store_true')
    parser.add_argument('--state-dir', type=Path, default=Path.home() / '.local/state/nanocodex-wow/autoconnect')
    parser.add_argument('--evidence', type=Path, help='default STATE_DIR/evidence.json; no screenshots or message bodies')
    parser.add_argument('--discovery-timeout', type=float, default=30)
    parser.add_argument('--discovery-attempts', type=int, default=8)
    parser.add_argument('--duration', type=float, default=None, help='default30 test, 0 continuous normal run')
    parser.add_argument('--max-input-batches', type=int, default=4, help='first-live-test bound1..8')
    parser.add_argument('--key-hold-ms', type=int, default=0)
    args = parser.parse_args(argv)
    if args.probe_only and args.allow_input:
        parser.error('--probe-only does not permit --allow-input')
    if not args.probe_only and not args.allow_input:
        parser.error('connection requires --allow-input; use --probe-only for read-only discovery')
    duration = (30 if args.first_live_test else 0) if args.duration is None else args.duration
    if (not 1 <= args.discovery_timeout <= 60 or not 1 <= args.discovery_attempts <= 16 or
            not math.isfinite(duration) or duration < 0 or not 0 <= args.key_hold_ms <= 5 or
            (args.first_live_test and (not 1 <= duration <= 30 or not 1 <= args.max_input_batches <= 8))):
        parser.error('bounded arguments required: discovery1..60s/1..16 attempts, hold0..5ms, test1..30s/1..8 batches')
    receipt = {'schema': 1, 'mode': 'probe-only' if args.probe_only else 'first-live-test' if args.first_live_test else 'run',
               'lifecycle': 'starting', 'model_roundtrip_proven': False,
               'carrier_ack_is_model_completion': False, 'input_attempts': 0}
    state = destination = desktop = None
    guard = None
    try:
        desktop_session()  # No subprocess or account initialization before this guard.
        state = private_directory(args.state_dir)
        destination = args.evidence or state / 'evidence.json'
        guard = os.open(state / 'autoconnect.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
        discovery = Discovery()
        def discovery_report(value):
            receipt.update(value)
            write_evidence(destination, receipt)
        window, located = wait_for_discovery(
            discovery, timeout=args.discovery_timeout, attempts=args.discovery_attempts,
            require_ready=not args.probe_only,
            continuous=not args.probe_only and not args.first_live_test and duration == 0,
            report=discovery_report)
        receipt.pop('waiting_reason', None)
        receipt.update(lifecycle='discovered', discovery={**located.evidence(),
                       'independent_captures': 2, 'capture_calls': discovery.captures,
                       'logical_crop': window.geometry, 'output_scale': SCALE,
                       'window_address': window.address, 'window_class': window.window_class,
                       'window_title': TITLE, 'window_pid': window.pid})
        if not args.probe_only:
            desktop = ConnectedDesktop(discovery, window, located, key_hold_ms=args.key_hold_ms)
            desktop.preflight()
            receipt['native_manifest_verified'] = True
            receipt['native_chords_reserved'] = True
            def report(value):
                receipt.update(bridge=value, input_attempts=desktop.input_attempts, lifecycle=value['lifecycle'])
                write_evidence(destination, receipt)
            serve(desktop, state, first_live_test=args.first_live_test, duration=duration,
                  max_input_batches=args.max_input_batches, report=report)
            receipt['success'] = not receipt['bridge']['error']
        else:
            receipt['success'] = True
        receipt['lifecycle'] = 'stopped'
    except KeyboardInterrupt:
        receipt.update(lifecycle='stopped', success=False, error='interrupted; journal retained')
    except Exception:
        receipt.update(lifecycle='stopped', success=False,
                       error='desktop/discovery/native/journal gate failed; no automatic replay; inspect local configuration')
    finally:
        if desktop:
            receipt['input_attempts'] = desktop.input_attempts
        if destination:
            write_evidence(destination, receipt)
        if guard is not None:
            os.close(guard)
    print(json.dumps(receipt, indent=2))
    return 0 if receipt.get('success') else 1


if __name__ == '__main__':
    raise SystemExit(main())
