import subprocess
import unittest
from messages import chunk, fragments, Assembler, MAX_MESSAGE
from protocol import Link


class MessageTests(unittest.TestCase):
    def test_lua_contract_and_golden(self):
        p = subprocess.run(['lua','transport/test_application.lua'],capture_output=True,text=True,check=True)
        self.assertEqual(p.stdout.splitlines()[0],chunk('R',1,89,88,b'\xa9').hex())

    def test_multichunk_binary_boundaries(self):
        for payload in (b'',b'a',b'x'*88,b'R'*87+b'\xc3\xa9',bytes(range(256)),b'x'*MAX_MESSAGE):
            got=[]
            assembly=Assembler(lambda kind,body:got.append((kind,body)))
            sender=Link(42)
            receiver=Link(42,assembly.receive)
            for part in fragments('Q',1,payload):
                sender.send(part)
                receiver.receive(sender.packet())
                receiver.receive(sender.packet())  # duplicate never appends twice
                sender.receive(receiver.packet())
                self.assertIsNone(sender.pending)
            self.assertEqual(got,[('request',payload)])
            self.assertIsNone(assembly.current)

    def test_inbound_types(self):
        for code,kind in [('R','reply'),('P','projects'),('A','ack'),('E','error'),('S','stream')]:
            got=[]
            a=Assembler(lambda k,b:got.append((k,b)),accepted_kinds=('R','P','A','E','S'))
            a.receive(next(fragments(code,1,'test')))
            self.assertEqual(got,[(kind,b'test')])

    def test_rejection_preserves_partial_and_no_ack(self):
        allow=[False]; a=Assembler(lambda kind,payload:allow[0])
        parts=list(fragments('Q',1,b'x'*89))
        link=Link(42,a.receive)
        sender=Link(42); sender.send(parts[0]); link.receive(sender.packet()); sender.receive(link.packet())
        sender.send(parts[1])
        with self.assertRaises(ValueError): link.receive(sender.packet())
        self.assertEqual(link.rx,1); self.assertEqual(len(a.current[3]),88)
        allow[0]=True; link.receive(sender.packet()); self.assertEqual(link.rx,2)
        with self.assertRaises(ValueError): a.receive(parts[0])  # replayed message ID

    def test_malformed_bounds_and_interleaving(self):
        a=Assembler(lambda k,p:True)
        for part in (b'',b'bad',b'MQ\0\1\xff\xff\0\0x',chunk('R',1,1,0,b'x'),chunk('Q',2,1,0,b'x'),chunk('Q',1,2,1,b'x')):
            with self.assertRaises((ValueError,UnicodeError)): a.receive(part)
        a.receive(chunk('Q',1,100,0,b'a'*88))
        for part in (chunk('Q',2,100,88,b'b'*12),chunk('Q',1,99,88,b'b'*11),chunk('Q',1,100,89,b'b'*11)):
            with self.assertRaises(ValueError): a.receive(part)
        with self.assertRaises(ValueError): list(fragments('Q',1,b'x'*(MAX_MESSAGE+1)))
        with self.assertRaises(ValueError): chunk('Q',1,1,0,b'')

if __name__=='__main__': unittest.main(verbosity=2)
