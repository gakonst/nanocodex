package main

import (
	"bytes"
	"fmt"
	"io"
	"testing"

	"github.com/pion/rtp"
)

// Encoded synthetic bytes only; no capture, encoder, socket, decoder or display.
func BenchmarkFramedH264Packetization(b *testing.B) {
	for _, size := range []int{4096, 65536, 125000, 1048576} {
		b.Run(fmt.Sprint(size), func(b *testing.B) {
			frame := bytes.Repeat([]byte{0x35}, size)
			copy(frame, []byte{0, 0, 0, 1, 0x65})
			var wire bytes.Buffer
			wire.WriteString(chunkedH264Magic)
			if err := writeEncodedFrame(&wire, frame); err != nil {
				b.Fatal(err)
			}
			payload := wire.Bytes()
			b.SetBytes(int64(size))
			b.ReportAllocs()
			for b.Loop() {
				if err := (&h264Forwarder{}).read(bytes.NewReader(payload), func(*rtp.Packet) error { return nil }); err != nil {
					b.Fatal(err)
				}
			}
		})
	}
}

func TestForwarderStopsOnSinkFailure(t *testing.T) {
	var wire bytes.Buffer
	wire.WriteString(chunkedH264Magic)
	frame := append([]byte{0, 0, 0, 1, 0x65}, bytes.Repeat([]byte{0x35}, 10000)...)
	if err := writeEncodedFrame(&wire, frame); err != nil {
		t.Fatal(err)
	}
	writes := 0
	err := (&h264Forwarder{}).read(&wire, func(*rtp.Packet) error { writes++; return io.ErrClosedPipe })
	if err != io.ErrClosedPipe || writes != 1 {
		t.Fatalf("writes=%d err=%v", writes, err)
	}
}
