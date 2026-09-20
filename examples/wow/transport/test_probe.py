import tempfile
from pathlib import Path
import unittest
from messages import Assembler
from protocol import Link
from probe import run_probe

class ProbeTests(unittest.TestCase):
    def desktop(self):
        class Fake:
            last_png=b'SIMULATED PNG fixture, not live evidence'
            received=[]
            def __init__(self):
                self.received=[]
                self.link=Link(42,Assembler(lambda kind,body:self.received.append(body),('R',)).receive)
                self.block=False
            def foreground(self): return True
            def reserved(self): return True
            def capture(self): return self.link.packet(ready=True)
            def send_keys(self,keys):
                if self.block: return False
                assert keys[0]=='F21' and keys[-1]=='F22'
                digits=''.join(str(int(k[1:])-13) for k in keys[1:-1])
                data=bytes(int(digits[i:i+3],8) for i in range(0,len(digits),3))
                self.link.receive(data)
                return True
        return Fake()
    def test_ack_receipt_and_capture_only(self):
        for enabled in (False,True):
            with tempfile.TemporaryDirectory() as root:
                fake=self.desktop(); now=[0.]
                def pause(dt): now[0]+=dt
                receipt=run_probe(fake,42,Path(root)/'proof',allow_input=enabled,
                                  size=256,clock=lambda:now[0],pause=pause,simulated=True)
                self.assertTrue(receipt['success'])
                self.assertEqual(receipt['acknowledged_bytes'],256 if enabled else 0)
                self.assertEqual(len(fake.received),int(enabled))
                self.assertTrue((Path(root)/'proof'/'receipt.json').exists())
                self.assertEqual(receipt['evidence'],'simulated')
    def test_uncertain_input_never_success(self):
        with tempfile.TemporaryDirectory() as root:
            fake=self.desktop();fake.block=True;now=[0.]
            result=run_probe(fake,42,Path(root)/'proof',allow_input=True,clock=lambda:now[0],
                             pause=lambda dt:now.__setitem__(0,now[0]+dt),simulated=True)
            self.assertFalse(result['success']);self.assertEqual(result['acknowledged_bytes'],0)
            self.assertIn('uncertain',result['error'])

class CalibrationTests(unittest.TestCase):
    def test_offline_scale_and_wrong_session(self):
        from PIL import Image
        from protocol import Frame,raster
        from calibrate import calibrate
        cells=raster(Frame(42,ready=True).encode())
        image=Image.new('RGB',(128,128))
        for y in range(128):
            for x in range(128):
                c=cells[y//4][x//4];image.putpixel((x,y),(c,c,c))
        result=calibrate(image,0,0,42,3.8,4.1)
        self.assertTrue(3.8<=result['cell_size']<=4.1)
        with self.assertRaises(ValueError): calibrate(image,0,0,99,3.8,4.1)
