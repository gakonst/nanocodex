import pathlib
import subprocess
import tempfile
import unittest

from transport.protocol import Frame, Link, keys
from transport.driver import Pump


class BinaryCarrierTests(unittest.TestCase):
    def test_msb_encoding_bounds_and_octal_default(self):
        packet = Frame(17, 1, payload=bytes(range(96))).encode()
        wire = keys(packet, encoding='binary')
        self.assertEqual(len(wire), 890)
        self.assertEqual(wire[0], 'F24')
        self.assertEqual(wire[-1], 'F24')
        self.assertEqual(set(wire), {'F19', 'F23', 'F24'})
        decoded = bytes(int(''.join('1' if key == 'F23' else '0'
                                   for key in wire[i:i+8]), 2)
                        for i in range(1, len(wire)-1, 8))
        self.assertEqual(decoded, packet)
        self.assertEqual(keys(packet), keys(packet, encoding='octal'))
        self.assertEqual(len(keys(packet)), 335)
        with self.assertRaises(ValueError):
            keys(packet, encoding='unknown')
        with self.assertRaises(ValueError):
            keys(packet[:-1] + bytes([packet[-1] ^ 1]), encoding='binary')

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

    def test_pump_defaults_and_configuration_bounds(self):
        args = (Link(17), lambda: True, lambda key: True, lambda: 0, lambda: True)
        default = Pump(*args)
        self.assertEqual(default.encoding, 'octal')
        self.assertEqual(default.burst_size, 335)
        for size in (0, 891, 335.5):
            with self.assertRaises(ValueError):
                Pump(*args, burst_size=size)
        with self.assertRaises(ValueError):
            Pump(*args, encoding='unknown')


if __name__ == '__main__':
    unittest.main()
