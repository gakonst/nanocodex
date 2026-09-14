package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
	"time"

	"github.com/danielpaulus/go-ios/ios"
	"howett.net/plist"
)

func listPhones() error {
	done := make(chan error, 1)
	go func() { done <- writePhoneList() }()
	select {
	case err := <-done:
		return err
	case <-time.After(10 * time.Second):
		return errors.New("paired-device discovery timed out; reconnect and trust the iPhone")
	}
}

func writePhoneList() error {
	devices, err := ios.ListDevices()
	if err != nil {
		return err
	}
	phones := []map[string]string{}
	seen := map[string]bool{}
	for _, device := range devices.DeviceList {
		if len(phones) >= 64 {
			break
		}
		id := device.Properties.SerialNumber
		if seen[id] {
			continue
		}
		seen[id] = true
		values, err := ios.GetValues(device)
		if err != nil {
			continue
		}
		phones = append(phones, map[string]string{"id": id, "name": values.Value.DeviceName})
	}
	return json.NewEncoder(os.Stdout).Encode(phones)
}

// A user-selected, already signed developer runner is executable code. Keep the
// source artifact unchanged, fix its build-root references in a private copy,
// and run only WebDriverAgent's long-lived testRunner entrypoint.
func preparePhoneRunner(path, destination string) error {
	if filepath.Ext(path) != ".xctestrun" {
		return errors.New("choose a built WebDriverAgent .xctestrun file")
	}
	info, err := os.Stat(path)
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() || info.Size() > 2<<20 {
		return errors.New("invalid runner configuration")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	var config map[string]any
	if _, err = plist.Unmarshal(data, &config); err != nil {
		return err
	}
	// Xcode's standalone build-for-testing format. Reject unrelated or combined
	// test plans rather than silently launching additional test targets.
	target, ok := config["WebDriverAgentRunner"].(map[string]any)
	if !ok || len(config) != 2 || config["__xctestrun_metadata__"] == nil || target["ProductModuleName"] != "WebDriverAgentRunner" {
		return errors.New("build the WebDriverAgentRunner scheme with build-for-testing first")
	}
	environment, ok := target["EnvironmentVariables"].(map[string]any)
	if !ok {
		environment = map[string]any{}
		target["EnvironmentVariables"] = environment
	}
	environment["USE_IP"] = "127.0.0.1"
	environment["USE_PORT"] = "18100"
	environment["MJPEG_SERVER_PORT"] = "19100"
	absolute, err := filepath.Abs(path)
	if err != nil {
		return err
	}
	var resolve func(any) any
	resolve = func(value any) any {
		switch value := value.(type) {
		case string:
			return strings.ReplaceAll(value, "__TESTROOT__", filepath.Dir(absolute))
		case map[string]any:
			for key, entry := range value {
				value[key] = resolve(entry)
			}
			return value
		case []any:
			for index, entry := range value {
				value[index] = resolve(entry)
			}
			return value
		default:
			return value
		}
	}
	data, err = plist.Marshal(resolve(config), plist.XMLFormat)
	if err != nil {
		return err
	}
	return os.WriteFile(destination, data, 0600)
}

func phoneBridge(parent context.Context, udid, runner string) error {
	if runtime.GOOS != "darwin" {
		return errors.New("the developer phone bridge requires macOS and Xcode")
	}
	if udid == "" {
		return errors.New("select a paired iPhone")
	}
	device, err := ios.GetDevice(udid)
	if err != nil {
		return err
	}
	if device.Properties.SerialNumber != udid {
		return errors.New("paired device identity mismatch")
	}
	directory, err := os.MkdirTemp("", "nanocodex-phone-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(directory)
	prepared := filepath.Join(directory, "WebDriverAgent.xctestrun")
	if err = preparePhoneRunner(runner, prepared); err != nil {
		return err
	}
	// Keep the tunnel alive briefly on owner exit so WDA can stop its device
	// server cleanly before we terminate the local runner process group.
	ctx, cancel := context.WithCancel(context.WithoutCancel(parent))
	defer cancel()
	tunnelReady := make(chan struct{})
	tunnelDone := make(chan error, 1)
	go func() {
		tunnelDone <- phoneTunnel(ctx, device.DeviceID, []int{18100, 19100}, func() { close(tunnelReady) })
	}()
	select {
	case err = <-tunnelDone:
		return err
	case <-parent.Done():
		return parent.Err()
	case <-tunnelReady:
	}
	defer func() { cancel(); <-tunnelDone }()
	command := exec.CommandContext(ctx, "/usr/bin/xcodebuild", "test-without-building", "-xctestrun", prepared,
		"-destination", "id="+udid, "-only-testing:WebDriverAgentRunner/UITestingUITests/testRunner")
	command.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	command.Cancel = func() error { return syscall.Kill(-command.Process.Pid, syscall.SIGKILL) }
	command.WaitDelay = 3 * time.Second
	command.Env = []string{"PATH=/usr/bin:/bin:/usr/sbin:/sbin"}
	for _, key := range []string{"HOME", "USER", "TMPDIR", "LANG", "DEVELOPER_DIR"} {
		if value := os.Getenv(key); value != "" {
			command.Env = append(command.Env, key+"="+value)
		}
	}
	command.Stdout = os.Stderr
	command.Stderr = os.Stderr
	if err = command.Start(); err != nil {
		return err
	}
	done := make(chan error, 1)
	go func() { done <- command.Wait() }()
	defer func() { cancel(); <-done }()
	client := &http.Client{Timeout: time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	defer func() {
		response, err := client.Get("http://127.0.0.1:18100/wda/shutdown")
		if err == nil {
			_, _ = io.Copy(io.Discard, io.LimitReader(response.Body, 4096))
			_ = response.Body.Close()
		}
		select {
		case err := <-done:
			done <- err
		case <-time.After(time.Second):
		}
	}()
	deadline := time.NewTimer(90 * time.Second)
	defer deadline.Stop()
	ticker := time.NewTicker(500 * time.Millisecond)
	defer ticker.Stop()
	for {
		select {
		case <-parent.Done():
			return parent.Err()
		case err = <-tunnelDone:
			tunnelDone <- err
			return fmt.Errorf("iPhone tunnel closed: %w", err)
		case <-deadline.C:
			return errors.New("iPhone runner did not start; check pairing, Developer Mode, signing, and unlock the phone")
		case err = <-done:
			done <- err // leave completion for the cleanup owner
			return fmt.Errorf("iPhone runner exited: %v", err)
		case <-ticker.C:
			request, _ := http.NewRequestWithContext(ctx, "GET", "http://127.0.0.1:18100/status", nil)
			response, err := client.Do(request)
			if err != nil {
				continue
			}
			data, _ := io.ReadAll(io.LimitReader(response.Body, 64<<10))
			_ = response.Body.Close()
			var status struct {
				Value struct {
					Ready bool `json:"ready"`
				} `json:"value"`
			}
			if response.StatusCode != 200 || json.Unmarshal(data, &status) != nil || !status.Value.Ready {
				continue
			}
			if err = json.NewEncoder(os.Stdout).Encode(map[string]string{"type": "ready", "deviceID": udid}); err != nil {
				return err
			}
			select {
			case <-parent.Done():
				return parent.Err()
			case err = <-tunnelDone:
				tunnelDone <- err
				return fmt.Errorf("iPhone tunnel closed: %w", err)
			case err = <-done:
				done <- err
				return fmt.Errorf("iPhone runner exited: %v", err)
			}
		}
	}
}
