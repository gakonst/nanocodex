"""Capacity regression tests: local SQLite and fake IO only."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

from transport.daemon import Bridge, MAX_OUTPUTS, MAX_PENDING


def output(event):
    return dict(kind='ack', value='retained', request_id='request',
                state='local_queued', event_id=event)


class OutputCapacityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / 'bridge.sqlite3'
        self.desktop = Mock()
        self.desktop.key_encoding = 'octal'
        self.dispatcher = Mock()
        self.bridge = Bridge(self.desktop, 17, self.dispatcher, self.path)
        self.addCleanup(lambda: self.bridge.close())
        with self.bridge.store.db:
            self.bridge.store.db.execute('INSERT INTO bridge_requests VALUES(?,?,?,?,?)',
                                         (17, 1, 'request', b'{}', 'waiting'))

    def seed(self, count, delivered=1):
        with self.bridge.store.db:
            self.bridge.store.db.executemany('INSERT INTO bridge_outputs VALUES(?,?,?,?)',
                ((17, str(i), json.dumps(output(str(i))), delivered) for i in range(count)))

    def rows(self):
        return self.bridge.store.db.execute('SELECT * FROM bridge_outputs ORDER BY rowid').fetchall()

    def test_historical_capacity_stops_visibly_and_survives_restart(self):
        for count in (MAX_OUTPUTS - 4, MAX_OUTPUTS):
            with self.subTest(count=count):
                with self.bridge.store.db:
                    self.bridge.store.db.execute('DELETE FROM bridge_outputs')
                self.seed(count)
                self.bridge.error = None
                self.bridge.stop.clear()
                self.bridge.active = {'output': output('0'), 'mid': 12, 'index': 0}
                self.bridge.link.send(b'pending')
                self.bridge.uncertain = True
                self.bridge.uncertain_seq = self.bridge.link.tx
                self.bridge.next_mid = 13
                before = self.rows()
                pending = self.bridge.link.pending
                active = self.bridge.active.copy()
                self.assertFalse(self.bridge.work_once())
                self.assertEqual(self.rows(), before)
                self.assertIn('capacity reached', self.bridge.evidence()['error'])
                self.assertIn('reconcile', self.bridge.error)
                self.assertEqual(self.bridge.evidence()['lifecycle'], 'stopped')
                self.assertTrue(self.bridge.stop.is_set())
                self.bridge.close()
                self.bridge = Bridge(self.desktop, 17, self.dispatcher, self.path)
                self.assertIsNotNone(self.bridge.error)
                self.assertEqual(self.bridge.active, active)
                self.assertEqual(self.bridge.link.pending, pending)
                self.assertEqual(self.bridge.next_mid, 13)
                self.assertTrue(self.bridge.uncertain)
                self.assertEqual(self.bridge.uncertain_seq, self.bridge.link.tx)
                self.assertFalse(self.bridge.work_once())
                self.assertFalse(self.bridge.step())
                self.assertEqual(self.bridge.store.db.execute('SELECT state FROM bridge_requests').fetchone(), ('waiting',))
                self.assertEqual(self.rows(), before)
                self.bridge.link.pending = None
        self.dispatcher.poll.assert_not_called()
        self.dispatcher.dispatch.assert_not_called()
        self.desktop.send_keys.assert_not_called()

    def test_batch_larger_than_reserved_space_is_atomic(self):
        with patch('transport.daemon.MAX_OUTPUTS', 10):
            self.seed(5)
            before = self.rows()
            self.dispatcher.poll.return_value = [output(str(i)) for i in range(12)]
            self.assertFalse(self.bridge.work_once())
            self.assertEqual(self.rows(), before)
            self.assertIn('capacity reached', self.bridge.error)
            self.assertEqual(self.bridge.store.db.execute('SELECT state FROM bridge_requests').fetchone(), ('waiting',))
            self.assertFalse(self.bridge.work_once())
            self.dispatcher.poll.assert_called_once_with('request')

    def test_replayed_delivered_outputs_are_not_reinserted(self):
        with patch('transport.daemon.MAX_OUTPUTS', 10):
            self.seed(5)
            before = self.rows()
            self.dispatcher.poll.return_value = [output(str(i)) for i in range(5)] * 2
            self.assertTrue(self.bridge.work_once())
            self.assertEqual(self.rows(), before)
            self.assertIsNone(self.bridge.error)
            self.bridge._advance_output()
            self.assertIsNone(self.bridge.active)
            self.assertIsNone(self.bridge.link.pending)

    def test_status_output_cannot_overflow_full_journal(self):
        self.seed(MAX_OUTPUTS)
        before = self.rows()
        self.dispatcher.backend.handle.return_value = {'connected': False}
        self.bridge.check_status()
        self.assertEqual(self.rows(), before)
        self.assertIn('capacity reached', self.bridge.error)

    def test_pending_backpressure_remains_recoverable(self):
        self.seed(MAX_PENDING - 4, delivered=0)
        self.assertFalse(self.bridge.work_once())
        self.assertIsNone(self.bridge.error)
        self.assertFalse(self.bridge.stop.is_set())
        self.dispatcher.poll.assert_not_called()
        with self.bridge.store.db:
            self.bridge.store.db.execute('UPDATE bridge_outputs SET delivered=1')
        self.dispatcher.poll.return_value = []
        self.assertTrue(self.bridge.work_once())
        self.dispatcher.poll.assert_called_once_with('request')


if __name__ == '__main__':
    unittest.main()
