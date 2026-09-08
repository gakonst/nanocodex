package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/pion/webrtc/v4"
)

type discardedHostInput struct{}

func (discardedHostInput) Write(data []byte) (int, error) { return io.Discard.Write(data) }
func (discardedHostInput) Close() error                   { return nil }

func TestHostPreservesReplacementCloseFromBroker(t *testing.T) {
	for _, waitForCatalog := range []bool{false, true} {
		t.Run(fmt.Sprintf("catalog-%t", waitForCatalog), func(t *testing.T) {
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.URL.Path != "/v1/account/hands/host" || r.Header.Get("Authorization") != "Bearer test-publisher" {
					http.Error(w, "unauthorized", 401)
					return
				}
				socket, err := websocket.Accept(w, r, nil)
				if err != nil {
					return
				}
				defer socket.CloseNow()
				_ = socket.Write(ctx, websocket.MessageText, []byte(`{"type":"ready","connection_id":"publisher"}`))
				if waitForCatalog {
					_, data, err := socket.Read(ctx)
					var message remoteMessage
					if err != nil || json.Unmarshal(data, &message) != nil || message.Type != "catalog" {
						t.Error("publisher did not send its catalog")
						return
					}
				}
				_ = socket.Close(websocket.StatusPolicyViolation, "Host replaced")
			}))
			defer server.Close()
			credential := filepath.Join(t.TempDir(), "credential")
			if err := os.WriteFile(credential, []byte("test-publisher"), 0600); err != nil {
				t.Fatal(err)
			}
			// No compositor, physical input or network peer: exercise the real
			// authenticated WebSocket and publisher cancellation loop only.
			capture := &waymoteCapture{input: discardedHostInput{}, done: make(chan struct{})}
			err := serveWayland(ctx, hostConfig{Origin: server.URL, CredentialFile: credential,
				MachineID: "replacement-test", Name: "Replacement test", Width: 640, Height: 360,
				Frames: true, quiet: true, capture: capture})
			if !errors.Is(err, errRemoteHostReplaced) {
				t.Fatalf("publisher lost terminal broker close: %v", err)
			}
		})
	}
}

// Exercise the actual host, account broker, compositor, video and input with a
// short renewal interval. Run in the isolated desktop used for browser evidence.
func TestAccountWaylandControlSurvivesICERenewal(t *testing.T) {
	origin, credential := os.Getenv("NANOCODEX_TEST_REMOTE_ORIGIN"), os.Getenv("NANOCODEX_TEST_REMOTE_CREDENTIAL")
	if os.Getenv("NANOCODEX_TEST_WAYLAND") != "1" || origin == "" || credential == "" {
		t.Skip("requires the isolated Wayland desktop and local account service")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	service, err := newRemoteService(origin, credential)
	if err != nil {
		t.Fatal(err)
	}
	if service.base.Hostname() != "127.0.0.1" {
		t.Fatal("requires the isolated loopback account service")
	}
	machine := fmt.Sprintf("wayland-renewal-%d", time.Now().UnixNano())
	hostDone := make(chan error, 1)
	go func() {
		hostDone <- serveWayland(ctx, hostConfig{Origin: origin, CredentialFile: credential,
			MachineID: machine, Name: "Wayland renewal test", Waymote: "waymote-streamd", Width: 1600, Height: 900,
			IncludeLoopback: true, ICERenewalInterval: 2 * time.Second})
	}()
	defer func() {
		cancel()
		select {
		case <-hostDone:
		case <-time.After(3 * time.Second):
			t.Error("host failed to stop")
		}
	}()
	var generation string
	for generation == "" {
		request, _ := http.NewRequestWithContext(ctx, "GET", origin+"/v1/account/hands/screens", nil)
		request.Header.Set("Authorization", "Bearer "+service.token)
		response, err := service.client.Do(request)
		if err != nil {
			t.Fatal(err)
		}
		var catalog struct {
			Surfaces []struct {
				Machine    string `json:"machine_id"`
				Generation string `json:"generation"`
			} `json:"surfaces"`
		}
		err = json.NewDecoder(response.Body).Decode(&catalog)
		response.Body.Close()
		if err != nil {
			t.Fatal(err)
		}
		for _, surface := range catalog.Surfaces {
			if surface.Machine == machine {
				generation = surface.Generation
			}
		}
		if generation == "" {
			select {
			case <-ctx.Done():
				t.Fatal("host did not publish")
			case <-time.After(50 * time.Millisecond):
			}
		}
	}
	endpoint := *service.base
	endpoint.Scheme = "ws"
	endpoint.Path = "/v1/account/hands/view"
	endpoint.RawQuery = url.Values{"machine_id": {machine}, "surface_id": {"desktop"}, "generation": {generation}}.Encode()
	socket, _, err := websocket.Dial(ctx, endpoint.String(), &websocket.DialOptions{HTTPHeader: http.Header{"Authorization": {"Bearer " + service.token}}})
	if err != nil {
		t.Fatal(err)
	}
	defer socket.CloseNow()
	settings := webrtc.SettingEngine{}
	settings.SetIncludeLoopbackCandidate(true)
	viewer, err := webrtc.NewAPI(webrtc.WithSettingEngine(settings)).NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatal(err)
	}
	defer viewer.Close()
	failures := make(chan error, 8)
	fail := func(err error) {
		if err != nil {
			select {
			case failures <- err:
			default:
			}
		}
	}
	var writeMu sync.Mutex
	send := func(signal remoteSignal) {
		data, err := json.Marshal(remoteMessage{Type: "signal", Signal: &signal})
		if err != nil {
			fail(err)
			return
		}
		writeMu.Lock()
		defer writeMu.Unlock()
		fail(socket.Write(ctx, websocket.MessageText, data))
	}
	viewer.OnICECandidate(func(candidate *webrtc.ICECandidate) {
		if candidate != nil {
			c := candidate.ToJSON()
			send(remoteSignal{Type: "candidate", Candidate: c.Candidate, SDPMid: c.SDPMid, SDPMLineIndex: c.SDPMLineIndex})
		}
	})
	var packets atomic.Int64
	viewer.OnTrack(func(track *webrtc.TrackRemote, _ *webrtc.RTPReceiver) {
		for {
			if _, _, err := track.ReadRTP(); err != nil {
				return
			}
			packets.Add(1)
		}
	})
	type grant struct {
		channel    *webrtc.DataChannel
		generation string
	}
	granted := make(chan grant, 1)
	opened := make(chan *webrtc.DataChannel, 1)
	viewer.OnDataChannel(func(channel *webrtc.DataChannel) {
		if channel.Label() != "remote-control-v1" {
			return
		}
		channel.OnOpen(func() { opened <- channel })
		channel.OnMessage(func(data webrtc.DataChannelMessage) {
			var message controlMessage
			if json.Unmarshal(data.Data, &message) == nil && message.Type == "granted" {
				granted <- grant{channel, message.Generation}
			}
		})
	})
	offers := make(chan string, 8)
	go func() {
		var candidates []webrtc.ICECandidateInit
		for {
			_, data, err := socket.Read(ctx)
			if err != nil {
				if ctx.Err() == nil {
					fail(err)
				}
				return
			}
			var message remoteMessage
			if err = json.Unmarshal(data, &message); err != nil {
				fail(err)
				return
			}
			if message.Signal == nil {
				continue
			}
			signal := message.Signal
			if signal.Type == "candidate" {
				candidate := webrtc.ICECandidateInit{Candidate: signal.Candidate, SDPMid: signal.SDPMid, SDPMLineIndex: signal.SDPMLineIndex}
				if viewer.RemoteDescription() == nil {
					candidates = append(candidates, candidate)
				} else {
					fail(viewer.AddICECandidate(candidate))
				}
				continue
			}
			if signal.Type != "offer" {
				fail(fmt.Errorf("unexpected signal %s", signal.Type))
				return
			}
			if err = viewer.SetRemoteDescription(webrtc.SessionDescription{Type: webrtc.SDPTypeOffer, SDP: signal.SDP}); err != nil {
				fail(err)
				return
			}
			for _, candidate := range candidates {
				fail(viewer.AddICECandidate(candidate))
			}
			candidates = nil
			answer, err := viewer.CreateAnswer(nil)
			if err != nil {
				fail(err)
				return
			}
			if err = viewer.SetLocalDescription(answer); err != nil {
				fail(err)
				return
			}
			send(remoteSignal{Type: "answer", SDP: answer.SDP})
			for _, line := range strings.Split(signal.SDP, "\r\n") {
				if strings.HasPrefix(line, "a=ice-ufrag:") {
					offers <- line
					break
				}
			}
		}
	}()
	waitOffer := func() string {
		t.Helper()
		select {
		case value := <-offers:
			return value
		case err := <-failures:
			t.Fatal(err)
		case <-ctx.Done():
			t.Fatal("no ICE offer")
		}
		return ""
	}
	first, renewed := waitOffer(), waitOffer()
	if first == renewed {
		t.Fatal("ICE renewal reused the old ICE credentials")
	}
	select {
	case channel := <-opened:
		fail(channel.SendText(`{"type":"acquire"}`))
	case <-ctx.Done():
		t.Fatal("input channel did not open")
	}
	var control grant
	select {
	case control = <-granted:
	case err := <-failures:
		t.Fatal(err)
	case <-ctx.Done():
		t.Fatal("control was not granted")
	}
	before := packets.Load()
	path := "/workspace/" + machine
	defer os.Remove(path)
	x, y, button, down, up, enter := 0.5, 0.5, 0, true, false, uint16(40)
	command := "printf 'renewed\\n' > " + path
	inputs := []remoteInput{{Kind: "button", X: &x, Y: &y, Button: &button, Down: &down}, {Kind: "button", X: &x, Y: &y, Button: &button, Down: &up},
		{Kind: "text", Text: &command}, {Kind: "key", Key: &enter, Down: &down}, {Kind: "key", Key: &enter, Down: &up}}
	for i, input := range inputs {
		input.Sequence = uint64(i + 1)
		input.Generation = control.generation
		data, _ := json.Marshal(input)
		if err = control.channel.SendText(string(data)); err != nil {
			t.Fatal(err)
		}
	}
	for {
		data, _ := os.ReadFile(path)
		if string(data) == "renewed\n" && packets.Load() > before {
			break
		}
		select {
		case err := <-failures:
			t.Fatal(err)
		case <-ctx.Done():
			t.Fatalf("video/input did not survive ICE renewal: packets=%d initial=%d channel=%s file=%q", packets.Load(), before, control.channel.ReadyState(), string(data))
		case <-time.After(50 * time.Millisecond):
		}
	}
}
