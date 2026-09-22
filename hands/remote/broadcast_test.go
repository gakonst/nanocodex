package main

import (
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"
)

func TestBroadcastPresetSourceAndAspect(t *testing.T) {
	for _, tc := range []struct {
		preset                     string
		w, h, ow, oh, fps, bitrate int
	}{
		{"source", 3840, 2160, 3840, 2160, 60, 24000}, {"source", 5120, 2880, 3840, 2160, 60, 24000},
		{"source", 1920, 1200, 1920, 1200, 60, 24000}, {"1080p", 2560, 1600, 1728, 1080, 60, 8000},
		{"720p", 1920, 1080, 1280, 720, 60, 4500}, {"twitch", 1920, 1080, 1920, 1080, 60, 6000},
		{"x", 1280, 720, 1280, 720, 30, 9000}, {"1080p", 640, 480, 640, 480, 60, 8000},
	} {
		r, e := broadcastSettings(tc.preset, tc.w, tc.h)
		if e != nil || r.Width != tc.ow || r.Height != tc.oh || r.FPS != tc.fps || r.Bitrate != tc.bitrate {
			t.Fatalf("%+v -> %+v %v", tc, r, e)
		}
	}
}
func TestBroadcastURLAndWireRedaction(t *testing.T) {
	for _, u := range []string{"https://example.com/key", "file:///tmp/key", "rtmp://user:password@example.com/live/key", "rtmp://host/\nkey", "rtmp://host/", "rtmp://host:0/live/key", "rtmp://host/live/key#secret"} {
		if validateBroadcastURL(u) == nil {
			t.Fatalf("accepted invalid URL")
		}
	}
	for _, u := range []string{"rtmp://localhost:1935/live/key", "rtmps://example.com/live/key?token=private"} {
		if validateBroadcastURL(u) != nil {
			t.Fatal("valid URL rejected")
		}
	}
	r, _ := broadcastSettings("source", 1920, 1080)
	r.Status = "live"
	data, err := json.Marshal(remoteMessage{Type: "broadcast_result", ViewerID: "viewer", RequestID: "request", BroadcastResult: &r})
	if err != nil {
		t.Fatal(err)
	}
	var wire map[string]any
	_ = json.Unmarshal(data, &wire)
	if wire["status"] != "live" || wire["width"] != float64(1920) || wire["preset"] != "source" || wire["url"] != nil {
		t.Fatalf("invalid wire result %s", data)
	}
	args := strings.Join(broadcastArgs([]string{"-i", "pipe:0"}, []string{"-i", "speaker.monitor"}, r, "rtmps://example.com/live/key"), " ")
	for _, option := range []string{"-tls_verify 1", "-c:a aac", "-b:a 128k", "-g 120", "-thread_queue_size 2", "-thread_queue_size 8"} {
		if !strings.Contains(args, option) {
			t.Errorf("missing %s", option)
		}
	}
}
func TestBroadcastSupervisorLifecycle(t *testing.T) {
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		t.Skip("ffmpeg unavailable")
	}
	executable := filepath.Join(t.TempDir(), "capture")
	if err := os.WriteFile(executable, []byte("#!/bin/sh\ntrap 'exit 0' TERM INT\nwhile :; do echo out_time_us=1000000; sleep 0.1; done\n"), 0700); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	b := newDesktopBroadcast()
	defer b.stop()
	if r := b.start(ctx, executable, "rtmp://localhost/live/private", "source", 1920, 1080); r.Status != "starting" {
		t.Fatalf("start: %+v", r)
	}
	if r := b.start(ctx, executable, "rtmp://localhost/live/other", "source", 1920, 1080); r.Error != "busy" {
		t.Fatal("duplicate publisher")
	}
	deadline := time.Now().Add(4 * time.Second)
	for b.status().Status != "live" && time.Now().Before(deadline) {
		time.Sleep(20 * time.Millisecond)
	}
	if b.status().Status != "live" {
		t.Fatal("publisher never live")
	}
	cancel()
	select {
	case <-b.done:
	case <-time.After(5 * time.Second):
		t.Fatal("cancellation did not reap publisher")
	}
	b.stop()
	if b.status().Status != "stopped" {
		t.Fatal("wrong stop state")
	}
}

// Concurrent requests must serialize lifecycle changes without a duplicate
// publisher or closing a replacement publisher's completion channel.
func TestBroadcastConcurrentLifecycle(t *testing.T) {
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		t.Skip("ffmpeg unavailable")
	}
	executable := filepath.Join(t.TempDir(), "capture")
	if err := os.WriteFile(executable, []byte("#!/bin/sh\ntrap 'exit 0' TERM INT\nwhile :; do echo out_time_us=1000000; sleep 0.1; done\n"), 0700); err != nil {
		t.Fatal(err)
	}
	b := newDesktopBroadcast()
	defer b.stop()
	ctx, cancel := context.WithCancel(context.Background())
	b.start(ctx, executable, "rtmp://localhost/live/private", "source", 1920, 1080)
	cancel()
	select {
	case <-b.done:
	case <-time.After(5 * time.Second):
		t.Fatal("cancel did not finish")
	}
	if b.status().Status != "stopped" {
		t.Fatal("stale live status after cancellation")
	}
	if r := b.start(context.Background(), executable, "rtmp://localhost/live/private", "source", 1920, 1080); r.Status != "starting" {
		t.Fatalf("restart: %+v", r)
	}
	var calls sync.WaitGroup
	for i := 0; i < 8; i++ {
		calls.Add(1)
		go func() {
			defer calls.Done()
			b.stop()
			b.start(context.Background(), executable, "rtmp://localhost/live/private", "source", 1920, 1080)
			_ = b.status()
		}()
	}
	calls.Wait()
	b.stop()
}

// Run through scripts/test-hand-rtmp.py. Desktop mode uses the actual Waymote
// helper and Pulse monitor. Otherwise exercise the production FFmpeg encoder
// options with paced synthetic video/audio on platforms without Wayland.
func TestBroadcastRTMP(t *testing.T) {
	destination := os.Getenv("NANOCODEX_RTMP_TEST_URL")
	if destination == "" {
		t.Skip("private RTMP receiver not requested")
	}
	if err := validateBroadcastURL(destination); err != nil {
		t.Fatal(err)
	}
	seconds, _ := strconv.Atoi(os.Getenv("NANOCODEX_RTMP_TEST_SECONDS"))
	if seconds <= 0 {
		seconds = 10
	}
	preset := os.Getenv("NANOCODEX_RTMP_TEST_PRESET")
	ctx, cancel := context.WithTimeout(context.Background(), time.Duration(seconds)*time.Second)
	defer cancel()
	if os.Getenv("NANOCODEX_RTMP_TEST_DESKTOP") == "1" {
		b := newDesktopBroadcast()
		defer b.stop()
		r := b.start(ctx, "waymote-streamd", destination, preset, 1920, 1080)
		if r.Status != "starting" {
			t.Fatalf("start %+v", r)
		}
		live := false
		for ctx.Err() == nil {
			if b.status().Status == "live" {
				live = true
			}
			time.Sleep(100 * time.Millisecond)
		}
		b.stop()
		if !live {
			t.Fatal("desktop never published")
		}
		return
	}
	// The child is a paced synthetic capture source; startup, progress,
	// cancellation and retries all run through the production supervisor.
	t.Setenv("NANOCODEX_TEST_BROADCAST_CAPTURE", "1")
	executable, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	b := newDesktopBroadcast()
	defer b.stop()
	if r := b.start(ctx, executable, destination, preset, 1920, 1080); r.Status != "starting" {
		t.Fatalf("start: %+v", r)
	}
	live := false
	for ctx.Err() == nil {
		live = live || b.status().Status == "live"
		time.Sleep(100 * time.Millisecond)
	}
	b.stop()
	if !live {
		t.Fatal("publisher never became live")
	}
}

func runSyntheticBroadcastCapture() int {
	r, err := broadcastSettings(os.Getenv("NANOCODEX_BROADCAST_PRESET"), 1920, 1080)
	if err != nil {
		return 1
	}
	args := broadcastArgs([]string{"-re", "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=" + strconv.Itoa(r.FPS)}, []string{"-re", "-f", "lavfi", "-i", "sine=frequency=997:sample_rate=48000"}, r, os.Getenv("NANOCODEX_BROADCAST_URL"))
	command := exec.Command("ffmpeg", args...)
	command.Stdout = os.Stdout
	if command.Run() != nil {
		return 1
	}
	return 0
}
