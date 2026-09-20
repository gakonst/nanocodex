"""Real Lua addon + Python journal/dispatcher/stream pipeline; no game/account IO."""
import json
from pathlib import Path
import select
import shutil
import subprocess
import tempfile
import unittest
from types import SimpleNamespace

from transport.calibrate import decode_grid
from transport.wayland import Desktop

from transport.daemon import Bridge
from transport.dispatch import Dispatcher


class LuaDesktop:
    key_encoding = 'octal'

    def __init__(self):
        self.process = subprocess.Popen(['lua', 'transport/integration_peer.lua'],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, bufsize=1)
        self.state = self.command('capture')
        self.replay = False
        self.adapter = Desktop('fixture', 'Wow', 0, 0, 4, run=self.run, input_backend='native-chord')

    def command(self, command):
        self.process.stdin.write(command + '\n')
        self.process.stdin.flush()
        if not select.select([self.process.stdout], [], [], 3)[0]:
            raise AssertionError('Lua peer did not respond within three seconds')
        line = self.process.stdout.readline()
        if not line:
            raise AssertionError('Lua peer exited: ' + self.process.stderr.read())
        self.state = json.loads(line)
        return self.state

    def foreground(self): return True
    def reserved(self): return True
    def capture(self):
        state = self.command('capture')
        cells = state['cells']
        assert len(cells) == 1024
        packet = decode_grid(lambda x, y: (int(cells[y // 4 * 32 + x // 4]) * 255,) * 3, 0, 0, 4)
        assert packet == bytes.fromhex(state['packet'])
        return packet

    def run(self, args, **kwargs):
        if args[:2] == ['hyprctl', '-j']:
            value = [] if args[2] == 'binds' else {'address': 'fixture', 'class': 'Wow'}
            return SimpleNamespace(stdout=json.dumps(value).encode())
        assert args[0].endswith('/carrier-keys')
        if args[-1] == '--describe':
            return SimpleNamespace(stdout=json.dumps(dict(schema=1, modifiers=['CTRL', 'SHIFT'],
                keys=['F' + str(n) for n in (1, 2, 3, 5, 6, 7, 8, 9, 10, 11)])).encode())
        assert args[1] == '0'
        self.command('keys ' + ' '.join(args[2:]))
        if self.replay:  # Lost ACK: the exact carrier frame arrives twice.
            self.command('keys ' + ' '.join(args[2:]))
        return SimpleNamespace(stdout=b'')

    def send_keys(self, keys):
        return self.adapter.send_keys(keys)

    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=3)
        self.process.stdout.close()
        self.process.stderr.close()


class Backend:
    def __init__(self):
        self.store = self
        self.rows = []
        self.sent = []

    def handle(self, method, path, query, data):
        assert (method, path) == ('POST', '/api/send')
        self.sent.append(data)
        return dict(thread_id='fixture-thread', turn_id='fixture-turn-' + str(len(self.sent)), status='accepted')

    def events(self, *args): return self.rows


@unittest.skipUnless(shutil.which('lua'), 'Lua interpreter required')
class AddonBridgeIntegrationTests(unittest.TestCase):
    def test_explicit_repeated_ask_stream_receipts_replay_and_journal(self):
        with tempfile.TemporaryDirectory() as directory:
            desktop = LuaDesktop()
            self.addCleanup(desktop.close)
            backend = Backend()
            dispatcher = Dispatcher(backend, Path(directory) / 'dispatch.sqlite3')
            self.addCleanup(dispatcher.close)
            bridge = Bridge(desktop, 17, dispatcher, Path(directory) / 'bridge.sqlite3')
            self.addCleanup(lambda: bridge.close())

            def pump_until(predicate):
                for _ in range(300):
                    bridge.step()
                    self.assertIsNone(bridge.error)
                    if predicate(): return
                self.fail('carrier did not reach expected state: ' + repr(desktop.state))

            def drain():
                pump_until(lambda: bridge.store.db.execute(
                    'SELECT count(*) FROM bridge_outputs WHERE delivered=0').fetchone()[0] == 0)

            partial = 'Hello |Ω ' * 30  # More than one 88-byte carrier payload.
            for number in (1, 2):
                self.assertTrue(desktop.command('ask')['submitted'])
                self.assertFalse(desktop.command('ask')['submitted'])
                self.assertEqual(desktop.state['reason'], 'busy')
                pump_until(lambda: not desktop.state['pending'])
                self.assertEqual(bridge.store.db.execute(
                    'SELECT count(*) FROM bridge_requests').fetchone()[0], number)
                bridge.work_once()
                drain()
                self.assertIn('Accepted remotely; awaiting reply.', desktop.state['transport'])
                self.assertEqual(len(backend.sent), number)
                self.assertEqual(backend.sent[-1]['idempotency_key'], f'ncw:00000011:{number:04x}')
                desktop.replay = True
                backend.rows = [dict(type='event', cursor='1', turn_id=f'fixture-turn-{number}',
                    event=dict(type='assistant.delta', payload=dict(text=partial,
                        model_call_index=0, item_id='answer', phase='final_answer')))]
                bridge.work_once()
                bridge.step()
                # Restart while the first stream message is only partially assembled
                # by the actual Lua addon. Keep both journals and the same peer.
                self.assertNotEqual(desktop.state['text'], partial.replace('|', '||'))
                self.assertGreater(desktop.state['incoming_bytes'], 0)
                self.assertGreater(bridge.store.db.execute(
                    'SELECT count(*) FROM bridge_outputs WHERE delivered=0').fetchone()[0], 0)
                bridge.close()
                bridge = Bridge(desktop, 17, dispatcher, Path(directory) / 'bridge.sqlite3')
                self.assertTrue(bridge.restored)
                drain()
                self.assertEqual(desktop.state['text'], partial.replace('|', '||'))
                self.assertEqual(desktop.state['status'], 'Streaming reply…')
                backend.rows.append(dict(type='turn_completed', cursor='2',
                    turn_id=f'fixture-turn-{number}', final_message=partial + '!'))
                bridge.work_once()
                drain()
                self.assertEqual(desktop.state['text'], partial.replace('|', '||') + '!')
                self.assertEqual(desktop.state['status'], 'Completed')
                bridge.work_once()
                self.assertEqual(len(backend.sent), number)
                self.assertFalse(bridge.evidence()['model_roundtrip_proven'])
                self.assertGreater(desktop.state['frames'], 0)
            rows = bridge.store.db.execute('SELECT mid,rid,body FROM bridge_requests ORDER BY mid').fetchall()
            self.assertEqual([row[0] for row in rows], [1, 2])
            self.assertEqual(rows[0][2], rows[1][2])  # Identical bytes, separate explicit intents.
