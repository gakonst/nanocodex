package main

import (
	"context"
	"os"
	"testing"
	"time"

	"github.com/pion/webrtc/v4"
)

func TestWaylandCaptureAndInput(t *testing.T) {
	if os.Getenv("NANOCODEX_TEST_WAYLAND") != "1" {
		t.Skip("requires the isolated labwc guest desktop")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	capture, err := startWaymote(ctx, "waymote-streamd")
	if err != nil {
		t.Fatal(err)
	}
	defer capture.close()
	publisher, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatal(err)
	}
	defer publisher.Close()
	viewer, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatal(err)
	}
	defer viewer.Close()
	sender, err := publisher.AddTrack(capture.track)
	if err != nil {
		t.Fatal(err)
	}
	go func() {
		buffer := make([]byte, 1500)
		for {
			if _, _, err := sender.Read(buffer); err != nil {
				return
			}
		}
	}()
	video := make(chan struct{})
	viewer.OnTrack(func(track *webrtc.TrackRemote, _ *webrtc.RTPReceiver) {
		for count := 0; count < 20; count++ {
			packet, _, err := track.ReadRTP()
			if err != nil {
				return
			}
			if len(packet.Payload) == 0 {
				return
			}
		}
		close(video)
	})
	offer, err := publisher.CreateOffer(nil)
	if err != nil {
		t.Fatal(err)
	}
	gathered := webrtc.GatheringCompletePromise(publisher)
	if err = publisher.SetLocalDescription(offer); err != nil {
		t.Fatal(err)
	}
	select {
	case <-gathered:
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
	if err = viewer.SetRemoteDescription(*publisher.LocalDescription()); err != nil {
		t.Fatal(err)
	}
	answer, err := viewer.CreateAnswer(nil)
	if err != nil {
		t.Fatal(err)
	}
	gathered = webrtc.GatheringCompletePromise(viewer)
	if err = viewer.SetLocalDescription(answer); err != nil {
		t.Fatal(err)
	}
	select {
	case <-gathered:
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
	if err = publisher.SetRemoteDescription(*viewer.LocalDescription()); err != nil {
		t.Fatal(err)
	}
	select {
	case <-video:
	case <-capture.done:
		t.Fatal("Waymote exited before video")
	case <-ctx.Done():
		t.Fatal("No WebRTC video", ctx.Err())
	}
	x, y := 0.5, 0.5
	button := 0
	down, up := true, false
	enter := uint16(40)
	text := "printf 'remote-control-ok\\n' > /workspace/remote-input-evidence"
	events := []remoteInput{
		{Kind: "button", X: &x, Y: &y, Button: &button, Down: &down},
		{Kind: "button", X: &x, Y: &y, Button: &button, Down: &up},
		{Kind: "text", Text: &text},
		{Kind: "key", Key: &enter, Down: &down},
		{Kind: "key", Key: &enter, Down: &up},
	}
	for index, event := range events {
		event.Generation = "guest-evidence"
		event.Sequence = uint64(index + 1)
		if err = capture.apply(event); err != nil {
			t.Fatal(err)
		}
		time.Sleep(30 * time.Millisecond)
	}
	for {
		contents, err := os.ReadFile("/workspace/remote-input-evidence")
		if err == nil && string(contents) == "remote-control-ok\n" {
			break
		}
		select {
		case <-ctx.Done():
			t.Fatalf("Terminal input did not produce the expected guest file: %v", err)
		case <-time.After(100 * time.Millisecond):
		}
	}
	if err = os.Remove("/workspace/remote-input-evidence"); err != nil {
		t.Fatal(err)
	}
}
