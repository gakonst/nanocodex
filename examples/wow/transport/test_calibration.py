import unittest
from PIL import Image
from transport.calibrate import decode_grid,calibrate
from transport.protocol import Frame,raster

class XYCalibrationTests(unittest.TestCase):
    def make(self,payload=b''):
        packet=Frame(53281,1 if payload else 0,0,payload,True).encode()
        grid=raster(packet);im=Image.new('RGB',(160,160))
        for y in range(144):
            for x in range(152):
                row=min(31,int(y/4.5));col=min(31,int(x/4.75));c=grid[row][col]
                im.putpixel((x,y),(c,c,c))
        return packet,im
    def test_independent_axes_full_payload(self):
        packet,im=self.make(bytes(range(96)))
        actual,d=decode_grid(lambda x,y:im.getpixel((x,y)),0,0,4.75,4.5,details=True)
        self.assertEqual(actual,packet);self.assertEqual(d['sampled_bits'],1024)
        result=calibrate(im,0,0,53281,4.74,4.76,y_low=4.49,y_high=4.51)
        self.assertEqual(result['packet_hex'],packet.hex())
    def test_padding_bit_invalidates_valid_header(self):
        packet,im=self.make()
        im.putpixel((149,141),(255,255,255))
        with self.assertRaisesRegex(ValueError,'padding'):
            decode_grid(lambda x,y:im.getpixel((x,y)),0,0,4.75,4.5)
    def test_gray_classification_and_margin(self):
        packet,im=self.make()
        im.putpixel((2,2),(117,126,130))
        p,detail=decode_grid(lambda x,y:im.getpixel((x,y)),0,0,4.75,4.5,details=True)
        self.assertEqual(p,packet);self.assertLess(detail['minimum_luminance_margin'],4)
        with self.assertRaisesRegex(ValueError,'ambiguous'):
            decode_grid(lambda x,y:im.getpixel((x,y)),0,0,4.75,4.5,min_margin=8)
