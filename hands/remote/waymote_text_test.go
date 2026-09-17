package main

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestWaylandTextBackends(t *testing.T) {
	for _, tc := range []struct {
		name, probe, typing          string
		display, x11, wtype, missing bool
		wantBackend                  string
		wantErr                      bool
	}{
		{name: "focused X", probe: "printf '1234\\n'", display: true, x11: true, wtype: true, wantBackend: "x11"},
		{name: "X without wtype opt in", probe: "printf '1234\\n'", display: true, x11: true, wantBackend: "x11"},
		{name: "no focus", probe: "echo 'window 100 has no pid associated with it.' >&2; exit 1", display: true, x11: true, wtype: true, wantBackend: "wtype"},
		{name: "None PID", probe: "printf '0\\n'", display: true, x11: true, wtype: true, wantBackend: "wtype"},
		{name: "negative PID", probe: "printf '%s\\n' -1", display: true, x11: true, wtype: true, wantBackend: "wtype"},
		{name: "missing tool", missing: true, display: true, x11: true, wtype: true, wantBackend: "wtype"},
		{name: "no display", x11: true, wtype: true, wantBackend: "wtype"},
		{name: "X not opted in", display: true, wtype: true, wantBackend: "wtype"},
		{name: "IME fallback", probe: "echo 'window 100 has no pid associated with it.' >&2; exit 1", display: true, x11: true},
		{name: "typing failure no retry", probe: "printf '1234\\n'", typing: "exit 1", display: true, x11: true, wtype: true, wantBackend: "x11", wantErr: true},
		{name: "unknown probe failure", probe: "exit 1", display: true, x11: true, wtype: true, wantErr: true},
		{name: "None focus", probe: "echo 'xdo_focus_window reported an error' >&2; exit 1", display: true, x11: true, wtype: true, wantBackend: "wtype"},
		{name: "invalid probe no retry", probe: "printf 'invalid\\n'", display: true, x11: true, wtype: true, wantErr: true},
		{name: "probe timeout no retry", probe: "exec /bin/sleep 2", display: true, x11: true, wtype: true, wantErr: true},
		{name: "inherited stdout no retry", probe: "/bin/sleep 2 &\nprintf '1234\\n'", display: true, x11: true, wtype: true, wantErr: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			t.Setenv("PATH", dir)
			t.Setenv("DISPLAY", "")
			t.Setenv("NANOCODEX_WAYLAND_TEXT_X11", "")
			t.Setenv("NANOCODEX_WAYLAND_TEXT_WTYPE", "")
			if tc.display {
				t.Setenv("DISPLAY", ":123")
			}
			if tc.x11 {
				t.Setenv("NANOCODEX_WAYLAND_TEXT_X11", "1")
			}
			if tc.wtype {
				t.Setenv("NANOCODEX_WAYLAND_TEXT_WTYPE", "1")
			}
			t.Setenv("TEST_TEXT_DIR", dir)
			if !tc.missing {
				script := "#!/bin/sh\nif [ \"$1\" = getwindowfocus ]; then\n[ \"$#\" = 2 ] && [ \"$2\" = getwindowpid ] || exit 9\n" + tc.probe + "\nexit 0\nfi\nprintf '%s\\n' \"$@\" > \"$TEST_TEXT_DIR/x11.args\"\n/bin/cat > \"$TEST_TEXT_DIR/x11.text\"\n" + tc.typing + "\n"
				if err := os.WriteFile(filepath.Join(dir, "xdotool"), []byte(script), 0700); err != nil {
					t.Fatal(err)
				}
			}
			if err := os.WriteFile(filepath.Join(dir, "wtype"), []byte("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$TEST_TEXT_DIR/wtype.args\"\n/bin/cat > \"$TEST_TEXT_DIR/wtype.text\"\n"), 0700); err != nil {
				t.Fatal(err)
			}
			text := "private text ' $(no-execution) —\nsecond line\n"
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			start := time.Now()
			handled, err := typeWaylandText(ctx, text)
			if (err != nil) != tc.wantErr {
				t.Fatalf("error = %v; want error %v", err, tc.wantErr)
			}
			if time.Since(start) > time.Second {
				t.Fatal("probe exceeded its bound")
			}
			if !tc.wantErr && handled != (tc.wantBackend != "") {
				t.Fatalf("handled = %v", handled)
			}
			if err != nil && strings.Contains(err.Error(), text) {
				t.Fatal("error contains input text")
			}
			for _, backend := range []string{"x11", "wtype"} {
				data, readErr := os.ReadFile(filepath.Join(dir, backend+".text"))
				if backend == tc.wantBackend {
					if readErr != nil || string(data) != text {
						t.Fatalf("%s did not receive exact stdin", backend)
					}
					args, readErr := os.ReadFile(filepath.Join(dir, backend+".args"))
					wantArgs := "-\n"
					if backend == "x11" {
						wantArgs = "type\n--clearmodifiers\n--delay\n1\n--file\n-\n"
					}
					if readErr != nil || string(args) != wantArgs {
						t.Fatalf("%s arguments = %q", backend, args)
					}
				} else if !os.IsNotExist(readErr) {
					t.Fatalf("unexpected %s invocation", backend)
				}
			}
		})
	}
}
