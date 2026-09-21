#!/usr/bin/env python3
"""Offline independent X/Y sampling and full CRC/padding validation."""
import argparse
import hashlib
import json
import math
from PIL import Image
try:
    from .protocol import Frame
except ImportError:
    from protocol import Frame


def decode_grid(sample,left,top,cell_size_x,cell_size_y=None,*,min_margin=2,details=False):
    """Physical sample-center coordinates. Fixed midpoint threshold, no guessed bits.

    Lossless compositor edges can be gray. Reject chromatic/near-midpoint pixels;
    expose the weakest threshold margin, and validate ALL padding plus CRC. A
    successful decode proves only this captured frame, never independent delivery.
    """
    cy=cell_size_x if cell_size_y is None else cell_size_y
    if not all(math.isfinite(v) for v in (left,top,cell_size_x,cy,min_margin)) or not 2<=cell_size_x<=32 or not 2<=cy<=32 or not 0<=min_margin<=127:
        raise ValueError('grid bounds')
    bits=[];weakest=128.;low_contrast=0
    for y in range(32):
        for x in range(32):
            rgb=sample(int(left+(x+.5)*cell_size_x),int(top+(y+.5)*cy))[:3]
            luminance=sum(rgb)/3
            margin=abs(luminance-128)
            if max(rgb)-min(rgb)>40 or margin<min_margin:
                raise ValueError('ambiguous pixel')
            weakest=min(weakest,margin)
            low_contrast+=margin<64
            bits.append(int(luminance>128))
    raw=bytes(sum(bits[i+j]<<(7-j) for j in range(8)) for i in range(0,1024,8))
    size=raw[12]+15
    if size>111 or any(raw[size:]):raise ValueError('length/padding')
    packet=raw[:size]
    Frame.decode(packet)
    detail={'minimum_luminance_margin':weakest,'low_contrast_cells':low_contrast,
            'sampled_bits':1024,'padding_bytes_checked':128-size,'threshold':128,
            'full_crc_valid':True,'full_padding_valid':True}
    return (packet,detail) if details else packet


def calibrate(image,left,top,session,low=2.,high=8.,step=.01,*,y_low=None,y_high=None,min_margin=2):
    if not 0<=left<image.width or not 0<=top<image.height or not 2<=low<=high<=32 or step<.01:
        raise ValueError('calibration bounds')
    yl=low if y_low is None else y_low;yh=high if y_high is None else y_high
    if not 2<=yl<=yh<=32:raise ValueError('Y calibration bounds')
    xs=[round(low+n*step,4) for n in range(int(round((high-low)/step))+1)]
    ys=[round(yl+n*step,4) for n in range(int(round((yh-yl)/step))+1)]
    if len(xs)*len(ys)>1000000:raise ValueError('search bound; narrow range')
    rgb=image.convert('RGB');matches=[];cache={}
    for cx in xs:
        if left+32*cx>image.width:continue
        for cy in ys:
            if top+32*cy>image.height:continue
            # Candidate row signatures can repeat; only decode each sampling grid once.
            key=(tuple(int(left+(x+.5)*cx) for x in range(32)),tuple(int(top+(y+.5)*cy) for y in range(32)))
            if key not in cache:
                prefix=[]
                for x in key[0][:24]:
                    pixel=rgb.getpixel((x,key[1][0]));lum=sum(pixel)/3
                    if max(pixel)-min(pixel)>40 or abs(lum-128)<min_margin:break
                    prefix.append(int(lum>128))
                if len(prefix)!=24 or bytes(sum(prefix[i+j]<<(7-j) for j in range(8)) for i in (0,8,16))!=b'NC1':
                    cache[key]=None;continue
                try:
                    packet,detail=decode_grid(lambda x,y:rgb.getpixel((x,y)),left,top,cx,cy,min_margin=min_margin,details=True)
                    cache[key]=(packet,detail) if Frame.decode(packet).session==session else None
                except (ValueError,IndexError):cache[key]=None
            if cache[key]:matches.append((cx,cy,*cache[key]))
    if not matches:raise ValueError('no full CRC+padding valid carrier')
    if len({sha(packet) for _,_,packet,_ in matches})!=1:raise ValueError('ambiguous packets')
    # Prefer fewer low-contrast samples; choose center of best sampled candidate set.
    best_margin=max(d['minimum_luminance_margin'] for _,_,_,d in matches)
    best_low=min(d['low_contrast_cells'] for _,_,_,d in matches if d['minimum_luminance_margin']==best_margin)
    best=[m for m in matches if m[3]['minimum_luminance_margin']==best_margin and m[3]['low_contrast_cells']==best_low]
    cx,cy,packet,detail=best[len(best)//2]
    return {'left':left,'top':top,'cell_size':cx,'cell_size_x':cx,'cell_size_y':cy,
            'matching_x_range':[min(m[0] for m in matches),max(m[0] for m in matches)],
            'matching_y_range':[min(m[1] for m in matches),max(m[1] for m in matches)],
            'session':session,'packet_sha256':sha(packet),'packet_hex':packet.hex(),
            'confidence':detail,'note':'One offline screenshot only. Require two new independent matching captures before input; no reload required.'}


def sha(data):return hashlib.sha256(data).hexdigest()

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('png');p.add_argument('--left',type=float,required=True);p.add_argument('--top',type=float,required=True)
    p.add_argument('--session',type=lambda s:int(s,0),required=True)
    p.add_argument('--cell-min',type=float,default=4);p.add_argument('--cell-max',type=float,default=5)
    p.add_argument('--y-min',type=float);p.add_argument('--y-max',type=float)
    p.add_argument('--min-margin',type=float,default=2)
    a=p.parse_args()
    with Image.open(a.png) as image:
        result=calibrate(image,a.left,a.top,a.session,a.cell_min,a.cell_max,y_low=a.y_min,y_high=a.y_max,min_margin=a.min_margin)
    result['source_sha256']=sha(open(a.png,'rb').read())
    print(json.dumps(result,indent=2))

if __name__=='__main__':main()
