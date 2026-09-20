"""Local burst carrier pump. No focus changes or binding installation.

Desktop adapter verifies foreground WoW and unassigned compositor F13-F24.
send_keys must confirm a bounded batch of press+release events. send_key remains a
compatibility adapter. interval=0 uses immediate local delivery; a caller may pace
explicitly after measuring actual event loss. Binary fallback callers should use
encoding='binary', send_keys, interval=0 and burst_size=890 for a single batch.
ACK/CRC, not emission, prove delivery.
"""
try:
    from .protocol import Frame, Link, keys
except ImportError:
    from protocol import Frame, Link, keys

class Pump:
    def __init__(self, link, foreground, send_key, clock, reserved, interval=0,
                 send_keys=None, burst_size=335, retry_delay=1.0, encoding='octal'):
        if not isinstance(link, Link) or interval < 0 or not isinstance(burst_size, int) or not 1 <= burst_size <= 890 or retry_delay < .02:
            raise ValueError('link/rate/burst')
        if encoding not in ('octal', 'binary'):
            raise ValueError('key encoding')
        self.encoding = encoding
        self.link, self.foreground, self.send_key = link, foreground, send_key
        self.clock, self.reserved, self.interval = clock, reserved, interval
        self.send_keys, self.burst_size, self.retry_delay = send_keys, burst_size, retry_delay
        self.observed = None
        self.stable = None
        self.seen_at = float('-inf')
        self.stream = []
        self.due = 0
        self.attempts = 0
        self.sent_ack = 0
        self.error = None
        self.inflight = None
        self.last_wire = None

    def observe(self, packet):
        f = Frame.decode(packet)
        if f.session != self.link.session:
            raise ValueError('session')
        # Two separately captured equal frames required, not two samples of one image.
        if packet == self.observed:
            self.link.receive(packet)
            self.stable = f
            self.seen_at = self.clock()
        else:
            self.stable = None
        self.observed = packet

    def tick(self):
        now = self.clock()
        if self.error:
            return False
        if not (self.stable and self.stable.ready and now-self.seen_at <= .5
                and self.reserved() and self.foreground()):
            self.stream = []
            self.inflight = None
            return False
        packet = self.link.packet()
        # ACK of the prior packet enables next chunk immediately, no retry delay.
        if packet != self.last_wire and not self.stream:
            self.attempts = 0
            self.last_wire = packet
            self.due = now
        if now < self.due:
            return False
        if not self.stream:
            if self.link.pending is None and self.sent_ack == self.link.rx and not self.stable.seq:
                return False
            if self.attempts >= 3:
                self.error = 'ack timeout: delivery unknown; reconcile before new session'
                return False
            self.inflight = Frame.decode(packet)
            self.stream = keys(self.inflight.encode(), encoding=self.encoding)
            self.attempts += 1
        count = min(len(self.stream), self.burst_size) if self.send_keys and not self.interval else 1
        batch = self.stream[:count]
        try:
            confirmed = self.send_keys(batch) if self.send_keys else self.send_key(batch[0])
            if confirmed is not True:
                raise RuntimeError('input adapter did not confirm press/release')
        except Exception:
            self.error = 'input outcome uncertain; stopped without retry'
            self.stream = []
            return False
        del self.stream[:count]
        self.due = now + self.interval
        if not self.stream:
            self.sent_ack = self.inflight.ack
            self.due = self.clock() + self.retry_delay
        return True
