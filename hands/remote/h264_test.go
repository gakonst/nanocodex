package main

import (
	"bytes"
	"io"
	"testing"
	"testing/iotest"

	"github.com/pion/rtp"
	"github.com/pion/rtp/codecs"
)

func TestWaymotePipeBecomesBrowserSizedRTP(t *testing.T) {
	first := append([]byte{0, 0, 0, 1, 9, 0xf0, 0, 0, 1, 0x65}, bytes.Repeat([]byte{0x35}, 150_000)...)
	second := []byte{0, 0, 1, 9, 0xf0, 0, 0, 0, 1, 0x41, 0x35}
	stream := append(append([]byte{}, first...), second...)
	for _, reader := range []io.Reader{bytes.NewReader(stream), iotest.HalfReader(bytes.NewReader(stream))} {
		forwarder := h264Forwarder{}
		decoder := codecs.H264Packet{}
		var decoded []byte
		count, markers := 0, 0
		err := forwarder.read(reader, func(packet *rtp.Packet) error {
			count++
			if packet.MarshalSize() > 1200 {
				t.Fatal("oversized WebRTC packet")
			}
			if packet.Timestamp != uint32((markers+1)*1500) || packet.SequenceNumber != uint16(count) {
				t.Fatal("invalid video clock or packet sequence")
			}
			if packet.Marker {
				markers++
			}
			data, err := decoder.Unmarshal(packet.Payload)
			decoded = append(decoded, data...)
			return err
		})
		// The RTP H.264 payloader discards AUDs; the encoded slices are unchanged.
		want := append([]byte{0, 0, 0, 1, 0x65}, bytes.Repeat([]byte{0x35}, 150_000)...)
		want = append(want, 0, 0, 0, 1, 0x41, 0x35)
		if err != nil || count < 100 || markers != 2 || !bytes.Equal(decoded, want) {
			t.Fatalf("frame transfer failed: %v, %d packets, %d markers, %d bytes", err, count, markers, len(decoded))
		}
	}
}

func TestWaymotePipeBoundsAndSplitDelimiters(t *testing.T) {
	unit := []byte{0, 0, 0, 1, 9, 0xf0, 0, 0, 1, 0x65, 0x35}
	stream := append(append([]byte{}, unit...), unit...)
	for _, reader := range []io.Reader{iotest.OneByteReader(bytes.NewReader(stream)), bytes.NewReader(stream)} {
		frames := 0
		err := (&h264Forwarder{}).read(reader, func(p *rtp.Packet) error {
			if p.Marker {
				frames++
			}
			return nil
		})
		if err != nil || frames != 2 {
			t.Fatalf("split delimiter: %v, %d frames", err, frames)
		}
	}
	for _, data := range [][]byte{[]byte("not video"), append(unit, bytes.Repeat([]byte{0x35}, maxH264Frame)...)} {
		if err := (&h264Forwarder{}).read(bytes.NewReader(data), func(*rtp.Packet) error { t.Fatal("invalid frame emitted"); return nil }); err == nil {
			t.Fatal("invalid stream accepted")
		}
	}
}
