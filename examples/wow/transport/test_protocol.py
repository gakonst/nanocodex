import pathlib
import subprocess
import unittest
from protocol import Frame, Link, keys, raster, decode_pixels
from driver import Pump

class ProtocolTests(unittest.TestCase):
    def test_lua_interop_and_lifecycle(self):
        p = subprocess.run(['lua', 'transport/test_transport.lua'], capture_output=True, text=True, check=True)
        expected = Frame(0xffffffff,65535,65534,b'\0\x80\xffUTF8:\xc3\xa9',True)
        self.assertEqual(p.stdout.splitlines()[0], expected.encode().hex())
        self.assertEqual(Frame.decode(bytes.fromhex(p.stdout.splitlines()[0])), expected)

    def test_all_byte_values_pixels(self):
        for start in range(0,256,96):
            packet=Frame(17,1,0,bytes(range(start,min(start+96,256))),True).encode()
            cells=raster(packet)
            def sample(x,y):
                c=cells[y//4][x//4]
                return (c,c,c)
            self.assertEqual(decode_pixels(sample,0,0,4),packet)
            self.assertEqual(len(keys(packet)),len(packet)*3+2)
        with self.assertRaises(ValueError): decode_pixels(lambda x,y:(128,128,128),0,0,4)

    def test_crc_every_single_bit(self):
        packet=Frame(1,1,0,b'x'*96,True).encode()
        for i in range(len(packet)*8):
            p=bytearray(packet); p[i//8]^=1<<(i%8)
            with self.assertRaises(ValueError): Frame.decode(p)

    def test_stop_wait_and_rejected_delivery(self):
        got=[]
        a,b=Link(42),Link(42,lambda p,s:got.append((p,s)))
        a.send(b'hello'); packet=a.packet()
        with self.assertRaises(ValueError): a.send(b'busy')
        b.receive(packet); b.receive(packet)
        self.assertEqual(got,[(b'hello',1)])
        a.receive(b.packet()); self.assertIsNone(a.pending)
        for frame in [Frame(43),Frame(42,3),Frame(42,1,0,b'other'),Frame(42,0,1)]:
            with self.assertRaises(ValueError): b.receive(frame.encode())
        c=Link(42,lambda p,s:False)
        with self.assertRaises(ValueError): c.receive(packet)
        self.assertEqual(c.rx,0)
        a.tx=65535
        with self.assertRaises(ValueError): a.send(b'exhausted')
        with self.assertRaises(ValueError): Frame(1,0,0,b'bad').encode()
        with self.assertRaises(ValueError): Frame(1,1,0,b'x'*97).encode()

    def test_focus_readiness_and_uncertain_input(self):
        now=[0.0]; foreground=[False]; reserved=[True]; sent=[]
        a=Link(1); a.send(b'test')
        pump=Pump(a,lambda:foreground[0],lambda k:sent.append(k) or True,lambda:now[0],lambda:reserved[0],interval=.04)
        f=Frame(1,ready=True).encode()
        pump.observe(f); self.assertFalse(pump.tick())
        pump.observe(f); self.assertFalse(pump.tick())
        foreground[0]=True; self.assertTrue(pump.tick()); self.assertEqual(sent,['F21'])
        now[0]+=.05; foreground[0]=False; self.assertFalse(pump.tick()); self.assertEqual(pump.stream,[])
        foreground[0]=True; self.assertTrue(pump.tick()); self.assertEqual(sent[-1],'F21')
        now[0]+=1; self.assertFalse(pump.tick()) # stale screenshot
        pump.observe(f); reserved[0]=False; self.assertFalse(pump.tick())
        reserved[0]=True; pump.send_key=lambda k:None
        self.assertFalse(pump.tick()); self.assertIn('uncertain',pump.error)

    def test_pump_roundtrip(self):
        now=[0.0]; sent=[]; a=Link(9); a.send(b'proof')
        p=Pump(a,lambda:True,lambda k:sent.append(k) or True,lambda:now[0],lambda:True,interval=.04)
        for _ in range(200):
            f=Frame(9,ready=True).encode(); p.observe(f); p.observe(f)
            p.tick(); now[0]+=.04
            if sent and sent[-1]=='F22': break
        self.assertEqual(sent,keys(a.packet()))
        ack=Frame(9,ack=1,ready=True).encode(); p.observe(ack); p.observe(ack)
        self.assertIsNone(a.pending)
        now[0]+=2; p.observe(ack); self.assertFalse(p.tick())

    def test_lost_ack_retransmit_and_retry_bound(self):
        now=[0.0]; sent=[]; delivered=[]
        a=Link(9,lambda data,seq:delivered.append(data))
        p=Pump(a,lambda:True,lambda k:sent.append(k) or True,lambda:now[0],lambda:True,interval=.04)
        peer=Frame(9,1,0,b'reply',True).encode()
        for _ in range(500):
            p.observe(peer); p.observe(peer); p.tick(); now[0]+=.04
        self.assertEqual(sent.count('F21'),3)
        self.assertEqual(sent.count('F22'),3)
        self.assertEqual(delivered,[b'reply'])
        self.assertIn('ack timeout',p.error)

    def test_retry_grace_starts_after_local_batch_finishes(self):
        now=[0.0]; sent=[]; link=Link(9); link.send(b'x')
        def slow(batch):
            sent.append(batch); now[0]+=0.8; return True
        pump=Pump(link,lambda:True,None,lambda:now[0],lambda:True,send_keys=slow)
        idle=Frame(9,ready=True).encode()
        pump.observe(idle); pump.observe(idle); self.assertTrue(pump.tick())
        now[0]=1.0; pump.observe(idle); self.assertFalse(pump.tick())
        # A real ACK still unlocks the next payload immediately.
        ack=Frame(9,ack=1,ready=True).encode(); pump.observe(ack); pump.observe(ack)
        link.send(b'next'); self.assertTrue(pump.tick())
        self.assertEqual(len(sent),2)

    def test_fast_batch_and_next_chunk_no_delay(self):
        now=[0.0]; batches=[]; a=Link(9); a.send(b'x'*96)
        p=Pump(a,lambda:True,None,lambda:now[0],lambda:True,
               send_keys=lambda batch:batches.append(batch) or True)
        idle=Frame(9,ready=True).encode(); p.observe(idle); p.observe(idle)
        self.assertTrue(p.tick())
        self.assertEqual(len(batches),1); self.assertEqual(len(batches[0]),335)
        self.assertIsNotNone(a.pending)  # fast emission is NOT success
        ack=Frame(9,ack=1,ready=True).encode(); p.observe(ack); p.observe(ack)
        a.send(b'next')
        self.assertTrue(p.tick())  # same clock instant, no artificial delay
        self.assertEqual(len(batches),2)
        self.assertEqual(batches[1],keys(a.packet()))

if __name__=='__main__': unittest.main(verbosity=2)
