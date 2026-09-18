import asyncio
import json
import tempfile
import time
import unittest
import threading
from unittest.mock import patch
from pathlib import Path
from lanes import Journal, RemoteError, Transport, run_lanes
from host import Lane


class Scheduling(unittest.IsolatedAsyncioTestCase):
    async def test_slow_and_failed_lanes_do_not_create_round_barrier(self):
        class Fake:
            revisions = [0]*3
            async def call(self, lane, operation, **args):
                if operation == 'observe':
                    return dict(revision=self.revisions[lane])
                self.revisions[lane] += 1
                return dict(revision=self.revisions[lane])
        slow_started = asyncio.Event()
        fast_done = asyncio.Event()
        fake = Fake()
        async def decide(lane, state):
            if lane == 0:
                slow_started.set()
                await fast_done.wait()
            elif lane == 1:
                await slow_started.wait()
            else:
                raise RuntimeError('failure is local')
            return 'advance'
        class TrackingJournal(Journal):
            async def record(self, event, lane, **fields):
                await super().record(event, lane, **fields)
                if event == 'committed' and lane == 1 and fields['receipt']['revision'] == 3:
                    fast_done.set()
        with tempfile.TemporaryDirectory() as root:
            journal = TrackingJournal(Path(root)/'journal')
            try:
                result = await asyncio.wait_for(run_lanes(fake, [0, 1, 2], decide, journal, steps=3), 2)
            finally:
                journal.close()
        self.assertEqual([r['completed'] for r in result], [3, 3, 0])
        self.assertEqual(result[2]['status'], 'stopped')
        self.assertLess(result[1]['timings'][-1]['completed_at'], result[0]['timings'][0]['completed_at'])

    async def test_slow_fsync_does_not_block_event_loop_or_send_early(self):
        entered, release = threading.Event(), threading.Event()
        calls = []
        class Fake:
            async def call(self, lane, operation, **args):
                calls.append(operation)
                return dict(revision=0)
        async def decide(*_):
            return 'advance'
        def slow_fsync(_):
            entered.set()
            if not release.wait(2):
                raise TimeoutError('event loop could not release fsync')
        with tempfile.TemporaryDirectory() as root:
            journal = Journal(Path(root)/'journal')
            try:
                with patch('lanes.os.fsync', slow_fsync):
                    task = asyncio.create_task(run_lanes(Fake(), [0], decide, journal, steps=1))
                    self.assertTrue(await asyncio.to_thread(entered.wait, 1))
                    await asyncio.sleep(.02)
                    self.assertEqual(calls, ['observe'])
                    release.set()
                    result = await task
                self.assertEqual(result[0]['completed'], 1)
                self.assertFalse(journal.unresolved)
            finally:
                release.set()
                journal.close()

    async def test_cancelled_prepare_is_durable_before_return(self):
        entered, release = threading.Event(), threading.Event()
        def slow_fsync(_):
            entered.set()
            release.wait(2)
        with tempfile.TemporaryDirectory() as root:
            path = Path(root)/'journal'
            journal = Journal(path)
            try:
                with patch('lanes.os.fsync', slow_fsync):
                    task = asyncio.create_task(journal.record('prepared', 0))
                    self.assertTrue(await asyncio.to_thread(entered.wait, 1))
                    task.cancel()
                    await asyncio.sleep(.02)
                    self.assertFalse(task.done())
                    release.set()
                    with self.assertRaises(asyncio.CancelledError):
                        await task
            finally:
                release.set()
                journal.close()
            reopened = Journal(path)
            try:
                self.assertEqual(reopened.unresolved, {'0'})
            finally:
                reopened.close()

    async def test_uncertain_write_not_replayed_on_resume(self):
        class Fake:
            writes = 0
            async def call(self, lane, operation, **args):
                if operation == 'observe':
                    return dict(revision=0)
                self.writes += 1
                raise TimeoutError('receipt lost after input')
        async def decide(*_):
            return 'advance'
        with tempfile.TemporaryDirectory() as root:
            path = Path(root)/'journal'
            fake = Fake()
            for _ in range(2):
                journal = Journal(path)
                try:
                    result = await run_lanes(fake, [0], decide, journal, steps=3)
                finally:
                    journal.close()
                self.assertEqual(result[0]['completed'], 0)
            self.assertEqual(fake.writes, 1)

    async def test_legacy_unresolved_identity_blocks_numeric_and_string_aliases(self):
        class NoCalls:
            async def call(self, *args, **kwargs):
                raise AssertionError('unreconciled lane must not reach transport')
        async def no_decision(*args):
            raise AssertionError('unreconciled lane must not reach decision')
        with tempfile.TemporaryDirectory() as root:
            path = Path(root)/'journal'
            for stored, resumed in ((0, '0'), ('0', 0)):
                with self.subTest(stored=stored, resumed=resumed):
                    path.write_text(json.dumps(dict(event='prepared', lane=stored))+'\n')
                    journal = Journal(path)
                    try:
                        result = await run_lanes(NoCalls(), [resumed], no_decision, journal, steps=1)
                        self.assertEqual(result[0]['status'], 'stopped')
                        self.assertIn('unreconciled prior write', result[0]['error'])
                    finally:
                        journal.close()

    async def test_mixed_type_aliases_rejected_before_admission(self):
        # Both IDs route to the same host lane, even though Python sets differ.
        with self.assertRaisesRegex(ValueError, 'distinct lanes'):
            await run_lanes(None, [0, '0'], None, None, steps=1)

    async def test_numeric_callbacks_and_canonical_journal_receipts(self):
        class Fake:
            async def call(self, lane, operation, **args):
                return dict(revision=0)
        seen = []
        async def decide(lane, observation):
            seen.append(lane)
            return 'advance'
        with tempfile.TemporaryDirectory() as root:
            path = Path(root)/'journal'
            journal = Journal(path)
            try:
                result = await run_lanes(Fake(), [0], decide, journal, steps=1)
                self.assertEqual(result[0]['status'], 'complete')
                self.assertFalse(journal.unresolved)
                await journal.record('prepared', 0)
                self.assertEqual(journal.unresolved, {'0'})
                await journal.record('committed', '0')
                self.assertFalse(journal.unresolved)
            finally:
                journal.close()
            self.assertEqual(seen, [0])
            self.assertIs(type(seen[0]), int)
            events = [json.loads(line) for line in path.read_text().splitlines()]
            self.assertEqual({event['lane'] for event in events}, {'0'})
            reopened = Journal(path)
            try:
                self.assertFalse(reopened.unresolved)
            finally:
                reopened.close()

    async def test_legacy_mixed_type_commit_resolves_only_matching_lane(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root)/'journal'
            events = [dict(event='prepared', lane=0), dict(event='prepared', lane=1),
                      dict(event='committed', lane='0')]
            path.write_text(''.join(json.dumps(event)+'\n' for event in events))
            journal = Journal(path)
            try:
                self.assertEqual(journal.unresolved, {'1'})
            finally:
                journal.close()

    async def test_transport_demultiplexes_without_waiting_for_slow_reply(self):
        code = '''import sys,json,threading,time
lock=threading.Lock()
def answer(r):
 time.sleep(.3 if r['lane']==0 else .01)
 with lock: print(json.dumps({'id':r['id'],'result':r['lane']}),flush=True)
for line in sys.stdin: threading.Thread(target=answer,args=(json.loads(line),)).start()
'''
        transport = await Transport.open(['python3', '-u', '-c', code])
        try:
            slow = asyncio.create_task(transport.call(0, 'observe'))
            fast = await transport.call(1, 'observe')
            self.assertEqual(fast, 1)
            self.assertFalse(slow.done())
            self.assertEqual(await slow, 0)
        finally:
            await transport.close()

    async def test_timeout_quarantines_only_affected_lane(self):
        class Fake:
            async def call(self, lane, operation, **args):
                if lane == 0:
                    raise TimeoutError('transport deadline')
                return dict(revision=0)
        async def decide(*_):
            return 'advance'
        with tempfile.TemporaryDirectory() as root:
            journal = Journal(Path(root)/'journal')
            try:
                result = await run_lanes(Fake(), [0, 1], decide, journal, steps=1)
            finally:
                journal.close()
        self.assertEqual([r['status'] for r in result], ['stopped', 'complete'])


class HostBoundary(unittest.TestCase):
    def test_stale_and_invalid_actions_never_reach_companion(self):
        lane = Lane({}, {})
        class NoCalls:
            def js(self, code):
                raise AssertionError('input should not be sent')
        lane.companion = NoCalls()
        lane.state = lambda: dict(revision=4, buttons={'advance': [10, 10]})
        for action in [dict(revision=3, choice='advance'), dict(revision=4, choice='invalid')]:
            with self.assertRaises(ValueError):
                lane.execute(dict(operation='act', **action))

    def test_uncertain_companion_failure_quarantines_lane(self):
        lane = Lane({}, {})
        class LostReply:
            calls = 0
            def js(self, code):
                self.calls += 1
                raise TimeoutError('lost response')
        lane.companion = LostReply()
        lane.state = lambda: dict(revision=4, buttons={'advance': [10, 10]})
        with self.assertRaises(TimeoutError):
            lane.execute(dict(operation='act', revision=4, choice='advance'))
        with self.assertRaisesRegex(RuntimeError, 'quarantined'):
            lane.execute(dict(operation='act', revision=4, choice='advance'))
        self.assertEqual(lane.companion.calls, 1)

if __name__ == '__main__':
    unittest.main()
