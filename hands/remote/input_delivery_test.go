package main

import (
	"bytes"
	"encoding/binary"
	"errors"
	"io"
	"strings"
	"testing"
	"unicode/utf8"
)

type recordingInput struct {
	writes  [][]byte
	closed  bool
	failAt  int
	failure error
}

func (w *recordingInput) Write(p []byte) (int, error) {
	w.writes = append(w.writes, append([]byte(nil), p...))
	if len(w.writes) == w.failAt {
		return len(p) / 2, w.failure
	}
	return len(p), nil
}
func (w *recordingInput) Close() error { w.closed = true; return nil }

func TestTextRecordDelivery(t *testing.T) {
	t.Setenv("NANOCODEX_WAYLAND_TEXT_X11", "")
	t.Setenv("NANOCODEX_WAYLAND_TEXT_WTYPE", "")
	for _, text := range []string{
		"mkdir -p /tmp/example; grim -t png /tmp/example/desktop.png; chmod 644 /tmp/example/*",
		strings.Repeat("x", 4000), strings.Repeat("x", 4096), strings.Repeat("x", 3999) + "🌍" + strings.Repeat("é", 46),
	} {
		w := &recordingInput{}
		c := waymoteCapture{input: w, sequence: ^uint32(0)}
		if err := c.apply(remoteInput{Kind: "text", Text: &text, Sequence: 1, Generation: "test"}); err != nil {
			t.Fatal(err)
		}
		var received bytes.Buffer
		for i, p := range w.writes {
			if len(p) > 4016 || len(p) < 17 || p[0] != 2 || p[1] != 10 || binary.LittleEndian.Uint32(p[4:8]) != uint32(len(p)-16) || binary.LittleEndian.Uint32(p[8:12]) != uint32(i+1) || binary.LittleEndian.Uint32(p[12:16]) != 0 {
				t.Fatalf("invalid text frame %d", i)
			}
			if !utf8.Valid(p[16:]) {
				t.Fatal("split UTF-8 codepoint")
			}
			received.Write(p[16:])
		}
		if received.String() != text {
			t.Fatal("text bytes changed")
		}
	}
}

func TestInputWriteFailureStopsFurtherDelivery(t *testing.T) {
	t.Setenv("NANOCODEX_WAYLAND_TEXT_X11", "")
	t.Setenv("NANOCODEX_WAYLAND_TEXT_WTYPE", "")
	for _, failure := range []error{nil, io.ErrClosedPipe} {
		for _, failAt := range []int{1, 2} {
			w := &recordingInput{failAt: failAt, failure: failure}
			c := waymoteCapture{input: w}
			text := strings.Repeat("a", 4096)
			err := c.apply(remoteInput{Kind: "text", Text: &text, Sequence: 1, Generation: "test"})
			want := failure
			if want == nil {
				want = io.ErrShortWrite
			}
			if !errors.Is(err, want) || !w.closed {
				t.Fatalf("error=%v closed=%v", err, w.closed)
			}
			if c.releaseAll() != err {
				t.Fatal("release did not retain failure")
			}
			if c.apply(remoteInput{Kind: "text", Text: &text, Sequence: 2, Generation: "test"}) != err {
				t.Fatal("subsequent text did not retain failure")
			}
			if len(w.writes) != failAt {
				t.Fatal("appended input to damaged stream")
			}
		}
	}
}

func TestStandardExtendedKeys(t *testing.T) {
	for usage, evdev := range map[uint16]uint32{70: 99, 71: 70, 72: 119, 101: 127, 102: 116, 104: 183, 105: 184, 106: 185, 107: 186, 108: 187, 109: 188, 110: 189, 111: 190, 112: 191, 113: 192, 114: 193, 115: 194} {
		w := &recordingInput{}
		c := waymoteCapture{input: w}
		for _, down := range []bool{true, false} {
			if err := c.apply(remoteInput{Kind: "key", Key: &usage, Down: &down, Sequence: 1, Generation: "test"}); err != nil {
				t.Fatalf("usage %d: %v", usage, err)
			}
		}
		if len(w.writes) != 2 || w.writes[0][2] != 1 || w.writes[1][2] != 0 {
			t.Fatal("lost key edge")
		}
		for _, p := range w.writes {
			if binary.LittleEndian.Uint32(p[4:8]) != evdev {
				t.Fatalf("usage %d mapping", usage)
			}
		}
	}
}

func TestTextRejectsInvalidUTF8BeforeWrite(t *testing.T) {
	w := &recordingInput{}
	c := waymoteCapture{input: w}
	text := string([]byte{0xff})
	if c.apply(remoteInput{Kind: "text", Text: &text, Sequence: 1, Generation: "test"}) == nil || len(w.writes) != 0 {
		t.Fatal("invalid UTF-8 reached daemon")
	}
}

// CPU serialization only: excludes pipes, compositor, application, and network.
func BenchmarkTextRecordSerialization(b *testing.B) {
	b.Setenv("NANOCODEX_WAYLAND_TEXT_X11", "")
	b.Setenv("NANOCODEX_WAYLAND_TEXT_WTYPE", "")
	text := strings.Repeat("x", 4096)
	c := waymoteCapture{input: discardedHostInput{}}
	event := remoteInput{Kind: "text", Text: &text, Sequence: 1, Generation: "test"}
	b.SetBytes(int64(len(text)))
	b.ReportAllocs()
	for b.Loop() {
		if err := c.apply(event); err != nil {
			b.Fatal(err)
		}
	}
}
