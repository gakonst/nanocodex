package main

import (
	"bytes"
	"context"
	_ "embed"
	"encoding/json"
	"errors"
	"flag"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"time"
	"unicode"
	"unicode/utf8"
)

// Exact copy of bin/nanocodex/src/nanocodex2/observation_providers/helper.py.
// TestObservationHelperParity prevents semantic drift.
//
//go:embed observation_helper.py
var observationHelper string

const observationDeadline = 600 * time.Millisecond

var observationChildren = make(chan struct{}, 8)

type observationContext struct {
	App    string `json:"app"`
	Window string `json:"window"`
}

func parseObservationContext(raw json.RawMessage) (*observationContext, error) {
	if len(raw) == 0 {
		return nil, nil
	}
	var value *observationContext
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.DisallowUnknownFields()
	if !utf8.Valid(raw) || decoder.Decode(&value) != nil || value == nil {
		return nil, errors.New("invalid observation context")
	}
	for _, s := range []string{value.App, value.Window} {
		if len(s) == 0 || len(s) > 512 || strings.ContainsFunc(s, unicode.IsControl) {
			return nil, errors.New("invalid observation context")
		}
	}
	return value, nil
}

type observationProvider struct{ id, kind, path, bus string }
type observationRegistry []observationProvider

func localObservationRegistry() observationRegistry {
	bus := os.Getenv("NANOCODEX_OBSERVATION_ATSPI_BUS")
	if len(bus) > 4096 {
		bus = ""
	}
	providers := observationRegistry{{id: "atspi", kind: "atspi", bus: bus}}
	config := os.Getenv("NANOCODEX_OBSERVATION_SNAPSHOT_PATHS")
	var paths []string
	if len(config) <= 16384 && json.Unmarshal([]byte(config), &paths) == nil {
		for i, path := range paths[:min(4, len(paths))] {
			if filepath.IsAbs(path) {
				providers = append(providers, observationProvider{id: fmtExternalID(i), kind: "external", path: path})
			}
		}
	}
	return providers
}
func fmtExternalID(i int) string { return "external:" + string(rune('0'+i)) }
func observationOutcome(status, code string) map[string]any {
	return map[string]any{"status": status, "error": code}
}
func (provider observationProvider) run(ctx context.Context, selector *observationContext) map[string]any {
	if provider.kind == "atspi" {
		if runtime.GOOS != "linux" {
			return observationOutcome("unavailable", "unsupported_platform")
		}
		if provider.bus == "" {
			return observationOutcome("unavailable", "session_bus_unavailable")
		}
	}
	if provider.kind == "external" && selector == nil {
		return observationOutcome("unavailable", "context_required")
	}
	select {
	case observationChildren <- struct{}{}:
		defer func() { <-observationChildren }()
	default:
		return observationOutcome("unavailable", "provider_busy")
	}
	command := exec.CommandContext(ctx, "python3", "-I", "-X", "utf8", "-c", observationHelper, provider.kind)
	if provider.kind == "external" {
		command.Args = append(command.Args, provider.path)
	}
	for _, entry := range os.Environ() {
		if !strings.HasPrefix(entry, "DBUS_SESSION_BUS_ADDRESS=") && !strings.HasPrefix(entry, "AT_SPI_BUS_ADDRESS=") {
			command.Env = append(command.Env, entry)
		}
	}
	if provider.kind == "atspi" {
		command.Env = append(command.Env, "DBUS_SESSION_BUS_ADDRESS="+provider.bus)
	}
	return runObservationChild(ctx, command, selector)
}
func runObservationChild(ctx context.Context, command *exec.Cmd, selector *observationContext) map[string]any {
	input, _ := json.Marshal(map[string]any{"context": selector})
	command.Stdin = bytes.NewReader(input)
	stdout, err := command.StdoutPipe()
	if err != nil {
		return observationOutcome("error", "invalid_provider_output")
	}
	if command.Start() != nil {
		return observationOutcome("unavailable", "python_unavailable")
	}
	data, readErr := io.ReadAll(io.LimitReader(stdout, 12289))
	if readErr != nil || len(data) > 12288 {
		_ = command.Process.Kill()
	}
	waitErr := command.Wait() // Always reap, including timeout, cancellation, and overflow.
	if ctx.Err() != nil {
		return observationOutcome("timeout", "deadline_exceeded")
	}
	var result map[string]any
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	if readErr != nil || waitErr != nil || len(data) > 12288 || decoder.Decode(&result) != nil || result == nil {
		return observationOutcome("error", "invalid_provider_output")
	}
	var extra any
	if decoder.Decode(&extra) != io.EOF {
		return observationOutcome("error", "invalid_provider_output")
	}
	return result
}
func normalizeObservation(id string, raw map[string]any) map[string]any {
	now := time.Now().UnixMilli()
	status, _ := raw["status"].(string)
	switch status {
	case "ok", "partial", "unavailable", "error", "timeout":
	default:
		status = "error"
	}
	result := map[string]any{"id": id, "status": status, "capturedAt": now, "freshness": "unknown"}
	timestamp := int64(0)
	if number, ok := raw["capturedAt"].(json.Number); ok {
		timestamp, _ = number.Int64()
	}
	valid := timestamp > 0 && timestamp <= now+1000
	if valid {
		age := max(int64(0), now-timestamp)
		result["capturedAt"] = timestamp
		result["ageMs"] = age
		result["freshness"] = "fresh"
		if age > 5000 {
			result["freshness"] = "stale"
		}
	}
	if code, ok := raw["error"].(string); ok && len(code) <= 80 && !strings.ContainsFunc(code, func(r rune) bool { return (r < 'a' || r > 'z') && r != '_' }) {
		result["error"] = code
	}
	if status == "ok" || status == "partial" {
		data, ok := raw["data"].(map[string]any)
		var encoded bytes.Buffer
		encoder := json.NewEncoder(&encoded)
		encoder.SetEscapeHTML(false)
		err := encoder.Encode(data)
		if valid && ok && err == nil && encoded.Len()-1 <= 8192 {
			result["data"] = data
		} else {
			result["status"] = "error"
			result["error"] = "invalid_provider_output"
		}
	}
	return result
}
func (registry observationRegistry) collect(parent context.Context, selector *observationContext, capturedAt int64) map[string]any {
	results := make([]map[string]any, len(registry))
	var group sync.WaitGroup
	for i, provider := range registry {
		group.Add(1)
		go func() {
			defer group.Done()
			ctx, cancel := context.WithTimeout(parent, observationDeadline)
			defer cancel()
			result := normalizeObservation(provider.id, provider.run(ctx, selector))
			successful := result["status"] == "ok" || result["status"] == "partial"
			result["scope"] = "none"
			if selector != nil {
				result["scope"] = "requested_context"
			} else if successful {
				result["scope"] = "active_window"
			}
			result["foreground_verified"] = successful && selector == nil && provider.kind == "atspi"
			results[i] = result
		}()
	}
	group.Wait()
	return map[string]any{"schemaVersion": 1, "capturedAt": capturedAt, "providers": results}
}

// Called only by agent completion, never by viewer frame capture.
func snapshotAgent(parent context.Context, width, height int, selector *observationContext, registry observationRegistry) agentResult {
	capturedAt := time.Now().UnixMilli()
	observations := make(chan map[string]any, 1)
	go func() { observations <- registry.collect(parent, selector, capturedAt) }()
	result := snapshotDesktop(parent, width, height)
	result.Observation = <-observations
	return result
}

// observeLocal is a read-only diagnostic using the exact agent observation path.
// Provider configuration remains exclusively local environment configuration.
func observeLocal(ctx context.Context, args []string, output io.Writer) error {
	flags := flag.NewFlagSet("observe-local", flag.ContinueOnError)
	width := flags.Int("width", 1600, "desktop output width")
	height := flags.Int("height", 900, "desktop output height")
	selectorJSON := flags.String("context", "", "optional exact JSON app/window selector")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if flags.NArg() != 0 || *width < 1 || *height < 1 || *width > 32768 || *height > 32768 {
		return errors.New("invalid observation dimensions or arguments")
	}
	selector, err := parseObservationContext(json.RawMessage(*selectorJSON))
	if err != nil {
		return err
	}
	result := snapshotAgent(ctx, *width, *height, selector, localObservationRegistry())
	return json.NewEncoder(output).Encode(result)
}
