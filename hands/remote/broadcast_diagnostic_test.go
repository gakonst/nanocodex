package main

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestBroadcastDiagnosticBounded(t *testing.T) {
	d := &broadcastDiagnostic{}
	d.Write([]byte(strings.Repeat("private-key", 10000)))
	if len(d.fragment) > 4096 {
		t.Fatal("unbounded diagnostic")
	}
	if d.category() != "broadcast_failed" {
		t.Fatal("arbitrary text became diagnostic")
	}
}

func TestBroadcastFakeFailures(t *testing.T) {
	for _, tc := range []struct{ message, category, wire string }{
		{"error: ControlConnectionClosed", "capture_failed", "capture_failed"},
		{"401 Unauthorized rtmps://secret/live/key", "authentication_failed", "connection_failed"},
		{"Connection refused", "connection_failed", "connection_failed"},
		{"Unknown encoder 'libx264'", "encoder_failed", "encoder_failed"},
		{"unexpected private-key", "broadcast_failed", "broadcast_failed"},
	} {
		t.Run(tc.category, func(t *testing.T) {
			dir := t.TempDir()
			t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
			os.WriteFile(filepath.Join(dir, "ffmpeg"), []byte("#!/bin/sh\nexit 0\n"), 0700)
			executable := filepath.Join(dir, "capture")
			if err := os.WriteFile(executable, []byte("#!/bin/sh\nprintf '%s\\n' \"$FAKE_FAILURE\" >&2\nexit 1\n"), 0700); err != nil {
				t.Fatal(err)
			}
			t.Setenv("FAKE_FAILURE", tc.message)
			d := &broadcastDiagnostic{}
			d.Write([]byte(tc.message))
			if d.category() != tc.category {
				t.Fatalf("category %s", d.category())
			}
			b := newDesktopBroadcast()
			defer b.stop()
			if r := b.start(context.Background(), executable, "rtmp://private-host/live/private-key", "source", 1920, 1080); r.Status != "starting" {
				t.Fatal(r)
			}
			deadline := time.Now().Add(3 * time.Second)
			for b.status().Status != "reconnecting" && time.Now().Before(deadline) {
				time.Sleep(10 * time.Millisecond)
			}
			if r := b.status(); r.Status != "reconnecting" || r.Error != tc.wire {
				t.Fatalf("result %+v", r)
			}
		})
	}
}

func TestBroadcastKeepsControlInputOpen(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	os.WriteFile(filepath.Join(dir, "ffmpeg"), []byte("#!/bin/sh\nexit 0\n"), 0700)
	executable := filepath.Join(dir, "capture")
	// EOF makes read return immediately; an open pipe permits progress.
	script := "#!/bin/sh\n(sleep 0.1; while :; do echo out_time_us=1000000; sleep 0.1; done) &\nchild=$!\nread control\nkill $child 2>/dev/null\necho 'error: ControlConnectionClosed' >&2\nexit 1\n"
	if err := os.WriteFile(executable, []byte(script), 0700); err != nil {
		t.Fatal(err)
	}
	b := newDesktopBroadcast()
	defer b.stop()
	if r := b.start(context.Background(), executable, "rtmp://private-host/live/private-key", "source", 1920, 1080); r.Status != "starting" {
		t.Fatal(r)
	}
	deadline := time.Now().Add(3 * time.Second)
	for b.status().Status != "live" && time.Now().Before(deadline) {
		time.Sleep(10 * time.Millisecond)
	}
	if r := b.status(); r.Status != "live" {
		t.Fatalf("control pipe closed: %+v", r)
	}
	b.stop()
	if b.status().Status != "stopped" {
		t.Fatal("did not stop")
	}
}
