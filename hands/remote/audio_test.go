package main

import (
	"bytes"
	"context"
	"io"
	"os/exec"
	"testing"
	"time"
)

func TestDesktopAudioNeverUsesDefaultSource(t *testing.T) {
	data := []byte(`[{"name":"speakers","monitor_source":"speakers.monitor"}]`)
	monitor, err := monitorSource(data, "speakers")
	if err != nil || monitor != "speakers.monitor" {
		t.Fatalf("monitor %q: %v", monitor, err)
	}
	if got, err := monitorSource([]byte(`[{"name":"speakers","monitor_source_name":"speakers.monitor","monitor_source":42}]`), "speakers"); err != nil || got != "speakers.monitor" {
		t.Fatalf("legacy monitor: %s %v", got, err)
	}
	for _, name := range []string{"", "microphone", "default"} {
		if _, err := monitorSource(data, name); err == nil {
			t.Fatalf("accepted %q", name)
		}
	}
	if _, err := monitorSource([]byte(`[{"name":"speakers"}]`), "speakers"); err == nil {
		t.Fatal("accepted missing monitor")
	}
}

func TestScreenBitrateBounds(t *testing.T) {
	for value, expected := range map[string]int{"": 6000, "1000": 1000, "40000": 40000, "100000": 100000} {
		got, err := screenBitrate(value)
		if err != nil || got != expected {
			t.Fatalf("%q: %d %v", value, got, err)
		}
	}
	for _, value := range []string{"999", "100001", "-1", "40000k", "1.5", " 6000"} {
		if _, err := screenBitrate(value); err == nil {
			t.Fatalf("accepted %q", value)
		}
	}
}

func TestLiveOpusEncoderStopsWithCapture(t *testing.T) {
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		t.Skip("ffmpeg unavailable")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	capture, err := startOpusAudio(ctx, io.Discard, []string{"-re", "-f", "lavfi", "-i", "sine=frequency=997:sample_rate=48000"})
	if err != nil {
		t.Fatal(err)
	}
	if capture.track.Codec().MimeType != "audio/opus" {
		t.Fatal("wrong audio codec")
	}
	time.Sleep(100 * time.Millisecond)
	select {
	case <-capture.done:
		t.Fatal("audio stopped unexpectedly")
	default:
	}
	capture.close()
	select {
	case <-capture.done:
	default:
		t.Fatal("encoder not reaped")
	}
}

func TestOpusPagePreservesAggregatedAndContinuedPackets(t *testing.T) {
	var pending []byte
	packets, err := splitOpusPackets([]byte{3, 3}, []byte{0xfc, 0xff, 0xfe, 0xfc, 0xff, 0xfe}, &pending)
	if err != nil || len(packets) != 2 || len(packets[0]) != 3 || len(pending) != 0 {
		t.Fatalf("aggregated packets: %v %v", packets, err)
	}
	packets, err = splitOpusPackets([]byte{255}, bytes.Repeat([]byte{1}, 255), &pending)
	if err != nil || len(packets) != 0 || len(pending) != 255 {
		t.Fatal("partial packet lost")
	}
	packets, err = splitOpusPackets([]byte{1}, []byte{2}, &pending)
	if err != nil || len(packets) != 1 || len(packets[0]) != 256 || len(pending) != 0 {
		t.Fatal("continued packet lost")
	}
	pending = make([]byte, 4000)
	if _, err = splitOpusPackets([]byte{1}, []byte{1}, &pending); err == nil {
		t.Fatal("unbounded packet accepted")
	}
}
