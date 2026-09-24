import pathlib
import subprocess
import tempfile
import unittest

from transport.protocol import Frame, Link, keys
from transport.driver import Pump


class BinaryCarrierTests(unittest.TestCase):
    def test_python_lua_interop_all_byte_values_and_lifecycle(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / 'wire.txt'
            path.write_text('\n'.join(' '.join(keys(Frame(17, seq, payload=bytes(
                range(start, min(start+96, 256)))).encode(), encoding='binary'))
                for seq, start in enumerate(range(0, 256, 96), 4)))
            result = subprocess.run(['lua', 'transport/test_binary_transport.lua', str(path)],
                                    capture_output=True, text=True, check=True)
        self.assertIn('all assertions passed', result.stdout)

    def test_single_batch_ack_and_next_packet(self):
        link = Link(17)
        link.send(b'x' * 96)
        batches = []
        pump = Pump(link, lambda: True, None, lambda: 0, lambda: True,
                    send_keys=lambda batch: batches.append(batch) or True,
                    burst_size=890, encoding='binary')
        ready = Frame(17, ready=True).encode()
        pump.observe(ready)
        pump.observe(ready)
        self.assertTrue(pump.tick())
        self.assertEqual(batches, [keys(link.packet(), encoding='binary')])
        self.assertEqual(len(batches[0]), 890)
        self.assertEqual(pump.stream, [])
        self.assertIsNotNone(link.pending)
        ack = Frame(17, ack=1, ready=True).encode()
        pump.observe(ack)
        pump.observe(ack)
        self.assertIsNone(link.pending)
        link.send(b'next')
        self.assertTrue(pump.tick())
        self.assertEqual(batches[-1], keys(link.packet(), encoding='binary'))

if __name__ == '__main__':
    unittest.main()
