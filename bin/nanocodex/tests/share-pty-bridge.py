"""PTY bridge for the native sharing E2E; stdin/stdout carry raw terminal bytes."""
import os
import pty
import select
import signal
import sys
import termios

master, slave = pty.openpty()
import struct
size = struct.pack('HHHH', 32, 160, 0, 0)
import fcntl
fcntl.ioctl(slave, termios.TIOCSWINSZ, size)
pid = os.fork()
if pid == 0:
    os.close(master)
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
    os.dup2(slave, 0)
    os.dup2(slave, 1)
    os.dup2(slave, 2)
    if slave > 2:
        os.close(slave)
    os.execv(sys.argv[1], sys.argv[1:])
os.close(slave)
signal.signal(signal.SIGTERM, lambda _signal, _frame: sys.exit(0))
try:
    while True:
        readable, _, _ = select.select([sys.stdin.fileno(), master], [], [], 0.1)
        if master in readable:
            try:
                chunk = os.read(master, 8192)
            except OSError:
                break
            if not chunk:
                break
            os.write(sys.stdout.fileno(), chunk)
        if sys.stdin.fileno() in readable:
            chunk = os.read(sys.stdin.fileno(), 8192)
            if not chunk:
                break
            os.write(master, chunk)
finally:
    try:
        os.killpg(pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    os.waitpid(pid, 0)
