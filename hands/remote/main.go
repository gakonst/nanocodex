// Nanocodex's native remote Hand companion. Account authentication and remote
// signaling stay outside the unauthenticated developer-device HTTP protocol.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/signal"
	"syscall"

	"github.com/danielpaulus/go-ios/ios"
	"github.com/danielpaulus/go-ios/ios/forward"
)

func main() {
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	if len(os.Args) > 1 && os.Args[1] == "phone-list" {
		if err := listPhones(); err != nil {
			log.Fatal(err)
		}
		return
	}
	if len(os.Args) > 1 && os.Args[1] == "phone-bridge" {
		flags := flag.NewFlagSet("phone-bridge", flag.ExitOnError)
		udid := flags.String("udid", "", "exact paired iPhone UDID")
		runner := flags.String("xctestrun", "", "locally built and signed WebDriverAgent xctestrun")
		parent := flags.Bool("parent-stdin", false, "stop when the owning application's stdin closes")
		_ = flags.Parse(os.Args[2:])
		if *parent {
			go func() { _, _ = io.Copy(io.Discard, os.Stdin); cancel() }()
		}
		if err := phoneBridge(ctx, *udid, *runner); err != nil && !errors.Is(err, context.Canceled) {
			log.Fatal(err)
		}
		return
	}
	if len(os.Args) > 1 && (os.Args[1] == "wayland-host" || os.Args[1] == "desktop-host" || os.Args[1] == "server-host") {
		flags := flag.NewFlagSet("wayland-host", flag.ExitOnError)
		var config hostConfig
		workspace := flags.String("workspace", "/workspace", "desktop working directory")
		desktopConfig := flags.String("desktop-config", "/etc/nanocodex-desktop", "labwc configuration directory")
		flags.StringVar(&config.Origin, "url", "", "managed account service origin")
		flags.StringVar(&config.CredentialFile, "credential-file", "", "private file containing a Hand credential")
		flags.StringVar(&config.MachineID, "machine-id", "", "allocated machine identity")
		flags.StringVar(&config.Name, "name", "VM Hand", "display name")
		flags.StringVar(&config.Waymote, "waymote", "waymote-streamd", "Waymote executable")
		flags.IntVar(&config.Width, "width", 1600, "headless output width")
		flags.IntVar(&config.Height, "height", 900, "headless output height")
		flags.BoolVar(&config.IncludeLoopback, "include-loopback", false, "allow local viewers on the host network")
		flags.BoolVar(&config.Frames, "frames", false, "use bounded HTTPS frame/input relay for restricted sandboxes")
		_ = flags.Parse(os.Args[2:])
		run := serveWayland
		if os.Args[1] == "desktop-host" {
			run = func(ctx context.Context, config hostConfig) error { return serveDesktop(ctx, config, *workspace) }
		}
		if os.Args[1] == "server-host" {
			run = func(ctx context.Context, config hostConfig) error {
				return serveDesktopSession(ctx, config, *workspace, *desktopConfig, false)
			}
		}
		if err := run(ctx, config); errors.Is(err, errRemoteHostReplaced) {
			// Cleanup has stopped capture and any owned compositor. Stay idle so
			// Docker's restart policy cannot reclaim the newer publisher's screen.
			// An explicit daemon restart enables publishing again.
			log.Print("Remote host replaced; restart this daemon to share again")
			<-ctx.Done()
		} else if err != nil && !errors.Is(err, context.Canceled) {
			log.Fatal(err)
		}
		return
	}
	if len(os.Args) < 2 || os.Args[1] != "phone-tunnel" {
		log.Fatal("usage: nanocodex-remote phone-tunnel --udid DEVICE | wayland-host --url ORIGIN --credential-file PATH --machine-id ID")
	}
	flags := flag.NewFlagSet("phone-tunnel", flag.ExitOnError)
	udid := flags.String("udid", "", "exact paired iPhone UDID")
	control := flags.Int("control-port", 18100, "phone runner control port")
	video := flags.Int("video-port", 19100, "phone runner video port")
	_ = flags.Parse(os.Args[2:])
	if *udid == "" || *control < 1024 || *control > 65535 || *video < 1024 || *video > 65535 || *control == *video {
		log.Fatal("an exact device and distinct unprivileged ports are required")
	}
	device, err := ios.GetDevice(*udid)
	if err != nil {
		log.Fatal(err)
	}
	if device.Properties.SerialNumber != *udid {
		log.Fatal("paired device identity mismatch")
	}
	if err := phoneTunnel(ctx, device.DeviceID, []int{*control, *video}, nil); err != nil && !errors.Is(err, context.Canceled) {
		log.Fatal(err)
	}
}

// The general-purpose go-ios CLI binds all interfaces. This app-owned listener
// deliberately accepts only local native clients; the account-facing transport
// is authenticated WebRTC, never a public WDA port.
func phoneTunnel(ctx context.Context, deviceID int, ports []int, ready func()) error {
	var listeners []net.Listener
	defer func() {
		for _, listener := range listeners {
			_ = listener.Close()
		}
	}()
	for _, port := range ports {
		listener, err := net.Listen("tcp4", fmt.Sprintf("127.0.0.1:%d", port))
		if err != nil {
			return err
		}
		listeners = append(listeners, listener)
	}
	errCh := make(chan error, len(listeners))
	for index, listener := range listeners {
		port := uint16(ports[index])
		go func() {
			for {
				connection, err := listener.Accept()
				if err != nil {
					errCh <- err
					return
				}
				go func() { _ = forward.StartNewProxyConnection(ctx, connection, deviceID, port) }()
			}
		}()
	}
	log.Printf("Paired phone tunnel ready on loopback ports %v", ports)
	if ready != nil {
		ready()
	}
	select {
	case <-ctx.Done():
		return ctx.Err()
	case err := <-errCh:
		return err
	}
}
