import io
import json
import queue
import subprocess
import tempfile
from pathlib import Path
import threading
import time
import unittest
from unittest.mock import Mock, patch

import host


class LaneTests(unittest.TestCase):
    def lane(self, **options):
        lane = host.Lane({}, options)
        lane.companion = Mock()
        lane.state = Mock(return_value=dict(revision=4, buttons={'advance': [10, 10]}))
        return lane

    def test_observe_reads_state_after_delay_and_capture(self):
        lane = self.lane(test_observe_delay_ms=1)
        events = []
        def state():
            events.append('state')
            return dict(revision=len(events))
        lane.state = state
        lane.companion.js.side_effect = lambda code: events.append('js') or {'capture': True}
        result = lane.execute(dict(operation='observe'))
        self.assertEqual(events, ['state', 'js', 'js', 'state'])
        self.assertEqual(result['revision'], 4)
        self.assertEqual(result['capture'], {'capture': True})

    def test_uncertain_failures_close_and_prevent_reuse(self):
        for phase in ('delay', 'capture', 'state', 'act', 'receipt'):
            with self.subTest(phase=phase):
                lane = self.lane(**({'test_observe_delay_ms': 1} if phase == 'delay' else {}))
                error = ValueError('malformed protocol or state')
                if phase == 'state':
                    lane.state.side_effect = error
                elif phase == 'receipt':
                    lane.state.side_effect = [lane.state.return_value, error]
                else:
                    lane.companion.js.side_effect = error
                request = dict(operation='act' if phase in ('act', 'receipt') else 'observe',
                               revision=4, choice='advance')
                with self.assertRaisesRegex(ValueError, 'malformed'):
                    lane.execute(request)
                self.assertTrue(lane.failed)
                lane.companion.close.assert_called_once()
                calls = lane.companion.js.call_count
                with self.assertRaisesRegex(RuntimeError, 'quarantined'):
                    lane.execute(request)
                self.assertEqual(lane.companion.js.call_count, calls)

    def test_validated_rejections_allow_later_observation(self):
        lane = self.lane()
        for request in (dict(operation='act', revision=3, choice='advance'),
                        dict(operation='act', revision=4, choice='missing'),
                        dict(operation='unknown')):
            with self.assertRaises(ValueError):
                lane.execute(request)
        lane.companion.js.assert_not_called()
        lane.companion.close.assert_not_called()
        self.assertFalse(lane.failed)
        lane.execute(dict(operation='observe'))

    def test_initialization_failure_closes_published_companion(self):
        companion = Mock()
        companion.initialize.side_effect = TimeoutError('initialize stalled')
        lane = host.Lane({}, {})
        with patch.object(host, 'Companion', return_value=companion):
            with self.assertRaises(TimeoutError):
                lane.execute(dict(operation='observe'))
        self.assertTrue(lane.failed)
        companion.close.assert_called_once()

    def test_disconnect_during_initialization_unblocks_worker(self):
        started, closed = threading.Event(), threading.Event()
        companion = Mock()
        def initialize(_):
            started.set()
            if not closed.wait(2):
                raise AssertionError('disconnect did not close companion')
            raise RuntimeError('closed')
        companion.initialize.side_effect = initialize
        companion.close.side_effect = closed.set
        class Input:
            calls = 0
            def readline(self, _):
                self.calls += 1
                if self.calls == 1:
                    return b'{"id":1,"lane":"a","operation":"observe"}\n'
                if not started.wait(2):
                    raise AssertionError('worker did not initialize')
                return b''
        output = io.StringIO()
        with patch.object(host, 'Companion', return_value=companion), \
             patch.object(host.sys, 'stdin', Mock(buffer=Input())), \
             patch.object(host.sys, 'stdout', output):
            before = time.monotonic()
            host.serve(dict(lanes={'a': dict(pid=1)}))
            self.assertLess(time.monotonic()-before, 1)
        self.assertTrue(closed.is_set())

    def test_disconnect_closes_all_lanes_concurrently(self):
        closed = []
        def close(lane):
            time.sleep(.15)
            closed.append(lane)
        with patch.object(host.Lane, 'close', close), \
             patch.object(host.sys, 'stdin', Mock(buffer=io.BytesIO(b''))):
            before = time.monotonic()
            host.serve(dict(lanes={str(i): dict(pid=i+1) for i in range(8)}))
            self.assertLess(time.monotonic()-before, .8)
        self.assertEqual(len(closed), 8)

    def test_real_malformed_companion_is_terminated(self):
        with tempfile.TemporaryDirectory() as root:
            script = Path(root)/'companion'
            script.write_text('#!/usr/bin/env python3\nimport sys,time\n'
                              'sys.stdin.readline()\nprint("not json", flush=True)\ntime.sleep(30)\n')
            script.chmod(0o700)
            lane = host.Lane(dict(companion=str(script), env={}),
                             dict(pid=1, log=str(Path(root)/'log')))
            before = time.monotonic()
            try:
                with self.assertRaisesRegex(RuntimeError, 'protocol failure'):
                    lane.execute(dict(operation='observe'))
                self.assertLess(time.monotonic()-before, 3)
                self.assertTrue(lane.failed)
                self.assertIsNotNone(lane.companion.process.poll())
            finally:
                lane.close()
                lane.companion.process.stdin.close()
                lane.companion.process.stdout.close()

    def test_closed_lane_cannot_start_companion(self):
        lane = host.Lane({}, {})
        lane.close()
        with patch.object(host, 'Companion') as factory:
            with self.assertRaisesRegex(RuntimeError, 'quarantined'):
                lane.execute(dict(operation='observe'))
            factory.assert_not_called()


class ProtocolTests(unittest.TestCase):
    def companion(self):
        companion = host.Companion.__new__(host.Companion)
        companion.messages = queue.Queue(maxsize=16)
        companion.seq = 0
        companion.closed = threading.Event()
        companion.close_lock = threading.Lock()
        companion.failure = None
        companion.process = Mock(stdin=io.StringIO())
        return companion

    def test_reader_malformed_and_eof_wake_waiting_rpc(self):
        for line in ('not json\n', '[]\n', '{"jsonrpc":"1.0"}\n', ''):
            with self.subTest(line=line):
                companion = self.companion()
                waiting, result = threading.Event(), []
                class Writer(io.StringIO):
                    def flush(self):
                        waiting.set()
                companion.process.stdin = Writer()
                companion.process.stdout = io.StringIO(line)
                def rpc():
                    try:
                        companion.js('capture')
                    except Exception as exc:
                        result.append(exc)
                worker = threading.Thread(target=rpc, daemon=True)
                worker.start()
                self.assertTrue(waiting.wait(1))
                companion.read()
                worker.join(1)
                self.assertFalse(worker.is_alive())
                self.assertIsInstance(result[0], RuntimeError)
                self.assertIn('protocol failure', str(result[0]))

    def test_reader_queue_overflow_does_not_block(self):
        companion = self.companion()
        companion.process.stdout = io.StringIO((json.dumps(dict(jsonrpc='2.0', method='notice'))+'\n')*17)
        reader = threading.Thread(target=companion.read, daemon=True)
        reader.start()
        reader.join(1)
        self.assertFalse(reader.is_alive())
        self.assertIsNotNone(companion.failure)
        with self.assertRaises(RuntimeError):
            companion.js('capture')

    def test_rpc_rejects_wrong_ids_and_malformed_results(self):
        for reply in (dict(id=2, result={}), dict(id=True, result={}), dict(id=1),
                      dict(id=1, result=[]), dict(id=1, result={'isError': True}),
                      dict(id=1, error={'message': 'failed'})):
            with self.subTest(reply=reply):
                companion = self.companion()
                companion.messages.put(reply)
                with self.assertRaises(RuntimeError):
                    companion.js('capture')

    def test_notifications_can_precede_success(self):
        companion = self.companion()
        companion.messages.put(dict(method='notice'))
        companion.messages.put(dict(id=1, result={'content': []}))
        self.assertEqual(companion.js('capture'), {'content': []})

    def test_close_kills_with_bounded_waits_and_wakes_rpc(self):
        companion = self.companion()
        companion.process.poll.return_value = None
        companion.process.wait.side_effect = subprocess.TimeoutExpired('companion', 1)
        companion.close()
        companion.process.terminate.assert_called_once()
        companion.process.kill.assert_called_once()
        self.assertEqual([call.kwargs for call in companion.process.wait.call_args_list],
                         [{'timeout': 1}, {'timeout': 1}])
        self.assertIsInstance(companion.messages.get_nowait(), RuntimeError)
        companion.close()
        companion.process.kill.assert_called_once()


if __name__ == '__main__':
    unittest.main()
