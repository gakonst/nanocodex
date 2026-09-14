package main

import (
	"context"
	"errors"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"time"
)

// The VM owner runs this process through its trusted guest command channel.
// Its private credential file is atomically replaced on lease rotation and
// emptied on shutdown. The compositor survives signaling/credential reconnects.
func serveDesktop(parent context.Context, config hostConfig, workspace string) error {
	return serveDesktopSession(parent, config, workspace, "/etc/nanocodex-desktop", true)
}

// A server desktop runs as the SSH user without a nested VM or a privileged
// compositor. Its machine credential is independent of the SSH login key.
func serveDesktopSession(parent context.Context, config hostConfig, workspace, desktopConfig string, vm bool) error {
	if !filepath.IsAbs(workspace) {
		return errors.New("desktop workspace must be absolute")
	}
	if !filepath.IsAbs(desktopConfig) {
		return errors.New("desktop configuration must be absolute")
	}
	service, err := newRemoteService(config.Origin, config.CredentialFile)
	if err != nil {
		return err
	}
	prefix := "/v1/hand-hosts/"
	runtimeParent := os.TempDir()
	if vm {
		prefix = "/v1/vm-host-attachments/"
		runtimeParent = "/run"
	}
	if !strings.HasPrefix(service.base.Path, prefix) {
		return errors.New("desktop requires its scoped publisher endpoint")
	}
	ctx, cancel := context.WithCancel(parent)
	defer cancel()
	runtime, err := os.MkdirTemp(runtimeParent, "nanocodex-desktop-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(runtime)
	// This executable owns its process environment and all desktop children.
	for key, value := range map[string]string{
		"XDG_RUNTIME_DIR": runtime, "WAYLAND_DISPLAY": "wayland-0",
		"WLR_BACKENDS": "headless", "WLR_RENDERER": "pixman", "WLR_HEADLESS_OUTPUTS": "1",
		"XDG_SESSION_TYPE": "wayland", "XDG_CURRENT_DESKTOP": "labwc",
	} {
		if err := os.Setenv(key, value); err != nil {
			return err
		}
	}
	compositor := exec.CommandContext(ctx, "labwc", "--config-dir", desktopConfig)
	compositor.Dir = workspace
	compositor.Stdout, compositor.Stderr = io.Discard, io.Discard
	compositor.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	compositor.Cancel = func() error {
		err := syscall.Kill(-compositor.Process.Pid, syscall.SIGKILL)
		if errors.Is(err, syscall.ESRCH) {
			return os.ErrProcessDone
		}
		return err
	}
	compositor.WaitDelay = time.Second
	if err := compositor.Start(); err != nil {
		return errors.New("cannot start desktop compositor")
	}
	done := make(chan struct{})
	go func() { _ = compositor.Wait(); cancel(); close(done) }()
	defer func() { cancel(); <-done }()
	readyPath := config.CredentialFile + ".ready"
	_ = os.Remove(readyPath)
	defer os.Remove(readyPath)
	deadline := time.NewTimer(20 * time.Second)
	defer deadline.Stop()
	tick := time.NewTicker(100 * time.Millisecond)
	defer tick.Stop()
	for {
		if info, err := os.Stat(filepath.Join(runtime, "wayland-0")); err == nil && info.Mode()&os.ModeSocket != 0 {
			break
		}
		select {
		case <-ctx.Done():
			return errors.New("desktop compositor stopped")
		case <-deadline.C:
			return errors.New("desktop compositor did not become ready")
		case <-tick.C:
		}
	}
	config.quiet = true
	// Compositor readiness is independent of the remote signaling service.
	// A relay outage must not prevent the VM's shell attachment from starting.
	if err := os.WriteFile(readyPath, []byte("ready\n"), 0600); err != nil {
		return err
	}
	statusPath := config.CredentialFile + ".status"
	defer os.Remove(statusPath)
	config.published = func() { _ = os.WriteFile(statusPath, []byte("published\n"), 0600) }
	// Capture and input belong to the desktop, not to a signaling connection.
	// Recreating Waymote on reconnect can lose text while its replacement input
	// method activates. Each host session still drops its control lease and
	// releases held input before a new authenticated connection can take over.
	defer func() {
		if config.capture != nil {
			config.capture.close()
		}
	}()
	for ctx.Err() == nil {
		if config.capture != nil {
			select {
			case <-config.capture.done:
				config.capture.close()
				config.capture = nil
			default:
			}
		}
		if config.capture == nil {
			capture, err := startWaymoteWithDiagnostics(ctx, config.Waymote, io.Discard)
			if err != nil {
				return err
			}
			config.capture = capture
		}
		hostCtx, stop := context.WithCancel(ctx)
		finished := make(chan error, 1)
		go func() { finished <- serveWayland(hostCtx, config) }()
		changed := false
		for !changed {
			select {
			case <-ctx.Done():
				stop()
				<-finished
				return ctx.Err()
			case err := <-finished:
				if errors.Is(err, errRemoteHostReplaced) {
					stop()
					return err
				}
				if err != nil {
					_ = os.WriteFile(statusPath, []byte(err.Error()+"\n"), 0600)
				}
				changed = true
			case <-tick.C:
				next, err := newRemoteService(config.Origin, config.CredentialFile)
				if err != nil {
					stop()
					<-finished
					return nil
				} // Cleared/revoked by owner.
				if next.token != service.token {
					service = next
					stop()
					if err := <-finished; errors.Is(err, errRemoteHostReplaced) {
						return err
					}
					changed = true
				}
			}
		}
		stop()
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(time.Second):
		}
	}
	return ctx.Err()
}
