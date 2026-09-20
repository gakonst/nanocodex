#!/opt/hand-python/bin/python3
"""Test-only C1-framed source. Excludes compositor and native capture latency."""
import os, struct, sys, threading, subprocess, time, json, pathlib
state = 0
buttons, keys = set(), set()
relative_count = 0

def receive():
    global state, relative_count
    while True:
        event = sys.stdin.buffer.read(16)
        if len(event) != 16:
            return
        if event[0] != 2:
            continue
        kind, down = event[1], event[2] == 1
        code = int.from_bytes(event[4:8], 'little')
        if kind == 4:
            (keys.add if down else keys.discard)(code)
            if down and code == 57:
                state ^= 1
        elif kind == 2:
            (buttons.add if down else buttons.discard)(code)
        elif kind == 5:
            buttons.clear(); keys.clear()
        elif kind == 8:
            relative_count += 1
        if kind in (2, 4, 5):
            temp = pathlib.Path('/perf/held-input.tmp')
            temp.write_text(json.dumps({'buttons': sorted(buttons), 'keys': sorted(keys), 'relative_count': relative_count}))
            temp.replace('/perf/held-input.json')

threading.Thread(target=receive, daemon=True).start()
meta_read, meta_write = os.pipe()
command = ['ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'rawvideo',
    '-pixel_format', 'yuv420p', '-video_size', '1280x720', '-framerate', '60',
    '-i', 'pipe:0', '-an', '-c:v', 'libx264', '-preset', 'ultrafast',
    '-tune', 'zerolatency', '-threads', '1', '-b:v', '6000k', '-maxrate', '6000k',
    '-bufsize', '100k', '-x264-params', 'aud=1:repeat-headers=1',
    '-profile:v', 'baseline', '-g', '30', '-bf', '0', '-pix_fmt', 'yuv420p',
    '-map', '0:v:0', '-f', 'tee',
    f'[f=framecrc:flush_packets=1]pipe:{meta_write}|[f=h264:flush_packets=1]pipe:1']
encoder = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                           pass_fds=(meta_write,))
os.close(meta_write)

def forward():
    os.write(1, b'NCH264C1')
    with os.fdopen(meta_read, 'rb') as metadata:
        for line in metadata:
            if line.startswith(b'#'):
                continue
            size = int(line.split(b',')[4].strip())
            if not 0 < size <= 8 * 1024 * 1024:
                raise ValueError('invalid synthetic encoder packet')
            frame = encoder.stdout.read(size)
            if len(frame) != size:
                raise EOFError('truncated synthetic frame')
            for offset in range(0, size, 4092):
                chunk = frame[offset:offset + 4092]
                final = offset + len(chunk) == size
                record = struct.pack('>I', len(chunk) | (0x80000000 if final else 0)) + chunk
                if os.write(1, record) != len(record):
                    raise OSError('partial atomic C1 record')

threading.Thread(target=forward, daemon=True).start()
frames = [bytes([y]) * (1280 * 720) + bytes([128]) * (1280 * 720 // 2) for y in [16, 235]]
next_frame = time.monotonic()
try:
    while True:
        encoder.stdin.write(frames[state])
        encoder.stdin.flush()
        next_frame += 1 / 60
        time.sleep(max(0, next_frame - time.monotonic()))
except (BrokenPipeError, KeyboardInterrupt):
    pass
finally:
    encoder.kill()
    encoder.wait()
