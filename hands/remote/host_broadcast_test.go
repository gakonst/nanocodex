package main

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
)

type blockedHostBroadcast struct {
	entered chan struct{}
	cleaned chan struct{}
	release chan struct{}
	once    sync.Once
	stops   atomic.Int32
	starts  chan string
}

func newBlockedHostBroadcast() *blockedHostBroadcast {
	return &blockedHostBroadcast{entered: make(chan struct{}), cleaned: make(chan struct{}), release: make(chan struct{}), starts: make(chan string, 16)}
}
func (b *blockedHostBroadcast) unblock() { b.once.Do(func() { close(b.release) }) }
func (b *blockedHostBroadcast) start(ctx context.Context, _, destination, _ string, _, _ int) broadcastResult {
	b.starts <- destination
	return broadcastResult{Status: "starting"}
}
func (b *blockedHostBroadcast) stop() {
	switch b.stops.Add(1) {
	case 1:
		close(b.entered)
		<-b.release
	case 2:
		close(b.cleaned)
	}
}
func (b *blockedHostBroadcast) status() broadcastResult { return broadcastResult{Status: "stopped"} }

func awaitBroadcastSignal(t *testing.T, signal <-chan struct{}) {
	t.Helper()
	select {
	case <-signal:
	case <-time.After(3 * time.Second):
		t.Fatal("broadcast synchronization timed out")
	}
}

func TestHostBroadcastFIFOAndOverload(t *testing.T) {
	backend := newBlockedHostBroadcast()
	results := make(chan hostEvent, hostBroadcastQueueSize+1)
	worker := newHostBroadcast(context.Background(), backend, hostConfig{}, func(event hostEvent) { results <- event })
	defer worker.close()
	defer backend.unblock()
	if !worker.enqueue(remoteMessage{Action: "stop", ViewerID: "first-viewer", RequestID: "stop"}) {
		t.Fatal("stop rejected")
	}
	awaitBroadcastSignal(t, backend.entered)
	for i := 0; i < hostBroadcastQueueSize; i++ {
		id := string(rune('a' + i))
		if !worker.enqueue(remoteMessage{Action: "start", URL: id, ViewerID: "other-viewer", RequestID: id}) {
			t.Fatal("bounded queue filled too early")
		}
	}
	if worker.enqueue(remoteMessage{Action: "start", URL: "overflow"}) {
		t.Fatal("unbounded broadcast queue")
	}
	select {
	case <-backend.starts:
		t.Fatal("start overtook blocked stop")
	case <-results:
		t.Fatal("stop acknowledged before cleanup completed")
	default:
	}
	backend.unblock()
	for i := 0; i <= hostBroadcastQueueSize; i++ {
		select {
		case event := <-results:
			message := event.broadcast
			id, viewer, status := "stop", "first-viewer", "stopped"
			if i > 0 {
				id, viewer, status = string(rune('a'+i-1)), "other-viewer", "starting"
				if destination := <-backend.starts; destination != id {
					t.Fatalf("start order: got %q want %q", destination, id)
				}
			}
			if message == nil || message.Type != "broadcast_result" || message.ViewerID != viewer || message.RequestID != id || message.BroadcastResult.Status != status {
				t.Fatalf("incorrect ordered result: %+v", message)
			}
		case <-time.After(3 * time.Second):
			t.Fatal("missing broadcast result")
		}
	}
}

func TestHostBroadcastCancellationDiscardsQueuedStartsAndWaitsForCleanup(t *testing.T) {
	backend := newBlockedHostBroadcast()
	results := make(chan hostEvent, 4)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	worker := newHostBroadcast(ctx, backend, hostConfig{}, func(event hostEvent) { results <- event })
	defer worker.close()
	defer backend.unblock()
	worker.enqueue(remoteMessage{Action: "stop"})
	awaitBroadcastSignal(t, backend.entered)
	worker.enqueue(remoteMessage{Action: "start", URL: "must-not-start"})
	cancel()
	if worker.enqueue(remoteMessage{Action: "status"}) {
		t.Fatal("accepted request after cancellation")
	}
	closed := make(chan struct{})
	go func() { worker.close(); close(closed) }()
	select {
	case <-closed:
		t.Fatal("shutdown returned before cleanup finished")
	default:
	}
	backend.unblock()
	awaitBroadcastSignal(t, closed)
	if backend.stops.Load() != 2 {
		t.Fatal("worker did not perform final broadcast cleanup")
	}
	select {
	case <-backend.starts:
		t.Fatal("queued start ran after cancellation")
	case <-results:
		t.Fatal("cancelled operation emitted a result")
	default:
	}
}

type hostBroadcastReply struct {
	Type      string          `json:"type"`
	ViewerID  string          `json:"viewer_id"`
	RequestID string          `json:"request_id"`
	Data      json.RawMessage `json:"data"`
	Status    string          `json:"status"`
}

type recordedHostInput struct{ records chan []byte }

func (w *recordedHostInput) Write(data []byte) (int, error) {
	w.records <- append([]byte(nil), data...)
	return len(data), nil
}
func (w *recordedHostInput) Close() error { return nil }

// Exercise the real host/lease/signaling loop with synthetic records only. A
// blocked broadcaster must not postpone an existing viewer's key or revocation.
func TestHostBroadcastStopDoesNotBlockViewerInput(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	backend := newBlockedHostBroadcast()
	connected := make(chan *websocket.Conn, 1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/account/hands/host" || r.Header.Get("Authorization") != "Bearer test-publisher" {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		socket, err := websocket.Accept(w, r, nil)
		if err != nil {
			return
		}
		defer socket.CloseNow()
		connected <- socket
		<-ctx.Done()
	}))
	credential := filepath.Join(t.TempDir(), "credential")
	if err := os.WriteFile(credential, []byte("test-publisher"), 0600); err != nil {
		cancel()
		server.Close()
		t.Fatal(err)
	}
	input := &recordedHostInput{records: make(chan []byte, 32)}
	capture := &waymoteCapture{input: input, done: make(chan struct{})}
	hostDone := make(chan error, 1)
	go func() {
		hostDone <- serveWayland(ctx, hostConfig{Origin: server.URL, CredentialFile: credential,
			MachineID: "broadcast-test", Name: "Broadcast test", Width: 640, Height: 360,
			Frames: true, quiet: true, capture: capture, broadcast: backend})
	}()
	t.Cleanup(func() {
		cancel()
		backend.unblock()
		select {
		case <-hostDone:
		case <-time.After(3 * time.Second):
			t.Error("host did not finish broadcast cleanup")
		}
		server.Close()
	})
	var socket *websocket.Conn
	select {
	case socket = <-connected:
	case <-ctx.Done():
		t.Fatal("host did not connect")
	}
	replies := make(chan hostBroadcastReply, 32)
	readErrors := make(chan error, 1)
	go func() {
		for {
			_, data, err := socket.Read(ctx)
			var message hostBroadcastReply
			if err == nil {
				err = json.Unmarshal(data, &message)
			}
			if err != nil {
				readErrors <- err
				return
			}
			select {
			case replies <- message:
			case <-ctx.Done():
				return
			}
		}
	}()
	send := func(message remoteMessage) {
		t.Helper()
		data, err := json.Marshal(message)
		if err == nil {
			err = socket.Write(ctx, websocket.MessageText, data)
		}
		if err != nil {
			t.Fatal(err)
		}
	}
	receive := func(kind string) hostBroadcastReply {
		t.Helper()
		select {
		case message := <-replies:
			if message.Type != kind {
				t.Fatalf("got %q, want %q", message.Type, kind)
			}
			return message
		case err := <-readErrors:
			t.Fatal(err)
		case <-time.After(3 * time.Second):
			t.Fatalf("waiting for %s while broadcast stop is blocked", kind)
		}
		return hostBroadcastReply{}
	}
	record := func(kind byte) {
		t.Helper()
		select {
		case wire := <-input.records:
			if len(wire) != 16 || wire[1] != kind {
				t.Fatalf("unexpected input record %x, want kind %d", wire, kind)
			}
		case <-time.After(3 * time.Second):
			t.Fatal("input blocked behind broadcast stop")
		}
	}
	send(remoteMessage{Type: "ready", ConnectionID: "publisher"})
	receive("catalog")
	send(remoteMessage{Type: "viewer", ViewerID: "viewer", SurfaceID: "desktop"})
	send(remoteMessage{Type: "control", ViewerID: "viewer", Data: json.RawMessage(`{"type":"acquire"}`)})
	granted := receive("control")
	var lease controlMessage
	if err := json.Unmarshal(granted.Data, &lease); err != nil || lease.Type != "granted" || lease.Generation == "" {
		t.Fatalf("invalid lease grant: %s", granted.Data)
	}
	record(5) // acquire releases any previous native input
	send(remoteMessage{Type: "broadcast", ViewerID: "viewer", RequestID: "stop-request", SurfaceID: "desktop", Action: "stop"})
	awaitBroadcastSignal(t, backend.entered)
	key, down := uint16(4), true
	data, _ := json.Marshal(remoteInput{Kind: "key", Sequence: 1, Generation: lease.Generation, Key: &key, Down: &down})
	send(remoteMessage{Type: "input", ViewerID: "viewer", Data: data})
	data, _ = json.Marshal(controlMessage{Type: "release", Generation: lease.Generation})
	send(remoteMessage{Type: "control", ViewerID: "viewer", Data: data})
	record(4)
	record(5)
	revoked := receive("control")
	var control controlMessage
	if json.Unmarshal(revoked.Data, &control) != nil || control.Type != "revoked" {
		t.Fatalf("lease was not revoked during blocked stop: %s", revoked.Data)
	}
	// Released authority stays revoked while lifecycle work is pending.
	data, _ = json.Marshal(remoteInput{Kind: "key", Sequence: 2, Generation: lease.Generation, Key: &key, Down: &down})
	send(remoteMessage{Type: "input", ViewerID: "viewer", Data: data})
	if closed := receive("close_viewer"); closed.ViewerID != "viewer" {
		t.Fatal("released viewer was not rejected")
	}
	select {
	case wire := <-input.records:
		t.Fatalf("input accepted after release: %x", wire)
	case message := <-replies:
		t.Fatalf("premature broadcast result: %+v", message)
	default:
	}
	backend.unblock()
	result := receive("broadcast_result")
	if result.ViewerID != "viewer" || result.RequestID != "stop-request" || result.Status != "stopped" {
		t.Fatalf("incorrect stop acknowledgement: %+v", result)
	}
}

type blockedHostCleanupInput struct {
	entered chan struct{}
	release chan struct{}
	once    sync.Once
}

func (w *blockedHostCleanupInput) Write(data []byte) (int, error) {
	w.once.Do(func() { close(w.entered) })
	<-w.release
	return len(data), nil
}
func (w *blockedHostCleanupInput) Close() error { return nil }

// A host-local terminal signaling error must cancel queued side effects before
// deferred native input release (or peer teardown) has finished.
func TestHostBroadcastExitCancelsBeforeBlockedPeerCleanup(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	backend := newBlockedHostBroadcast()
	input := &blockedHostCleanupInput{entered: make(chan struct{}), release: make(chan struct{})}
	var releaseInput sync.Once
	unblockInput := func() { releaseInput.Do(func() { close(input.release) }) }
	connected := make(chan *websocket.Conn, 1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/account/hands/host" || r.Header.Get("Authorization") != "Bearer test-publisher" {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		socket, err := websocket.Accept(w, r, nil)
		if err != nil {
			return
		}
		defer socket.CloseNow()
		connected <- socket
		<-ctx.Done()
	}))
	credential := filepath.Join(t.TempDir(), "credential")
	if err := os.WriteFile(credential, []byte("test-publisher"), 0600); err != nil {
		cancel()
		server.Close()
		t.Fatal(err)
	}
	hostExited := make(chan struct{})
	var hostErr error
	go func() {
		defer close(hostExited)
		hostErr = serveWayland(ctx, hostConfig{Origin: server.URL, CredentialFile: credential,
			MachineID: "broadcast-exit-test", Name: "Broadcast exit test", Width: 640, Height: 360,
			Frames: true, quiet: true, capture: &waymoteCapture{input: input, done: make(chan struct{})}, broadcast: backend})
	}()
	t.Cleanup(func() {
		cancel()
		backend.unblock()
		unblockInput()
		select {
		case <-hostExited:
		case <-time.After(3 * time.Second):
			t.Error("host cleanup did not finish")
		}
		server.Close()
	})
	var socket *websocket.Conn
	select {
	case socket = <-connected:
	case <-ctx.Done():
		t.Fatal("host did not connect")
	}
	send := func(message remoteMessage) {
		t.Helper()
		data, err := json.Marshal(message)
		if err == nil {
			err = socket.Write(ctx, websocket.MessageText, data)
		}
		if err != nil {
			t.Fatal(err)
		}
	}
	send(remoteMessage{Type: "ready", ConnectionID: "publisher"})
	if _, _, err := socket.Read(ctx); err != nil { // catalog confirms initialization
		t.Fatal(err)
	}
	send(remoteMessage{Type: "broadcast", ViewerID: "viewer", RequestID: "stop", SurfaceID: "desktop", Action: "stop"})
	awaitBroadcastSignal(t, backend.entered)
	send(remoteMessage{Type: "broadcast", ViewerID: "viewer", RequestID: "queued-start", SurfaceID: "desktop", Action: "start", URL: "must-not-start"})
	// A second ready is a terminal protocol error handled inside the host loop;
	// the parent context remains live, exposing deferred-cancellation mistakes.
	send(remoteMessage{Type: "ready", ConnectionID: "duplicate"})
	awaitBroadcastSignal(t, input.entered)
	backend.unblock()
	select {
	case <-backend.cleaned:
	case <-backend.starts:
		t.Fatal("queued broadcast started after session exit during blocked cleanup")
	case <-time.After(3 * time.Second):
		t.Fatal("broadcast cancellation waited for blocked host cleanup")
	}
	select {
	case <-backend.starts:
		t.Fatal("queued start was not discarded")
	case <-hostExited:
		t.Fatal("host returned before native input cleanup finished")
	default:
	}
	unblockInput()
	awaitBroadcastSignal(t, hostExited)
	if hostErr == nil || hostErr.Error() != "invalid remote connection" {
		t.Fatalf("terminal host error was lost: %v", hostErr)
	}
}
