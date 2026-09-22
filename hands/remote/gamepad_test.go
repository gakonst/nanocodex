package main

import (
	"encoding/json"
	"errors"
	"io"
	"math"
	"reflect"
	"strings"
	"sync"
	"testing"
	"time"
)

type fakeGamepad struct {
	mu     sync.Mutex
	states []gamepadState
	closes int
	err    error
}

func (d *fakeGamepad) writeState(s gamepadState) error {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.states = append(d.states, s)
	return d.err
}
func (d *fakeGamepad) close() error { d.mu.Lock(); defer d.mu.Unlock(); d.closes++; return nil }
func (d *fakeGamepad) snapshot() ([]gamepadState, int) {
	d.mu.Lock()
	defer d.mu.Unlock()
	return append([]gamepadState(nil), d.states...), d.closes
}

func TestGamepadPayload(t *testing.T) {
	snapshot := `{"leftX":-1,"leftY":1,"rightX":0,"rightY":0,"leftTrigger":0,"rightTrigger":1,"buttons":["a","dpadUp"]}`
	wrap := func(s string) string {
		return `{"kind":"gamepad","sequence":1,"generation":"lease","gamepad":` + s + `}`
	}
	if _, err := decodeInput([]byte(wrap(snapshot))); err != nil {
		t.Fatal(err)
	}
	for _, s := range []string{`null`, `{}`, strings.Replace(snapshot, `"leftX":-1`, `"leftX":0,"leftX":-1`, 1), strings.Replace(snapshot, `"leftX":-1`, `"LeftX":-1`, 1), strings.Replace(snapshot, `"leftX":-1`, `"LeftX":0,"leftX":-1`, 1), strings.Replace(snapshot, `"leftX":-1,`, "", 1), strings.Replace(snapshot, `"leftX":-1`, `"leftX":null`, 1), strings.Replace(snapshot, `"leftX":-1`, `"leftX":-1.01`, 1), strings.Replace(snapshot, `"leftTrigger":0`, `"leftTrigger":-0.1`, 1), strings.Replace(snapshot, `["a","dpadUp"]`, `["a","a"]`, 1), strings.Replace(snapshot, `["a","dpadUp"]`, `["bogus"]`, 1), strings.Replace(snapshot, `["a","dpadUp"]`, `null`, 1), strings.Replace(snapshot, `"leftX":-1`, `"unknown":0,"leftX":-1`, 1)} {
		if _, err := decodeInput([]byte(wrap(s))); err == nil {
			t.Errorf("accepted %s", s)
		}
	}
	for _, payload := range []string{strings.Replace(wrap(snapshot), `"kind":"gamepad"`, `"kind":"releaseAll"`, 1), strings.Replace(wrap(snapshot), `"sequence":1`, `"x":0,"sequence":1`, 1), wrap(snapshot) + ` {}`, strings.Repeat(" ", 8193)} {
		if _, err := decodeInput([]byte(payload)); err == nil {
			t.Error("accepted invalid envelope")
		}
	}
	for _, v := range []float64{math.NaN(), math.Inf(1), -2, 2} {
		if (gamepadState{LeftX: v}).validate() == nil {
			t.Fatal("invalid axis")
		}
	}
	var roundtrip gamepadState
	encoded, _ := json.Marshal(gamepadState{Buttons: []string{}})
	if err := json.Unmarshal(encoded, &roundtrip); err != nil {
		t.Fatal(err)
	}
}
func TestGamepadReleaseGenerationAndClose(t *testing.T) {
	d := &fakeGamepad{}
	g := newGamepadController(d)
	held := gamepadState{LeftX: 1, RightTrigger: 1, Buttons: []string{"a"}}
	if err := g.apply(held); err != nil {
		t.Fatal(err)
	}
	old := g.generation
	if err := g.apply(held); err != nil {
		t.Fatal(err)
	}
	g.expire(old)
	states, _ := d.snapshot()
	if len(states) != 2 {
		t.Fatal("old watchdog released newer state")
	}
	if err := g.release(); err != nil {
		t.Fatal(err)
	}
	g.expire(old + 1)
	states, _ = d.snapshot()
	if len(states) != 3 || !reflect.DeepEqual(states[2], gamepadState{}) {
		t.Fatalf("release frames: %#v", states)
	}
	g.close()
	g.close()
	states, closes := d.snapshot()
	if len(states) != 4 || closes != 1 || !reflect.DeepEqual(states[3], gamepadState{}) || g.available() {
		t.Fatal("close did not neutralize exactly once")
	}
	if g.apply(held) == nil {
		t.Fatal("accepted closed input")
	}
}
func TestGamepadWatchdog(t *testing.T) {
	if gamepadIdleTimeout != 500*time.Millisecond {
		t.Fatal("watchdog contract")
	}
	d := &fakeGamepad{}
	g := newGamepadController(d)
	defer g.close()
	if err := g.apply(gamepadState{LeftX: 1}); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		states, _ := d.snapshot()
		if len(states) >= 2 {
			if !reflect.DeepEqual(states[1], gamepadState{}) {
				t.Fatal("not neutral")
			}
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatal("watchdog did not release")
}
func TestGamepadOptInAndFailure(t *testing.T) {
	calls := 0
	d := &fakeGamepad{}
	open := func() (gamepadDevice, error) { calls++; return d, nil }
	for _, value := range []string{"", "0", "true"} {
		t.Setenv("NANOCODEX_VIRTUAL_GAMEPAD", value)
		g, err := configuredGamepad(open)
		if err != nil || g.available() || calls != 0 {
			t.Fatal("opened without explicit opt-in")
		}
	}
	t.Setenv("NANOCODEX_VIRTUAL_GAMEPAD", "1")
	g, err := configuredGamepad(func() (gamepadDevice, error) { return nil, errors.New("denied") })
	if err == nil || g.available() {
		t.Fatal("advertised failed device")
	}
	g, err = configuredGamepad(open)
	if err != nil || !g.available() || calls != 1 {
		t.Fatal("missing capability")
	}
	d.err = errors.New("write failed")
	if g.apply(gamepadState{}) == nil || g.available() {
		t.Fatal("failed device still available")
	}
	_, closes := d.snapshot()
	if closes != 1 {
		t.Fatal("failed device not destroyed")
	}
}

type gamepadDiscardPipe struct{}

func (gamepadDiscardPipe) Write(p []byte) (int, error) { return len(p), nil }
func (gamepadDiscardPipe) Close() error                { return nil }
func TestGamepadCaptureReleasePaths(t *testing.T) {
	for _, path := range []string{"releaseAll", "disconnect", "close"} {
		t.Run(path, func(t *testing.T) {
			d := &fakeGamepad{}
			g := newGamepadController(d)
			done := make(chan struct{})
			close(done)
			capture := &waymoteCapture{gamepad: g, input: gamepadDiscardPipe{}, video: io.NopCloser(strings.NewReader("")), done: done, cancel: func() {}}
			if err := capture.apply(remoteInput{Kind: "gamepad", Sequence: 1, Generation: "lease", Gamepad: &gamepadState{LeftX: 1, Buttons: []string{"a"}}}); err != nil {
				t.Fatal(err)
			}
			switch path {
			case "releaseAll":
				if err := capture.apply(remoteInput{Kind: "releaseAll", Sequence: 2, Generation: "lease"}); err != nil {
					t.Fatal(err)
				}
			case "disconnect":
				if err := capture.releaseAll(); err != nil {
					t.Fatal(err)
				}
			case "close":
				capture.close()
			}
			states, _ := d.snapshot()
			if len(states) != 2 || !reflect.DeepEqual(states[1], gamepadState{}) {
				t.Fatalf("%s failed neutral: %#v", path, states)
			}
			g.close()
		})
	}
}
