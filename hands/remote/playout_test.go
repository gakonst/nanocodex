package main

import (
	"bytes"
	"context"
	"testing"
	"time"

	"github.com/pion/interceptor"
	"github.com/pion/rtp"
	"github.com/pion/webrtc/v4"
)

func TestScreenPlayoutIsNegotiatedPerViewer(t *testing.T) {
	original := &rtp.Header{Version: 2, SequenceNumber: 123}
	if err := original.SetExtension(3, []byte{42}); err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		name, mime string
		id         int
		want       bool
	}{
		{"video", "video/H264", 7, true}, {"other-viewer", "video/H264", 8, true},
		{"unsupported", "video/H264", 0, false}, {"audio", "audio/opus", 7, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			info := &interceptor.StreamInfo{MimeType: tc.mime}
			if tc.id != 0 {
				info.RTPHeaderExtensions = []interceptor.RTPHeaderExtension{{URI: screenPlayoutDelayURI, ID: tc.id}}
			}
			called := false
			writer := (&screenPlayoutInterceptor{}).BindLocalStream(info, interceptor.RTPWriterFunc(func(h *rtp.Header, payload []byte, _ interceptor.Attributes) (int, error) {
				called = true
				if h.SequenceNumber != 123 || !bytes.Equal(payload, []byte{9}) || !bytes.Equal(h.GetExtension(3), []byte{42}) {
					t.Fatal("changed existing RTP data")
				}
				if tc.want {
					var delay rtp.PlayoutDelayExtension
					if err := delay.Unmarshal(h.GetExtension(uint8(tc.id))); err != nil {
						t.Fatal(err)
					}
					if delay.MinDelay != 0 || delay.MaxDelay != 10 {
						t.Fatalf("wrong wire units: %+v", delay)
					}
				} else if len(h.GetExtensionIDs()) != 1 {
					t.Fatal("unnegotiated extension")
				}
				return len(payload), nil
			}))
			if _, err := writer.Write(original, []byte{9}, nil); err != nil {
				t.Fatal(err)
			}
			if !called || len(original.GetExtensionIDs()) != 1 {
				t.Fatal("shared header mutated")
			}
		})
	}
}

func TestScreenPlayoutOverRealPeerConnection(t *testing.T) {
	for _, supported := range []bool{true, false} {
		t.Run(map[bool]string{true: "supported", false: "legacy"}[supported], func(t *testing.T) {
			settings := webrtc.SettingEngine{}
			settings.SetIncludeLoopbackCandidate(true)
			api, err := screenPeerAPI(settings)
			if err != nil {
				t.Fatal(err)
			}
			publisher, err := api.NewPeerConnection(webrtc.Configuration{})
			if err != nil {
				t.Fatal(err)
			}
			defer publisher.Close()
			viewerAPI := webrtc.NewAPI(webrtc.WithSettingEngine(settings))
			if supported {
				viewerAPI, err = screenPeerAPI(settings)
				if err != nil {
					t.Fatal(err)
				}
			}
			viewer, err := viewerAPI.NewPeerConnection(webrtc.Configuration{})
			if err != nil {
				t.Fatal(err)
			}
			defer viewer.Close()
			track, err := webrtc.NewTrackLocalStaticRTP(webrtc.RTPCodecCapability{MimeType: webrtc.MimeTypeH264}, "screen", "test")
			if err != nil {
				t.Fatal(err)
			}
			sender, err := publisher.AddTrack(track)
			if err != nil {
				t.Fatal(err)
			}
			go func() {
				b := make([]byte, 1500)
				for {
					if _, _, err := sender.Read(b); err != nil {
						return
					}
				}
			}()
			received := make(chan *rtp.Packet, 1)
			viewer.OnTrack(func(remote *webrtc.TrackRemote, _ *webrtc.RTPReceiver) {
				p, _, err := remote.ReadRTP()
				if err == nil {
					received <- p
				}
			})
			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			gather := func(peer *webrtc.PeerConnection, description webrtc.SessionDescription) {
				done := webrtc.GatheringCompletePromise(peer)
				if err := peer.SetLocalDescription(description); err != nil {
					t.Fatal(err)
				}
				select {
				case <-done:
				case <-ctx.Done():
					t.Fatal(ctx.Err())
				}
			}
			offer, err := publisher.CreateOffer(nil)
			if err != nil {
				t.Fatal(err)
			}
			gather(publisher, offer)
			if err := viewer.SetRemoteDescription(*publisher.LocalDescription()); err != nil {
				t.Fatal(err)
			}
			answer, err := viewer.CreateAnswer(nil)
			if err != nil {
				t.Fatal(err)
			}
			gather(viewer, answer)
			if err := publisher.SetRemoteDescription(*viewer.LocalDescription()); err != nil {
				t.Fatal(err)
			}
			var id int
			for _, ext := range sender.GetParameters().HeaderExtensions {
				if ext.URI == screenPlayoutDelayURI {
					id = ext.ID
				}
			}
			if (id != 0) != supported {
				t.Fatalf("negotiated id %d supported=%v", id, supported)
			}
			ticker := time.NewTicker(20 * time.Millisecond)
			defer ticker.Stop()
			for sequence := uint16(0); ; sequence++ {
				select {
				case <-ticker.C:
					if err := track.WriteRTP(&rtp.Packet{Header: rtp.Header{Version: 2, SequenceNumber: sequence, Timestamp: uint32(sequence) * 1800, Marker: true}, Payload: []byte{0x65, 0x88, 0x84}}); err != nil {
						t.Fatal(err)
					}
				case packet := <-received:
					if supported {
						var delay rtp.PlayoutDelayExtension
						if err := delay.Unmarshal(packet.GetExtension(uint8(id))); err != nil {
							t.Fatal(err)
						}
						if delay.MaxDelay != 10 {
							t.Fatalf("wrong playout delay: %+v", delay)
						}
					}
					return
				case <-ctx.Done():
					t.Fatal("no media", ctx.Err())
				}
			}
		})
	}
}
