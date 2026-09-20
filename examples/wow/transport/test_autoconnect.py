"""Autoconnect regressions: synthetic pixels, fake desktop IO, local SQLite only."""
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

from PIL import Image

from transport import autoconnect as auto
from transport.daemon import Bridge
from transport.protocol import Frame


PACKET = Frame(53281, 1, 0, bytes(range(96)), True).encode()
IDLE = Frame(53281, ready=True).encode()
WINDOW = dict(title=auto.TITLE, address='0x123', **{'class': 'wow'},
              pid=42, at=[10, 20], size=[160, 160])


def carrier(packet=PACKET, *, raw=None, origin=(17, 13), pitch=(4.75, 4.5), size=(320, 320)):
    raw = packet.ljust(128, b'\0') if raw is None else raw
    image = Image.new('RGB', size, (32, 48, 64))
    for y in range(int(32 * pitch[1])):
        for x in range(int(32 * pitch[0])):
            index = int(y / pitch[1]) * 32 + int(x / pitch[0])
            value = 255 * ((raw[index // 8] >> (7 - index % 8)) & 1)
            image.putpixel((x + origin[0], y + origin[1]), (value,) * 3)
    return image


def png(image):
    buffer = io.BytesIO()
    image.save(buffer, format='PNG')
    return buffer.getvalue()


class Clock:
    def __init__(self):
        self.value = 0.

    def __call__(self):
        return self.value

    def pause(self, seconds):
        self.value += seconds


class FakeIO:
    def __init__(self, images, windows=None):
        self.images = iter(png(im) for im in images)
        self.windows = iter(windows) if windows is not None else None
        self.commands = []

    def __call__(self, command, **kwargs):
        self.commands.append(command)
        if command == ['hyprctl', '-j', 'activewindow']:
            body = json.dumps(next(self.windows) if self.windows else WINDOW).encode()
        elif command[0] == 'grim':
            body = next(self.images)
            assert command == ['grim', '-s', '2', '-g', '10,20 160x160', '-t', 'png', '-']
        else:
            raise AssertionError('Unexpected desktop command: ' + repr(command))
        return subprocess.CompletedProcess(command, 0, body, b'')


class DiscoveryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.image = carrier()
        cls.located = auto.locate(cls.image)

    def test_full_nc1_noninteger_axes_and_offset(self):
        found = self.located
        self.assertEqual(found.packet, PACKET)
        self.assertEqual(found.decode(self.image), PACKET)
        evidence = found.evidence()
        self.assertEqual(evidence['confidence']['sampled_bits'], 1024)
        self.assertTrue(evidence['confidence']['full_crc_valid'])
        self.assertTrue(evidence['confidence']['full_padding_valid'])
        self.assertEqual(evidence['confidence']['padding_bytes_checked'], 17)
        self.assertNotIn(PACKET.hex(), json.dumps(evidence))

    def test_rejects_corrupt_crc_and_padding(self):
        for index in (len(PACKET) - 1, 127):
            with self.subTest(index=index):
                raw = bytearray(PACKET.ljust(128, b'\0'))
                raw[index] ^= 1
                image = carrier(raw=raw)
                with self.assertRaises(ValueError):
                    self.located.decode(image)
                with self.assertRaises(auto.DiscoveryError):
                    auto.locate(image)

    def test_requires_two_separate_full_captures(self):
        io_run = FakeIO([self.image, self.image.copy()])
        clock = Clock()
        discovery = auto.Discovery(io_run, clock=clock, pause=clock.pause)
        window, found = discovery.discover(attempts=1)
        self.assertEqual(window, auto.Window.parse(WINDOW))
        self.assertEqual(found.packet, PACKET)
        self.assertEqual(discovery.captures, 2)
        self.assertEqual(sum(command[0] == 'grim' for command in io_run.commands), 2)
        self.assertTrue(all(command[0] in ('hyprctl', 'grim') for command in io_run.commands))

    def test_second_capture_checks_crc_padding_and_session(self):
        invalid_crc = bytearray(PACKET.ljust(128, b'\0'))
        invalid_crc[len(PACKET) - 1] ^= 1
        invalid_padding = bytearray(PACKET.ljust(128, b'\0'))
        invalid_padding[127] = 255
        changed_session = Frame(53282, 1, 0, bytes(range(96)), True).encode()
        for second in (carrier(raw=invalid_crc), carrier(raw=invalid_padding), carrier(changed_session)):
            with self.subTest(second=second):
                clock = Clock()
                discovery = auto.Discovery(FakeIO([self.image, second]), clock=clock, pause=clock.pause)
                with self.assertRaises(auto.DiscoveryError):
                    discovery.discover(attempts=1)
                self.assertEqual(discovery.captures, 2)

    def test_window_change_before_or_after_capture_stops(self):
        for field, value in [('title', 'Other'), ('address', '0x456'), ('class', 'other'),
                             ('pid', 43), ('at', [11, 20]), ('size', [161, 160])]:
            for after in (False, True):
                with self.subTest(field=field, after=after):
                    changed = dict(WINDOW, **{field: value})
                    windows = [WINDOW, WINDOW, changed] if after else [WINDOW, changed]
                    runner = FakeIO([self.image], windows)
                    clock = Clock()
                    discovery = auto.Discovery(runner, clock=clock, pause=clock.pause)
                    with self.assertRaises(auto.DiscoveryError):
                        discovery.discover(attempts=1)
                    self.assertEqual(discovery.captures, int(after))

    def test_attempt_deadline_and_crop_bounds(self):
        clock = Clock()
        discovery = auto.Discovery(Mock(), clock=clock, pause=clock.pause)
        discovery.window = Mock(side_effect=auto.DiscoveryError('absent'))
        with self.assertRaises(auto.DiscoveryError):
            discovery.discover(timeout=1, attempts=16)
        self.assertEqual(discovery.window.call_count, 4)
        self.assertEqual(clock(), 1)
        discovery.window.reset_mock()
        with self.assertRaises(auto.DiscoveryError):
            discovery.discover(timeout=30, attempts=2)
        self.assertEqual(discovery.window.call_count, 2)
        for size in ((63, 100), (100, 641)):
            with self.assertRaises(auto.DiscoveryError):
                auto.locate(Image.new('RGB', size))
        with self.assertRaisesRegex(auto.DiscoveryError, 'budget'):
            auto.locate(self.image, deadline=0, clock=lambda: 0)
        large = auto.Window.parse(dict(WINDOW, size=[1000, 900]))
        self.assertEqual(large.geometry, '10,20 320x320')
        self.assertEqual(large.image_size, (640, 640))

    def test_not_ready_and_bad_capture_dimensions_fail_closed(self):
        clock = Clock()
        for image in (carrier(Frame(53281).encode()), Image.new('RGB', (319, 320))):
            discovery = auto.Discovery(FakeIO([image, image]), clock=clock, pause=clock.pause)
            with self.assertRaises(auto.DiscoveryError):
                discovery.discover(attempts=1)


class PreflightTests(unittest.TestCase):
    def setUp(self):
        self.window = auto.Window.parse(WINDOW)
        self.located = auto.locate(carrier())
        self.discovery = Mock()
        self.discovery.window.return_value = self.window
        self.desktop = auto.ConnectedDesktop(self.discovery, self.window, self.located)

    def test_input_and_serve_forbidden_before_preflight(self):
        with self.assertRaisesRegex(auto.DiscoveryError, 'preflight'):
            self.desktop.send_keys(['F21', 'F22'])
        self.discovery.run.assert_not_called()
        self.assertEqual(self.desktop.input_attempts, 0)
        with patch('durable_client.DurableBackend') as backend:
            with self.assertRaisesRegex(auto.DiscoveryError, 'preflight'):
                auto.serve(self.desktop, Path('/unused'))
            backend.assert_not_called()

    def test_manifest_foreground_and_reservation_are_all_required(self):
        for manifest, foreground, reserved in [({}, True, True), (auto.NATIVE_MANIFEST, False, True),
                                                (auto.NATIVE_MANIFEST, True, False)]:
            with self.subTest(manifest=manifest, foreground=foreground, reserved=reserved):
                with patch.object(self.desktop, '_text', return_value=json.dumps(manifest)), \
                     patch.object(self.desktop, 'foreground', return_value=foreground), \
                     patch.object(self.desktop, 'reserved', return_value=reserved):
                    with self.assertRaises(auto.DiscoveryError):
                        self.desktop.preflight()
                self.assertFalse(self.desktop.preflight_ok)
                self.assertEqual(self.desktop.input_attempts, 0)
        self.discovery.run.assert_not_called()

    def test_changed_session_rejected_after_discovery(self):
        self.discovery.capture.return_value = carrier(Frame(99, 1, 0, bytes(range(96)), True).encode())
        with self.assertRaisesRegex(auto.DiscoveryError, 'session'):
            self.desktop.capture()
        self.discovery.run.assert_not_called()


class LifecycleTests(unittest.TestCase):
    def test_continuous_wait_writes_fresh_evidence_and_recovers(self):
        discovery = Mock(captures=0)
        expected = (auto.Window.parse(WINDOW), auto.locate(carrier()))
        discovery.discover.side_effect = [auto.DiscoveryError('absent'), auto.DiscoveryError('occluded'), expected]
        reports = []
        clock = Clock()
        result = auto.wait_for_discovery(discovery, timeout=1, attempts=2, require_ready=True,
                                        continuous=True, report=reports.append,
                                        clock=clock, wall_clock=clock, pause=clock.pause)
        self.assertEqual(result, expected)
        self.assertEqual(discovery.discover.call_count, 3)
        self.assertEqual([r['lifecycle'] for r in reports], ['discovering', 'waiting', 'discovering', 'waiting', 'discovering'])
        self.assertEqual([r['discovery_round'] for r in reports], [1, 1, 2, 2, 3])
        self.assertEqual(reports[-1]['updated_at'], 2)
        self.assertEqual(reports[-1]['discovery_elapsed'], 2)

    def test_finite_discovery_does_not_retry(self):
        discovery = Mock(captures=0)
        discovery.discover.side_effect = auto.DiscoveryError('absent')
        pause = Mock()
        with self.assertRaises(auto.DiscoveryError):
            auto.wait_for_discovery(discovery, timeout=1, attempts=2, require_ready=True,
                                    continuous=False, report=Mock(), pause=pause)
        self.assertEqual(discovery.discover.call_count, 1)
        pause.assert_not_called()

    def test_session_guard_precedes_discovery_and_state(self):
        with patch.object(auto, 'desktop_session', side_effect=ValueError('wrong session')), \
             patch.object(auto, 'private_directory') as state, \
             patch.object(auto, 'Discovery') as discovery, patch('builtins.print'):
            self.assertEqual(auto.main(['--probe-only']), 1)
        state.assert_not_called()
        discovery.assert_not_called()

    def test_cli_modes_and_no_restart_after_uncertain_input(self):
        for mode, continuous in [(['--probe-only'], False),
                                  (['--first-live-test', '--allow-input'], False),
                                  (['--allow-input', '--duration', '0'], True),
                                  (['--allow-input', '--duration', '1'], False)]:
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                located = auto.locate(carrier())
                desktop = Mock(input_attempts=1)
                def serve(*args, **kwargs):
                    kwargs['report']({'lifecycle': 'stopped', 'error': 'input outcome uncertain'})
                with patch.object(auto, 'desktop_session'), patch.object(auto, 'Discovery', return_value=Mock(captures=2)), \
                     patch.object(auto, 'wait_for_discovery', return_value=(auto.Window.parse(WINDOW), located)) as wait, \
                     patch.object(auto, 'ConnectedDesktop', return_value=desktop), \
                     patch.object(auto, 'serve', side_effect=serve) as serving, patch('builtins.print'):
                    result = auto.main(mode + ['--state-dir', directory])
                self.assertEqual(wait.call_count, 1)
                self.assertEqual(wait.call_args.kwargs['continuous'], continuous)
                self.assertEqual(result, 0 if '--probe-only' in mode else 1)
                self.assertEqual(serving.call_count, 0 if '--probe-only' in mode else 1)
                evidence = json.loads((Path(directory) / 'evidence.json').read_text())
                self.assertEqual(evidence['lifecycle'], 'stopped')
                self.assertNotIn(PACKET.hex(), json.dumps(evidence))
                self.assertFalse(evidence['model_roundtrip_proven'])


class JournalTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.state = Path(self.temp.name)
        self.clock = Clock()
        self.desktop = Mock(preflight_ok=True, input_attempts=0, key_encoding='octal')
        self.desktop.located = auto.Located(0, 0, 4, 4, IDLE, {})
        self.desktop.foreground.return_value = True
        self.desktop.reserved.return_value = True
        self.desktop.capture.return_value = IDLE
        self.backend = Mock()
        self.dispatcher = Mock()

    def serve(self, *, allow_worker=False, **kwargs):
        with patch('durable_client.DurableBackend', return_value=self.backend), \
             patch('transport.dispatch.Dispatcher', return_value=self.dispatcher), \
             patch.object(Bridge, 'start_worker') as worker:
            result = auto.serve(self.desktop, self.state, duration=kwargs.pop('duration', 1),
                                clock=self.clock, pause=self.clock.pause, **kwargs)
        if allow_worker:
            worker.assert_called_once()
        else:
            worker.assert_not_called()
        self.backend.handle.assert_not_called()
        self.dispatcher.dispatch.assert_not_called()
        self.dispatcher.poll.assert_not_called()
        return result

    def test_uncertain_journal_never_replays_without_matching_ack(self):
        for sequence in (None, 1):
            with self.subTest(sequence=sequence):
                directory = auto.journal_directory(self.state, 53281)
                bridge = Bridge(self.desktop, 53281, self.dispatcher, directory / 'bridge.sqlite3', clock=self.clock)
                bridge.error = None
                bridge.uncertain = True
                bridge.uncertain_seq = sequence
                if sequence:
                    bridge.link.send(b'pending')
                bridge.close()
                result = self.serve()
                self.assertIsNotNone(result['error'])
                self.assertEqual(result['stats']['captures'], 2)
                self.desktop.send_keys.assert_not_called()
                resumed = Bridge(self.desktop, 53281, self.dispatcher, directory / 'bridge.sqlite3', clock=self.clock)
                self.assertTrue(resumed.uncertain)
                self.assertIsNotNone(resumed.error)
                if sequence:
                    self.assertEqual(resumed.link.pending, b'pending')
                resumed.close()

    def test_persisted_input_error_stops_before_capture_or_worker(self):
        directory = auto.journal_directory(self.state, 53281)
        bridge = Bridge(self.desktop, 53281, self.dispatcher, directory / 'bridge.sqlite3', clock=self.clock)
        bridge.error = 'carrier input outcome uncertain; reconcile before restart'
        bridge.close()
        result = self.serve()
        self.assertIn('uncertain', result['error'])
        self.desktop.capture.assert_not_called()
        self.desktop.send_keys.assert_not_called()

    def test_unknown_carrier_history_requires_existing_journal(self):
        self.desktop.located = auto.Located(0, 0, 4, 4, Frame(53281, ack=1, ready=True).encode(), {})
        result = self.serve()
        self.assertIn('existing session journal required', result['error'])
        self.desktop.capture.assert_not_called()
        self.desktop.send_keys.assert_not_called()

    def test_alt_tab_over_30_seconds_resumes_same_journal(self):
        self.desktop.foreground.side_effect = lambda: self.clock() < .03 or self.clock() >= 40
        captures = []
        def capture():
            captures.append(self.clock())
            return IDLE
        self.desktop.capture.side_effect = capture
        self.desktop.send_keys.return_value = True
        result = self.serve(duration=40.1, allow_worker=True)
        self.assertIsNone(result['error'])
        self.assertGreaterEqual(self.clock(), 40.1)
        self.assertGreaterEqual(sum(t >= 40 for t in captures), 2)
        self.assertFalse(any(.03 <= t < 40 for t in captures))
        self.assertEqual(self.desktop.send_keys.call_count, 1)
        directory = auto.journal_directory(self.state, 53281)
        resumed = Bridge(self.desktop, 53281, self.dispatcher, directory / 'bridge.sqlite3', clock=self.clock)
        self.assertTrue(resumed.restored)
        self.assertIsNone(resumed.error)
        self.assertFalse(resumed.uncertain)
        resumed.close()

    def test_visible_timeout_still_fires_after_background_pause(self):
        self.desktop.foreground.side_effect = lambda: self.clock() < 10 or self.clock() >= 50
        self.desktop.reserved.return_value = False
        result = self.serve(duration=80)
        self.assertIn('carrier timeout', result['error'])
        self.assertGreaterEqual(self.clock(), 70)
        self.assertLess(self.clock(), 71)
        self.desktop.send_keys.assert_not_called()

    def test_focus_loss_during_input_preserves_uncertain_error(self):
        self.desktop.foreground.side_effect = lambda: not self.desktop.send_keys.called
        self.desktop.send_keys.return_value = False
        result = self.serve(duration=60)
        self.assertIn('input outcome uncertain', result['error'])
        self.assertLess(self.clock(), 1)
        self.assertEqual(self.desktop.send_keys.call_count, 1)
        directory = auto.journal_directory(self.state, 53281)
        resumed = Bridge(self.desktop, 53281, self.dispatcher, directory / 'bridge.sqlite3', clock=self.clock)
        self.assertTrue(resumed.uncertain)
        self.assertIn('input outcome uncertain', resumed.error)
        resumed.close()

    def test_first_live_test_is_finite_and_cannot_start_worker(self):
        self.desktop.send_keys.return_value = True
        result = self.serve(first_live_test=True)
        self.assertIsNone(result['error'])
        self.assertGreaterEqual(self.clock(), 1)
        self.assertEqual(self.desktop.send_keys.call_count, 1)


if __name__ == '__main__':
    unittest.main()
