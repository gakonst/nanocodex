package main

import (
	"bytes"
	"context"
	"encoding/json"
	"image"
	"image/jpeg"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"syscall"
	"testing"
	"time"
)

func TestObservationHelperParity(t *testing.T) {
	b, e := os.ReadFile("../../bin/nanocodex/src/nanocodex2/observation_providers/helper.py")
	if e != nil || string(b) != observationHelper {
		t.Fatal("helper drift", e)
	}
}
func TestObservationContextValidation(t *testing.T) {
	for _, raw := range []string{`null`, `{}`, `{"app":"a","window":"b","path":"x"}`, `{"app":"a","window":"b\n"}`, `{"app":1,"window":"b"}`} {
		if _, e := (agentInput{Action: "observe", Context: json.RawMessage(raw)}).steps(""); e == nil {
			t.Fatal(raw)
		}
	}
	for _, a := range []string{"click", "release", "type"} {
		if _, e := (agentInput{Action: a, Context: json.RawMessage(`{"app":"a","window":"b"}`)}).steps(""); e == nil {
			t.Fatal(a)
		}
	}
	if _, e := (agentInput{Action: "observe", Context: json.RawMessage(`{"app":"a","window":"b"}`)}).steps(""); e != nil {
		t.Fatal(e)
	}
}
func TestObservationScopedSnapshot(t *testing.T) {
	p := filepath.Join(t.TempDir(), "snapshot")
	b, _ := json.Marshal(map[string]any{"schemaVersion": 1, "capturedAt": time.Now().UnixMilli() - 6000, "app": "app", "window": "main", "data": map[string]any{"label": "hello"}})
	if e := os.WriteFile(p, b, 0600); e != nil {
		t.Fatal(e)
	}
	r := observationRegistry{{id: "external:0", kind: "external", path: p}}
	for _, tt := range []struct {
		s    *observationContext
		code string
	}{{nil, "context_required"}, {&observationContext{"other", "main"}, "context_mismatch"}, {&observationContext{"app", "main"}, ""}} {
		v := r.collect(context.Background(), tt.s, 42)["providers"].([]map[string]any)[0]
		if tt.code != "" {
			if v["error"] != tt.code || v["data"] != nil {
				t.Fatal(v)
			}
		} else if v["status"] != "ok" || v["scope"] != "requested_context" || v["foreground_verified"] != false || v["freshness"] != "stale" {
			t.Fatal(v)
		}
	}
}
func TestObservationCancellationReapsChild(t *testing.T) {
	p := filepath.Join(t.TempDir(), "pid")
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	cmd := exec.CommandContext(ctx, "python3", "-I", "-c", "import os,sys,time; open(sys.argv[1],'w').write(str(os.getpid())); time.sleep(30)", p)
	done := make(chan map[string]any, 1)
	go func() { done <- runObservationChild(ctx, cmd, nil) }()
	deadline := time.Now().Add(3 * time.Second)
	pid := 0
	for time.Now().Before(deadline) {
		b, _ := os.ReadFile(p)
		pid, _ = strconv.Atoi(string(b))
		if pid > 0 {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}
	if pid == 0 {
		t.Fatal("helper did not start")
	}
	cancel()
	select {
	case v := <-done:
		if v["status"] != "timeout" {
			t.Fatal(v)
		}
	case <-time.After(time.Second):
		t.Fatal("cancellation stalled")
	}
	if syscall.Kill(pid, 0) == nil {
		t.Fatal("child alive")
	}
}
func TestObservationOutputBounded(t *testing.T) {
	ctx, c := context.WithTimeout(context.Background(), 2*time.Second)
	defer c()
	cmd := exec.CommandContext(ctx, "python3", "-I", "-c", "import sys,time; sys.stdout.write('x'*20000); sys.stdout.flush(); time.sleep(30)")
	v := runObservationChild(ctx, cmd, nil)
	if v["error"] != "invalid_provider_output" || ctx.Err() != nil {
		t.Fatal(v, ctx.Err())
	}
}
func TestObservationTimeoutPreservesScreenshot(t *testing.T) {
	dir := t.TempDir()
	var b bytes.Buffer
	jpeg.Encode(&b, image.NewRGBA(image.Rect(0, 0, 4, 2)), nil)
	frame := filepath.Join(dir, "frame.jpg")
	os.WriteFile(frame, b.Bytes(), 0600)
	os.WriteFile(filepath.Join(dir, "grim"), []byte("#!/bin/sh\nexec cat '"+frame+"'\n"), 0700)
	os.WriteFile(filepath.Join(dir, "python3"), []byte("#!/bin/sh\nexec sleep 30\n"), 0700)
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	start := time.Now()
	v := snapshotAgent(context.Background(), 4, 2, &observationContext{"a", "b"}, observationRegistry{{id: "external:0", kind: "external", path: "/unused"}})
	if v.Status != "ok" || v.JPEG == "" || v.Width != 4 || v.Height != 2 {
		t.Fatal(v)
	}
	if v.Observation["providers"].([]map[string]any)[0]["status"] != "timeout" || time.Since(start) > 2*time.Second {
		t.Fatal(v.Observation)
	}
}
func TestObservationLocalConfiguration(t *testing.T) {
	t.Setenv("DBUS_SESSION_BUS_ADDRESS", "ambient")
	t.Setenv("NANOCODEX_OBSERVATION_ATSPI_BUS", "")
	t.Setenv("NANOCODEX_OBSERVATION_SNAPSHOT_PATHS", `["/a","/b","/c","/d","/e"]`)
	r := localObservationRegistry()
	if len(r) != 5 || r[0].bus != "" {
		t.Fatal(r)
	}
}
