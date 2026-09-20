import json
import unittest
from types import SimpleNamespace
from wayland import Desktop


class AdapterTests(unittest.TestCase):
    def make(self,active=None,binds=None):
        calls=[]
        active=active or {'address':'0x123','class':'WowB.exe'}
        def run(args,**kwargs):
            calls.append(args)
            if args[:2]==['hyprctl','-j']:
                return SimpleNamespace(stdout=json.dumps(active if args[2]=='activewindow' else (binds or [])).encode())
            return SimpleNamespace(stdout=b'')
        return Desktop('0x123','WowB.exe',0,0,4,run=run),calls

    def test_single_local_process_press_release_batch(self):
        desktop,calls=self.make()
        self.assertTrue(desktop.send_keys(['F21']+['F13']*333+['F22']))
        input_calls=[c for c in calls if c[0]=='wtype']
        self.assertEqual(len(input_calls),1)
        self.assertEqual(len(input_calls[0]),671)
        self.assertEqual(input_calls[0][-2:],['-k','F22'])
        self.assertFalse(any('dispatch' in c for c in calls))

    def test_no_input_when_wrong_window_or_binding(self):
        for active,binds in [({'address':'other','class':'WowB.exe'},None),
                             (None,[{'key':'F13'}]),(None,[{'keycode':191}])]:
            desktop,calls=self.make(active,binds)
            self.assertFalse(desktop.send_keys(['F21']))
            self.assertFalse(any(c[0]=='wtype' for c in calls))

    def test_invalid_keys_never_reach_adapter(self):
        desktop,calls=self.make()
        for keys in ([],['W'],['F21']*336):
            with self.assertRaises(ValueError): desktop.send_keys(keys)
        self.assertEqual(calls,[])

    def test_lossless_capture_decodes_real_image_buffer(self):
        import io
        from PIL import Image
        from protocol import Frame, raster
        packet=Frame(7,1,0,b'carrier',True).encode()
        cells=raster(packet)
        image=Image.new('RGB',(128,128))
        for y in range(128):
            for x in range(128):
                c=cells[y//4][x//4]; image.putpixel((x,y),(c,c,c))
        out=io.BytesIO(); image.save(out,format='PNG')
        def run(args,**kwargs):
            if args[0]=='hyprctl':
                return SimpleNamespace(stdout=b'{"address":"0x123","class":"WowB.exe"}')
            self.assertEqual(args,['grim','-s','1','-g','0,0 128x128','-t','png','-'])
            return SimpleNamespace(stdout=out.getvalue())
        desktop=Desktop('0x123','WowB.exe',0,0,4,run=run)
        self.assertEqual(desktop.capture(),packet)

    def test_logical_crop_scale_two_fractional_physical_cells(self):
        import io
        from PIL import Image
        from protocol import Frame,raster
        packet=Frame(53281,ready=True).encode()
        cells=raster(packet)
        # Physical origin (19,71), cell4.8 => logical crop (9,35) 78x78.
        image=Image.new('RGB',(156,156),(80,70,60))
        for y in range(1,155):
            for x in range(1,155):
                cx=min(31,int((x-1)/4.8));cy=min(31,int((y-1)/4.8))
                c=cells[cy][cx];image.putpixel((x,y),(c,c,c))
        out=io.BytesIO();image.save(out,format='PNG')
        def run(args,**kwargs):
            if args[0]=='hyprctl':return SimpleNamespace(stdout=b'{"address":"0x123","class":"WowB.exe"}')
            self.assertEqual(args,['grim','-s','2','-g','9,35 78x78','-t','png','-'])
            return SimpleNamespace(stdout=out.getvalue())
        desktop=Desktop('0x123','WowB.exe',19,71,4.8,run=run,output_scale=2)
        self.assertEqual(desktop.capture(),packet)
        self.assertEqual(desktop.last_geometry['physical_carrier_origin'],[1,1])

if __name__=='__main__': unittest.main(verbosity=2)
