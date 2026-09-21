"""Bounded application envelopes over the existing NC1 reliable byte carrier.

Q=request JSON, R=reply text, P=ncw1 projects, A=receipt, E=error, S=stream event.
Wire fields: M + kind + uint16 id,total,offset (big endian) + <=88 raw bytes.
Transport ACK confirms accepted bytes only, never backend/model completion.
"""
import struct

MAX_MESSAGE = 16384
CHUNK_SIZE = 88
KINDS = {'Q': 'request', 'R': 'reply', 'P': 'projects', 'A': 'ack', 'E': 'error', 'S': 'stream'}
HEADER = struct.Struct('>ccHHH')


def chunk(kind, message_id, total, offset, payload):
    if kind not in KINDS or not 1 <= message_id <= 65535:
        raise ValueError('message identity')
    if not isinstance(payload, bytes) or not 0 <= total <= MAX_MESSAGE:
        raise ValueError('message length')
    if not 0 <= offset <= total or offset + len(payload) > total or len(payload) > CHUNK_SIZE:
        raise ValueError('chunk length')
    if not payload and (total or offset):
        raise ValueError('empty chunk')
    return HEADER.pack(b'M', kind.encode('ascii'), message_id, total, offset) + payload


def fragments(kind, message_id, text):
    """Text is encoded once: UTF-8 codepoints can span chunks, never decoded midway."""
    payload = text.encode('utf-8') if isinstance(text, str) else text
    if not isinstance(payload, bytes) or len(payload) > MAX_MESSAGE:
        raise ValueError('message length')
    for offset in range(0, max(1, len(payload)), CHUNK_SIZE):
        yield chunk(kind, message_id, len(payload), offset, payload[offset:offset+CHUNK_SIZE])


class Assembler:
    """One in-order bounded message. Feed only NEW accepted NC1 data sequences.

    Link suppresses duplicate transport packets. This class rejects replayed message
    IDs. A false/throwing callback refuses the final chunk without advancing assembly;
    handlers with effects must use durable idempotency before retrying.
    """
    def __init__(self, deliver, accepted_kinds=('Q',)):
        self.deliver = deliver
        self.accepted_kinds = frozenset(accepted_kinds)
        self.last_id = 0
        self.current = None

    def receive(self, payload, sequence=None):
        if not 8 <= len(payload) <= 96:
            raise ValueError('envelope length')
        magic, kind, mid, total, offset = HEADER.unpack(payload[:8])
        kind = kind.decode('ascii')
        text = payload[8:]
        if magic != b'M' or kind not in self.accepted_kinds:
            raise ValueError('message kind')
        chunk(kind, mid, total, offset, text)  # Validate all numeric/size fields.
        if mid != self.last_id + 1:
            raise ValueError('message sequence')
        if self.current:
            old_kind, old_mid, old_total, prefix = self.current
            if (kind, mid, total, offset) != (old_kind, old_mid, old_total, len(prefix)):
                raise ValueError('message gap/conflict')
        else:
            if offset:
                raise ValueError('message start')
            prefix = b''
        combined = prefix + text
        if len(combined) == total:
            if self.deliver(KINDS[kind], combined) is False:
                return False
            self.last_id = mid
            self.current = None
        else:
            self.current = (kind, mid, total, combined)
        return True
