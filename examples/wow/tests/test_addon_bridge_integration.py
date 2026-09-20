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

    def test_native_client_roster_history_create_send_stop_and_reconnect(self):
        """Real addon client state through NC1, Bridge journals and Dispatcher."""
        class ClientBackend(Backend):
            def __init__(self):
                super().__init__()
                self.created = 0
                self.cancelled = []
                self.resumed = 0
                self.history_queries = []
                self.workspace_reads = 0
                self.history = 'Earlier question |Ω\n' + 'History Ω ' * 1750
                self.threads = [dict(id='fixture-thread', title='First chat', status='unknown'),
                                dict(id='fixture-other', title='Other chat', status='unknown')]
                # Exercise multiple application pages, not just carrier chunks.
                self.threads += [dict(id=f'fixture-{i}', title=f'Thread {i} ' + 'x' * 700,
                                      status='unknown') for i in range(24)]

            def resume(self): self.resumed += 1

            def handle(self, method, path, query, data):
                if path == '/api/send': return super().handle(method, path, query, data)
                if path == '/api/status': return dict(connected=True)
                if path == '/api/workspace':
                    self.workspace_reads += 1
                    return dict(projects=[dict(id='fixture-project', name='Fixture project')],
                                threads=[dict(row, project_id='fixture-project', closed=False) for row in self.threads])
                if path == '/api/threads':
                    return dict(threads=[] if query.get('closed') == ['true'] else self.threads)
                if path == '/api/messages':
                    self.history_queries.append(query)
                    thread = query['thread_id'][0]
                    if thread == 'fixture-thread':
                        text = 'Oldest question' if 'before' in query else self.history
                        return dict(message_details=[dict(role='user', text=text)],
                                    first_cursor='1' if 'before' in query else '10',
                                    has_more='before' not in query)
                    return dict(message_details=[dict(role='assistant', text='Other conversation' if thread == 'fixture-other' else 'New empty chat')],
                                first_cursor='1', has_more=False)
                if path == '/api/threads/create':
                    self.created += 1
                    self.threads.append(dict(id='fixture-created', title=data['title'], status='unknown'))
                    return dict(project_id='fixture-project', thread_id='fixture-created', title=data['title'], status='created')
                if path == '/api/turns/cancel':
                    self.cancelled.append(data)
                    return dict(thread_id=data['thread_id'], turn_id=data['turn_id'],
                                receipt=dict(turn_id=data['turn_id'], state='cancelling'))
                raise AssertionError((method, path, query, data))

        with tempfile.TemporaryDirectory() as directory:
            desktop = LuaDesktop()
            self.addCleanup(desktop.close)
            desktop.replay = True
            backend = ClientBackend()
            dispatcher = Dispatcher(backend, Path(directory) / 'dispatch.sqlite3')
            self.addCleanup(dispatcher.close)
            bridge = Bridge(desktop, 17, dispatcher, Path(directory) / 'bridge.sqlite3')
            self.addCleanup(bridge.close)

            def drive_until(predicate):
                for _ in range(2400):
                    bridge.step()
                    self.assertIsNone(bridge.error)
                    bridge.work_once()
                    if predicate(): return
                self.fail('native client did not reach expected state: ' + repr(desktop.state))

            self.assertTrue(desktop.command('refresh')['submitted'])
            drive_until(lambda: desktop.state['threads'] == 26 and desktop.state['client']['account'] == 'Account connected')
            snapshots = [json.loads(row[0]).get("snapshot", {}) for row in dispatcher.journal.db.execute("SELECT record FROM dispatch_requests")]
            self.assertTrue(any(snapshot.get("kind") == "projects" and len(snapshot["pages"]) > 1 for snapshot in snapshots))
            self.assertEqual(backend.workspace_reads, 1, 'roster pages share one immutable backend read')
            self.assertTrue(desktop.command('select1')['submitted'])
            drive_until(lambda: desktop.state['client']['history'] == 'You:\n' + backend.history)
            self.assertEqual(len(backend.history_queries), 1, 'local history pages share one immutable backend read')
            self.assertIn('Earlier question ||Ω', desktop.state['text'])
            self.assertTrue(desktop.command('earlier')['submitted'])
            drive_until(lambda: desktop.state['client']['history'].startswith('You:\nOldest question\n\n'))
            self.assertEqual(backend.history_queries[-1]['before'], ['10'])

            self.assertTrue(desktop.command('sendthread')['submitted'])
            self.assertFalse(desktop.command('sendthread')['submitted'], 'pending click must not duplicate a send')
            drive_until(lambda: len(backend.sent) == 1 and 'Accepted remotely' in desktop.state['transport'])
            backend.rows = [dict(type='event', cursor='1', turn_id='fixture-turn-1',
                                event=dict(type='assistant.delta', payload=dict(text='Live partial |Ω',
                                      model_call_index=0, item_id='answer', phase='final_answer')))]
            drive_until(lambda: 'Live partial ||Ω' in desktop.state['text'])
            self.assertTrue(desktop.command('stop')['submitted'])
            drive_until(lambda: len(backend.cancelled) == 1 and 'Stop requested' in desktop.state['transport'])
            self.assertEqual(backend.cancelled[0]['thread_id'], 'fixture-thread')
            self.assertEqual(backend.cancelled[0]['turn_id'], 'fixture-turn-1')

            self.assertTrue(desktop.command('select2')['submitted'])
            drive_until(lambda: desktop.state['text'] == 'Assistant:\nOther conversation')
            backend.rows.append(dict(type='turn_completed', cursor='2', turn_id='fixture-turn-1', final_message='Background final'))
            drive_until(lambda: desktop.state['status'] == 'Completed')
            self.assertEqual(desktop.state['text'], 'Assistant:\nOther conversation', 'background completion must preserve selected history')
            self.assertEqual(len(backend.sent), 1)

            self.assertTrue(desktop.command('new')['submitted'])
            drive_until(lambda: desktop.state['client'].get('thread') == 'fixture-created' and desktop.state['text'] == 'Assistant:\nNew empty chat')
            self.assertEqual(backend.created, 1, 'carrier replay must not duplicate creation')
            self.assertTrue(desktop.command('reconnect')['submitted'])
            drive_until(lambda: backend.resumed == 1 and not desktop.state['pending'])
            self.assertFalse(bridge.evidence()['model_roundtrip_proven'])
