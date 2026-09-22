"""Gaming-user local public-screen/input adapter. Importing does not access desktop.

Uses installed grim, hyprctl and wtype; never reads another user's home, focuses a
window, changes bindings, injects game memory, or sends gameplay commands. Parent
must supply observed exact WoW window address/class and calibrated carrier geometry.
"""
import io
import json
import math
import re
import subprocess
from pathlib import Path
try:
    from .x11 import keyboard_mapping
except ImportError:
    from x11 import keyboard_mapping
try:
    from .calibrate import decode_grid
except ImportError:
    from calibrate import decode_grid

RESERVED = frozenset('F'+str(n) for n in range(13,25))


class Desktop:
    def __init__(self, window_address, window_class, left, top, cell_size, run=subprocess.run, output_scale=1, cell_size_y=None, min_margin=2, input_backend="wayland", window_title="World of Warcraft", key_encoding="octal", key_hold_ms=0):
        if not window_address or not window_class or cell_size < 2 or cell_size > 32:
            raise ValueError('explicit window identity and calibrated cell size required')
        if not math.isfinite(output_scale) or not .5<=output_scale<=4:
            raise ValueError('explicit output scale .5..4 required')
        self.cell_size_y=cell_size if cell_size_y is None else cell_size_y
        if not 2<=self.cell_size_y<=32 or not 0<=min_margin<=127:
            raise ValueError('Y cell size/luminance margin')
        if input_backend not in ("wayland", "x11", "native-chord") or not isinstance(window_title, str) or not window_title:
            raise ValueError("explicit input backend and window title required")
        self.input_backend, self.window_title = input_backend, window_title
        if key_encoding not in ('octal', 'binary'):
            raise ValueError('key encoding')
        if type(key_hold_ms) is not int or not 0 <= key_hold_ms <= 50:
            raise ValueError('key hold 0..50 ms')
        if input_backend == 'native-chord' and key_encoding != 'octal':
            raise ValueError('native chords require octal framing')
        self._native_verified = False
        self.key_hold_ms = key_hold_ms
        self.key_encoding = key_encoding
        self.carrier_keys = frozenset(('F19','F23','F24')) if key_encoding == 'binary' else RESERVED
        self.min_margin=min_margin
        self.output_scale=output_scale
        if left < 0 or top < 0:
            raise ValueError('capture origin')
        self.window_address, self.window_class = window_address, window_class
        self.left, self.top, self.cell_size = left, top, cell_size
        self.run = run

    def _json(self, command):
        p = self.run(['hyprctl','-j',command],stdout=subprocess.PIPE,stderr=subprocess.PIPE,
                     timeout=1,check=True)
        return json.loads(p.stdout)

    def foreground(self):
        try:
            window = self._json('activewindow')
            if not isinstance(window, dict) or window.get('address') != self.window_address or window.get('class') != self.window_class:
                return False
            if self.input_backend == 'x11':
                pid = window.get('pid')
                if window.get('xwayland') is not True or type(pid) is not int or pid <= 0:
                    return False
                focus = self._text(['xdotool', 'getwindowfocus']).strip()
                if not focus.isdecimal() or int(focus) <= 0:
                    return False
                title = self._text(['xdotool', 'getwindowname', focus]).rstrip('\n')
                xpid = self._text(['xdotool', 'getwindowpid', focus]).strip()
                return title == self.window_title and xpid.isdecimal() and int(xpid) == pid
            return True
        except (OSError,ValueError,subprocess.SubprocessError):
            return False

    def _text(self, args):
        p = self.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                     timeout=1, check=True)
        return p.stdout.decode('utf-8')

    def _x11_keycodes(self):
        # Read only. Require every carrier symbol in the unmodified first slot.
        # Numeric xdotool keycodes set needs_binding=0 in libxdo, so a keymap
        # race cannot cause xdotool to allocate a temporary symbol mapping.
        # https://github.com/jordansissel/xdotool/blob/master/xdo.c
        codes = {}
        for code, symbols in keyboard_mapping().items():
            if not symbols or not 8 <= code <= 255:
                continue
            number = symbols[0] - 0xffbe + 1
            key = 'F' + str(number)
            if key in RESERVED:
                codes.setdefault(key, code)
        if not self.carrier_keys <= codes.keys():
            raise ValueError('carrier keys must already have unmodified X11 mappings')
        return codes

    def reserved(self, keycodes=()):
        try:
            bindings = self._json('binds')
            if not isinstance(bindings,list):
                return False
            if self.input_backend == 'native-chord':
                for binding in bindings:
                    name = str(binding.get('key','')).upper()
                    code = int(binding.get('keycode',0) or 0)
                    if name in ('ANY','CATCHALL'):
                        return False
                    if int(binding.get('modmask',0) or 0) == 5:
                        if name in {'F'+str(i) for i in (1,2,3,5,6,7,8,9,10,11)} or 59 <= code <= 76 or code in (87,95):
                            return False
                        if name.startswith('CODE:') and (59 <= int(name[5:]) <= 76 or int(name[5:]) in (87,95)):
                            return False
                return True
            for binding in bindings:
                name=str(binding.get('key','')).upper()
                code=int(binding.get('keycode',0) or 0)
                # Reject all modifier variants and both common evdev/XKB offsets.
                if name in RESERVED or name in ('ANY','CATCHALL') or 183 <= code <= 202 or code in keycodes:
                    return False
                if name.startswith('CODE:') and (183 <= int(name[5:]) <= 202 or int(name[5:]) in keycodes):
                    return False
            return True
        except (OSError,ValueError,TypeError,subprocess.SubprocessError):
            return False

    def send_keys(self, keys):
        if not keys or len(keys)>(890 if self.key_encoding == 'binary' else 335) or any(key not in self.carrier_keys for key in keys):
            raise ValueError('bounded reserved-key batch required')
        codes = None
        if self.input_backend == 'x11':
            try:
                codes = self._x11_keycodes()
            except (OSError, ValueError, subprocess.SubprocessError):
                return False
        mapped_codes = set(codes.values()) if codes else set()
        if not self.reserved(mapped_codes | {code - 8 for code in mapped_codes}) or not self.foreground():
            return False
        # One process, zero artificial inter-key sleeps. -k emits press AND release.
        args=['wtype']
        for key in keys:
            args.extend(('-k',key))
        if codes is not None:
            # No --window/stack (XTEST), focus mutation, or modifier mutation.
            # Pad short codes so XStringToKeysym cannot interpret a digit key.
            args = ['xdotool', 'key', '--delay', '0'] + [str(codes[key]).zfill(3) for key in keys]
        timeout = 2
        if self.input_backend == 'native-chord':
            if any(not 13 <= int(key[1:]) <= 22 for key in keys):
                raise ValueError('chord alphabet')
            binary = str(Path(__file__).parent/'native'/'carrier-keys')
            if not self._native_verified:
                spec = json.loads(self._text([binary, '--describe']))
                if spec != {'schema':1,'modifiers':['CTRL','SHIFT'],'keys':['F'+str(n) for n in (1,2,3,5,6,7,8,9,10,11)]}:
                    raise ValueError('native helper differs from tested no-Alt/no-F4 protocol')
                self._native_verified = True
            args = [binary, str(self.key_hold_ms)] + ['C'+str(int(key[1:])-12) for key in keys]
            timeout += len(keys)*self.key_hold_ms*2/1000
        self.run(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=timeout,check=True)
        # Input APIs cannot retract events after a focus race. An observed race is
        # ambiguous and stops Pump; successful command exit is NOT carrier ACK.
        return self.foreground()

    def capture(self):
        if not self.foreground():
            raise ValueError('WoW not foreground')
        from PIL import Image
        # Calibration uses physical screenshot pixels; grim -g uses LOGICAL coords.
        scale=self.output_scale
        left=math.floor(self.left/scale);top=math.floor(self.top/scale)
        width=math.ceil((self.left+32*self.cell_size)/scale)-left
        height=math.ceil((self.top+32*self.cell_size_y)/scale)-top
        geometry=f'{left},{top} {width}x{height}'
        p=self.run(['grim','-s',str(scale),'-g',geometry,'-t','png','-'],stdout=subprocess.PIPE,
                   stderr=subprocess.PIPE,timeout=2,check=True)
        with Image.open(io.BytesIO(p.stdout)) as image:
            if image.size != (round(width*scale),round(height*scale)):
                raise ValueError('capture scale differs from explicit output scale')
            rgb=image.convert('RGB')
            packet,confidence=decode_grid(lambda x,y:rgb.getpixel((x,y)),self.left-left*scale,
                                 self.top-top*scale,self.cell_size,self.cell_size_y,
                                 min_margin=self.min_margin,details=True)
            self.last_png=p.stdout
            self.last_geometry={'logical_geometry':geometry,'output_scale':scale,
                                'physical_image_size':list(image.size),
                                'physical_carrier_origin':[self.left-left*scale,self.top-top*scale],
                                'physical_cell_size':self.cell_size,'physical_cell_size_x':self.cell_size,
                                'physical_cell_size_y':self.cell_size_y,'decode_confidence':confidence}
            return packet
