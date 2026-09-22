package main

import (
	"bytes"
	"encoding/binary"
	"io"
	"math"
	"testing"
)

type bufferedInput struct{ bytes.Buffer }

func (*bufferedInput) Close() error { return nil }

func TestRelativePointerProtocol(t *testing.T) {
	for _, value := range []string{
		`{"kind":"relativeMove","deltaX":-12.5,"deltaY":4096}`,
		`{"kind":"button","button":0,"down":true}`,
		`{"kind":"scroll","deltaX":0,"deltaY":-2}`,
	} {
		if _, err := decodeInput([]byte(value[:len(value)-1] + `,"sequence":1,"generation":"lease"}`)); err != nil {
			t.Fatal(value, err)
		}
	}
	for _, value := range []string{
		`{"kind":"relativeMove","deltaX":4097,"deltaY":0}`,
		`{"kind":"relativeMove","deltaX":1}`,
		`{"kind":"relativeMove","deltaX":1,"deltaY":0,"x":0.5,"y":0.5}`,
		`{"kind":"relativeMove","deltaX":1,"deltaY":0,"button":0}`,
		`{"kind":"button","button":0,"down":true,"x":0.5}`,
		`{"kind":"scroll","deltaX":0,"deltaY":1,"y":0.5}`,
	} {
		if _, err := decodeInput([]byte(value[:len(value)-1] + `,"sequence":1,"generation":"lease"}`)); err == nil {
			t.Fatal("accepted malformed relative input", value)
		}
	}
}

func TestRelativeWaymoteInputDoesNotWarpPointer(t *testing.T) {
	output := &bufferedInput{}
	var _ io.WriteCloser = output
	capture := waymoteCapture{input: output}
	dx, dy, button, down := -12.5, 4.25, 0, true
	for i, event := range []remoteInput{
		{Kind: "relativeMove", DeltaX: &dx, DeltaY: &dy},
		{Kind: "button", Button: &button, Down: &down},
	} {
		event.Sequence, event.Generation = uint64(i+1), "lease"
		if err := capture.apply(event); err != nil {
			t.Fatal(err)
		}
	}
	data := output.Bytes()
	if len(data) != 32 || data[1] != 8 || data[17] != 2 {
		t.Fatalf("unexpected wire records: %v", data)
	}
	if math.Float32frombits(binary.LittleEndian.Uint32(data[4:8])) != -12.5 || math.Float32frombits(binary.LittleEndian.Uint32(data[8:12])) != 4.25 {
		t.Fatal("relative displacement changed")
	}
}
