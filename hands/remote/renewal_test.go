package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
)

// Exercise real authenticated signaling and host timers without a compositor.
func TestPublisherRenewal(t *testing.T) {
	for _, tc := range []struct {
		name        string
		status      int
		acknowledge bool
	}{
		{"network", 0, true}, {"timeout", 408, true}, {"throttled", 429, true},
		{"server", 503, true}, {"unauthorized", 401, false}, {"forbidden", 403, false},
		{"http_success_without_renewed", 200, false}, {"continuous_failure", 502, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			ctx, cancel := context.WithTimeout(context.Background(), 32*time.Second)
			defer cancel()
			var attempts, active atomic.Int32
			var overlap atomic.Bool
			renew := make(chan struct{}, 1)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.Header.Get("Authorization") != "Bearer test-publisher" {
					http.Error(w, "unauthorized", 401)
					return
				}
				if strings.HasSuffix(r.URL.Path, "/renew") {
					if active.Add(1) != 1 {
						overlap.Store(true)
					}
					defer active.Add(-1)
					n := attempts.Add(1)
					var body map[string]string
					if json.NewDecoder(r.Body).Decode(&body) != nil || body["connection_id"] != "publisher" {
						t.Error("missing connection binding")
					}
					// Keep each request pending across multiple host ticks.
					time.Sleep(1500 * time.Millisecond)
					if tc.acknowledge && n > 1 {
						w.WriteHeader(200)
						select {
						case renew <- struct{}{}:
						default:
						}
						return
					}
					if tc.status == 0 {
						conn, _, err := w.(http.Hijacker).Hijack()
						if err == nil {
							conn.Close()
						}
						return
					}
					w.WriteHeader(tc.status)
					return
				}
				socket, err := websocket.Accept(w, r, nil)
				if err != nil {
					return
				}
				defer socket.CloseNow()
				_ = socket.Write(ctx, websocket.MessageText, []byte(`{"type":"ready","connection_id":"publisher"}`))
				if tc.acknowledge {
					select {
					case <-renew:
						_ = socket.Write(ctx, websocket.MessageText, []byte(`{"type":"renewed"}`))
						// Cross the original 25s deadline, proving authenticated renewal extends it.
						select {
						case <-time.After(14 * time.Second):
							_ = socket.Close(websocket.StatusPolicyViolation, "Host replaced")
						case <-ctx.Done():
						}
					case <-ctx.Done():
					}
				} else {
					<-ctx.Done()
				}
			}))
			defer server.Close()
			defer cancel()
			credential := filepath.Join(t.TempDir(), "credential")
			if err := os.WriteFile(credential, []byte("test-publisher"), 0600); err != nil {
				t.Fatal(err)
			}
			capture := &waymoteCapture{input: discardedHostInput{}, done: make(chan struct{})}
			started := time.Now()
			err := serveWayland(ctx, hostConfig{Origin: server.URL, CredentialFile: credential, MachineID: "renew-test", Name: "renew", Width: 640, Height: 360, Frames: true, quiet: true, capture: capture})
			elapsed := time.Since(started)
			if overlap.Load() {
				t.Fatal("concurrent renewal requests")
			}
			if tc.acknowledge {
				if err != errRemoteHostReplaced || attempts.Load() < 2 || elapsed < 25*time.Second {
					t.Fatalf("renewal failed: attempts=%d elapsed=%s err=%v", attempts.Load(), elapsed, err)
				}
			} else if tc.status == 401 || tc.status == 403 {
				if err == nil || err.Error() != fmt.Sprintf("remote service refused request (%d)", tc.status) || attempts.Load() != 1 || elapsed >= 20*time.Second {
					t.Fatalf("terminal refusal: attempts=%d elapsed=%s err=%v", attempts.Load(), elapsed, err)
				}
			} else {
				if err == nil || err.Error() != "remote authorization expired" || elapsed < 25*time.Second || elapsed > 28*time.Second {
					t.Fatalf("deadline changed: elapsed=%s err=%v", elapsed, err)
				}
				if tc.status == 502 && attempts.Load() < 2 {
					t.Fatal("transient failure was not retried")
				}
			}
		})
	}
}
