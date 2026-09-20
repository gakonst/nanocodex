#!/usr/bin/env python3
"""Explicit local calibration/capture and ACK benchmark; never focuses WoW."""
import argparse
import hashlib
import json
from pathlib import Path
import secrets
import time

try:
    from .driver import Pump
    from .messages import fragments
    from .protocol import Frame, Link
    from .wayland import Desktop
except ImportError:
    from driver import Pump
    from messages import fragments
    from protocol import Frame, Link
    from wayland import Desktop


def sha(data):
    return hashlib.sha256(data).hexdigest()


def run_probe(desktop, session, output, *, allow_input=False, size=256, count=1,
              timeout=30, clock=time.monotonic, pause=time.sleep, simulated=False):
    output=Path(output)
    output.mkdir(parents=True,exist_ok=False)
    receipt={'schema':1,'evidence':'simulated' if simulated else 'local-public-surface',
             'session':session,'mode':'benchmark' if allow_input else 'capture',
             'success':False,'acknowledged_bytes':0,'frames':[],
             'model_connectivity_proven':False,'input_batches':0}
    start=clock()
    def capture(name=None):
        packet=desktop.capture()
        f=Frame.decode(packet)
        if f.session!=session:
            raise ValueError('wrong session: explicit new shared session required')
        if f.seq:
            raise ValueError('outbound request present: probe refuses to consume application requests')
        if name:
            png=desktop.last_png
            (output/name).write_bytes(png)
            receipt['frames'].append({'file':name,'sha256':sha(png),'packet_sha256':sha(packet),
                                      'session':f.session,'seq':f.seq,'ack':f.ack,'ready':f.ready,
                                      'elapsed':clock()-start,'capture_geometry':getattr(desktop,'last_geometry',None)})
        return packet
    try:
        foreground_deadline=clock()+timeout
        while not desktop.foreground():
            if clock()>foreground_deadline:
                raise TimeoutError('WoW did not become foreground; no input sent')
            pause(.05)
        first=capture('initial-1.png')
        pause(.02)
        second=capture('initial-2.png')
        if first!=second:
            raise ValueError('two independent captures disagree; recheck calibration/state')
        if not allow_input:
            receipt['success']=True
            receipt['claim']='Two independent lossless captures decoded matching NC1 CRC/session. No input.'
            return receipt
        f=Frame.decode(second)
        if not f.ready or f.ack!=0 or not desktop.reserved():
            raise ValueError('fresh session ACK=0, addon ready and unbound OS keys required')
        link=Link(session)
        def emit(keys):
            receipt['input_batches']+=1
            return desktop.send_keys(keys)
        pump=Pump(link,desktop.foreground,None,clock,desktop.reserved,send_keys=emit, encoding=getattr(desktop,'key_encoding','octal'), burst_size=890 if getattr(desktop,'key_encoding','octal')=='binary' else 335)
        pump.observe(first); pump.observe(second)
        nonce=secrets.token_hex(8)
        receipt['challenge']=nonce
        payloads=[]
        measurement=clock()
        for mid in range(1,count+1):
            prefix=f'NC1-PROBE:{nonce}:{mid}:'.encode()
            body=(prefix+b'x'*size)[:size]
            payloads.append({'message_id':mid,'bytes':len(body),'sha256':sha(body)})
            for chunk in fragments('R',mid,body):
                link.send(chunk)
                deadline=clock()+timeout
                while link.pending is not None:
                    if clock()>deadline:
                        raise TimeoutError('ACK timeout; delivery unknown, do not replay automatically')
                    pump.observe(capture())
                    pump.tick()
                    if pump.error:
                        raise RuntimeError(pump.error)
                    # A render/capture loop pause, not a per-key throttle.
                    pause(.001)
                receipt['acknowledged_bytes']+=len(chunk)-8
        receipt['payloads']=payloads
        receipt['benchmark_seconds']=clock()-measurement
        receipt['payload_bytes_per_second']=receipt['acknowledged_bytes']/max(receipt['benchmark_seconds'],1e-9)
        final=capture('ack-1.png'); pause(.02); final2=capture('ack-2.png')
        if final!=final2 or Frame.decode(final).ack!=link.tx:
            raise ValueError('final stable ACK evidence missing')
        receipt['final_ack']=link.tx
        receipt['success']=True
        receipt['claim']='Peer carrier ACK of all probe chunks observed in two lossless captures; not backend/model proof.'
        return receipt
    except Exception as exc:
        receipt['error']=str(exc)
        return receipt
    finally:
        receipt['elapsed_seconds']=clock()-start
        (output/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode',choices=('capture','benchmark'))
    parser.add_argument('--window-address',required=True)
    parser.add_argument('--window-class',required=True)
    parser.add_argument('--left',type=int,required=True)
    parser.add_argument('--top',type=int,required=True)
    parser.add_argument('--cell-size',type=float,required=True)
    parser.add_argument('--cell-size-y',type=float)
    parser.add_argument('--input-backend',choices=('wayland','x11','native-chord'),default='wayland')
    parser.add_argument('--output-scale',type=float,default=1,help='grim output scale; left/top/cell-size stay physical pixels')
    parser.add_argument('--session',type=lambda s:int(s,0),required=True)
    parser.add_argument('--output',required=True,help='NEW artifact directory; existing path refused')
    parser.add_argument('--allow-input',action='store_true',help='required only for benchmark; emits reserved keys')
    parser.add_argument('--bytes',type=int,default=256)
    parser.add_argument('--count',type=int,default=1)
    parser.add_argument('--timeout',type=float,default=30)
    parser.add_argument('--key-hold-ms',type=int,default=0)
    parser.add_argument('--key-encoding',choices=('octal','binary'),default='octal')
    args=parser.parse_args()
    if args.mode=='benchmark' and not args.allow_input:
        parser.error('benchmark requires --allow-input; parent owns live input')
    if args.mode=='capture' and args.allow_input:
        parser.error('capture never accepts --allow-input')
    if not 64<=args.bytes<=16384 or not 1<=args.count<=32 or not 1<=args.timeout<=60 or not 1<=args.session<=0xffffffff:
        parser.error('bounds: bytes64..16384 count1..32 timeout1..60 session1..0xffffffff')
    desktop=Desktop(args.window_address,args.window_class,args.left,args.top,args.cell_size,output_scale=args.output_scale,cell_size_y=args.cell_size_y,input_backend=args.input_backend,key_encoding=args.key_encoding,key_hold_ms=args.key_hold_ms)
    result=run_probe(desktop,args.session,args.output,allow_input=args.allow_input,
                     size=args.bytes,count=args.count,timeout=args.timeout)
    print(json.dumps(result,indent=2))
    return 0 if result['success'] else 1

if __name__=='__main__':
    raise SystemExit(main())
