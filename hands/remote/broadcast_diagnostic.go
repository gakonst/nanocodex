package main

import (
	"strings"
	"sync"
)

// Retain at most one bounded fragment, and only publish allowlisted categories.
// Redacting arbitrary encoder output is insufficient: it can contain bare keys.
type broadcastDiagnostic struct {
	mu       sync.Mutex
	fragment []byte
	detected string
}

func (d *broadcastDiagnostic) Write(p []byte) (int, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	for _, b := range p {
		if b == '\n' || b == '\r' || len(d.fragment) >= 4096 {
			d.classify()
			d.fragment = d.fragment[:0]
		}
		if b != '\n' && b != '\r' {
			d.fragment = append(d.fragment, b)
		}
	}
	return len(p), nil
}

func (d *broadcastDiagnostic) classify() {
	line := strings.ToLower(string(d.fragment))
	category := ""
	for _, code := range []string{"capture_failed", "authentication_failed", "connection_failed", "encoder_failed", "broadcast_failed"} {
		if strings.TrimSpace(line) == "nanocodex_broadcast_error="+code {
			category = code
		}
	}
	if category == "" {
		switch {
		case containsBroadcastDiagnostic(line, "unauthorized", "authentication failed", "authorization failed", "403 forbidden", "401 unauthorized", "netstream.publish.denied", "netstream.publish.badname"):
			category = "authentication_failed"
		case containsBroadcastDiagnostic(line, "controlconnectionclosed", "failed to connect to wayland", "wayland connection failed", "failed to connect to pipewire", "failed to create screencopy", "no such pulse", "cannot open display", "failed to open pulse", "failed to connect context"):
			category = "capture_failed"
		case containsBroadcastDiagnostic(line, "unknown encoder", "error while opening encoder", "error initializing output stream", "cannot load libcuda", "no capable devices found"):
			category = "encoder_failed"
		case containsBroadcastDiagnostic(line, "connection refused", "connection timed out", "network is unreachable", "no route to host", "failed to resolve hostname", "temporary failure in name resolution", "connection reset by peer", "broken pipe", "certificate verify failed"):
			category = "connection_failed"
		}
	}
	// Preserve a specific root cause over cascading transport/generic failures.
	if category != "" && (d.detected == "" || d.detected == "broadcast_failed" || d.detected == "connection_failed") {
		d.detected = category
	}
}

func containsBroadcastDiagnostic(line string, patterns ...string) bool {
	for _, pattern := range patterns {
		if strings.Contains(line, pattern) {
			return true
		}
	}
	return false
}

func (d *broadcastDiagnostic) category() string {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.classify()
	return broadcastSafeCategory(d.detected)
}

func broadcastSafeCategory(category string) string {
	switch category {
	case "capture_failed", "authentication_failed", "connection_failed", "encoder_failed", "progress_timeout":
		return category
	default:
		return "broadcast_failed"
	}
}

func broadcastWireError(category string) string {
	switch category {
	case "authentication_failed", "progress_timeout":
		return "connection_failed"
	default:
		return broadcastSafeCategory(category)
	}
}
