"""NC1 public-surface codec. No account, screen, keyboard or gameplay access."""
from dataclasses import dataclass
import binascii
import struct

MAX_PAYLOAD = 96
MAX_PACKET = 111
HEADER = struct.Struct('>3sBIHHB')

@dataclass(frozen=True)
class Frame:
    session: int
    seq: int = 0
    ack: int = 0
    payload: bytes = b''
    ready: bool = False

    def encode(self):
        if not 1 <= self.session <= 0xffffffff or not 0 <= self.seq <= 65535 or not 0 <= self.ack <= 65535:
            raise ValueError('session/sequence')
        if len(self.payload) > MAX_PAYLOAD or (not self.seq and self.payload):
            raise ValueError('payload')
        body = HEADER.pack(b'NC1', int(self.ready), self.session, self.seq, self.ack, len(self.payload)) + self.payload
        return body + struct.pack('>H', binascii.crc_hqx(body, 0xffff))

    @classmethod
    def decode(cls, packet):
        if not 15 <= len(packet) <= MAX_PACKET:
            raise ValueError('length')
        magic, flags, session, seq, ack, size = HEADER.unpack(packet[:13])
        if magic != b'NC1' or flags > 1 or size > MAX_PAYLOAD or len(packet) != size + 15:
            raise ValueError('header/length')
        if binascii.crc_hqx(packet[:-2], 0xffff) != int.from_bytes(packet[-2:], 'big'):
            raise ValueError('crc')
        value = cls(session, seq, ack, packet[13:-2], bool(flags))
        value.encode()  # Validate semantic bounds too.
        return value

class Link:
    def __init__(self, session, deliver=lambda payload, seq: True):
        Frame(session).encode()
        self.session, self.deliver = session, deliver
        self.tx = self.rx = 0
        self.pending = self.last = None

    def send(self, payload):
        if not isinstance(payload, bytes) or len(payload) > MAX_PAYLOAD:
            raise ValueError('payload limit')
        if self.pending is not None:
            raise ValueError('busy')
        if self.tx == 65535:
            raise ValueError('new session required')
        self.tx += 1
        self.pending = payload
        return self.tx

    def packet(self, ready=False):
        return Frame(self.session, self.tx if self.pending is not None else 0,
                     self.rx, self.pending or b'', ready).encode()

    def receive(self, packet):
        f = Frame.decode(packet)
        if f.session != self.session or f.ack > self.tx:
            raise ValueError('session/future ack')
        if f.seq and f.seq not in (self.rx, self.rx + 1):
            raise ValueError('sequence gap')
        if f.seq and f.seq == self.rx and f.payload != self.last:
            raise ValueError('conflicting duplicate')
        if f.seq and f.seq == self.rx + 1:
            if self.deliver(f.payload, f.seq) is False:
                raise ValueError('delivery rejected')
            self.rx, self.last = f.seq, f.payload
        if self.pending is not None and f.ack == self.tx:
            self.pending = None
        return f


def keys(packet, encoding='octal'):
    Frame.decode(packet)
    if encoding == 'binary':
        return ['F24'] + ['F23' if (b >> shift) & 1 else 'F19'
                          for b in packet for shift in range(7, -1, -1)] + ['F24']
    if encoding != 'octal':
        raise ValueError('key encoding')
    return ['F21'] + ['F' + str(13 + int(d)) for b in packet for d in f'{b:03o}'] + ['F22']


def raster(packet):
    """32x32 binary cells, MSB first; 4 UI units per cell in Lua."""
    Frame.decode(packet)
    padded = packet.ljust(128, b'\0')
    return [[255 * ((padded[(y * 32 + x) // 8] >> (7 - x % 8)) & 1)
             for x in range(32)] for y in range(32)]


def decode_pixels(sample, left, top, cell_size):
    """sample(x,y)->RGB. Caller supplies calibrated physical cell coordinates.

    Samples interior of each cell; rejects ambiguous luminance or non-monochrome
    pixels. CRC rejects corrupted/torn frames; call twice for stable acquisition.
    """
    if cell_size < 2:
        raise ValueError('insufficient resolution')
    bits = []
    for y in range(32):
        for x in range(32):
            rgb = sample(int(left + (x + .5) * cell_size), int(top + (y + .5) * cell_size))[:3]
            if max(rgb) - min(rgb) > 40 or not (max(rgb) < 64 or min(rgb) > 191):
                raise ValueError('ambiguous pixel')
            bits.append(int(min(rgb) > 191))
    packet = bytes(sum(bits[i+j] << (7-j) for j in range(8)) for i in range(0, 1024, 8))
    size = packet[12] + 15
    if any(packet[size:]):
        raise ValueError('padding')
    packet = packet[:size]
    Frame.decode(packet)
    return packet
