package main

import (
	"strings"

	"github.com/pion/interceptor"
	"github.com/pion/rtp"
	"github.com/pion/webrtc/v4"
)

const screenPlayoutDelayURI = "http://www.webrtc.org/experiments/rtp-hdrext/playout-delay"

// Screen receivers should render promptly instead of accumulating frames for
// smooth movie playback. Request a 0–100ms best-effort range, leaving
// room for jitter and audio synchronization at the receiver.
// Negotiate the extension: older viewers continue using their normal behavior.
func screenPeerAPI(settings webrtc.SettingEngine) (*webrtc.API, error) {
	media := &webrtc.MediaEngine{}
	if err := media.RegisterDefaultCodecs(); err != nil {
		return nil, err
	}
	if err := media.RegisterHeaderExtension(webrtc.RTPHeaderExtensionCapability{URI: screenPlayoutDelayURI}, webrtc.RTPCodecTypeVideo); err != nil {
		return nil, err
	}
	registry := &interceptor.Registry{}
	// Preserve Pion's NACK, RTCP reports and other default interceptors.
	if err := webrtc.RegisterDefaultInterceptors(media, registry); err != nil {
		return nil, err
	}
	registry.Add(screenPlayoutFactory{})
	return webrtc.NewAPI(webrtc.WithSettingEngine(settings), webrtc.WithMediaEngine(media), webrtc.WithInterceptorRegistry(registry)), nil
}

type screenPlayoutFactory struct{}

func (screenPlayoutFactory) NewInterceptor(string) (interceptor.Interceptor, error) {
	return &screenPlayoutInterceptor{}, nil
}

type screenPlayoutInterceptor struct{ interceptor.NoOp }

func (*screenPlayoutInterceptor) BindLocalStream(info *interceptor.StreamInfo, writer interceptor.RTPWriter) interceptor.RTPWriter {
	if !strings.HasPrefix(strings.ToLower(info.MimeType), "video/") {
		return writer
	}
	for _, extension := range info.RTPHeaderExtensions {
		if extension.URI != screenPlayoutDelayURI || extension.ID < 1 || extension.ID > 255 {
			continue
		}
		id := uint8(extension.ID)
		// Wire units are 10ms. Repeat on every packet so joins, loss and ICE
		// restarts need no extra acknowledgement or per-viewer state.
		delay, _ := (rtp.PlayoutDelayExtension{MinDelay: 0, MaxDelay: 10}).Marshal()
		return interceptor.RTPWriterFunc(func(header *rtp.Header, payload []byte, attributes interceptor.Attributes) (int, error) {
			// StaticRTP shares headers across viewers. Never leak this viewer's
			// negotiated extension ID into another viewer's packet.
			out := header.Clone()
			if err := out.SetExtension(id, delay); err != nil {
				return 0, err
			}
			return writer.Write(&out, payload, attributes)
		})
	}
	return writer
}
