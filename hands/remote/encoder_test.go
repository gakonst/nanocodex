package main

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"io"
	"os"
	"os/exec"
	"runtime"
	"strings"
	"testing"
	"time"
)

// The Wayland integration tests execute this test binary as Waymote's encoder,
// exercising the same subprocess boundary as the production companion.
func TestMain(m *testing.M) {
	if os.Getenv("NANOCODEX_TEST_BROADCAST_CAPTURE") == "1" && os.Getenv(broadcastHelperEnv) == "1" {
		os.Exit(runSyntheticBroadcastCapture())
	}
	if os.Getenv(broadcastHelperEnv) == "1" {
		if runBroadcastEncoder() != nil {
			os.Exit(1)
		}
		os.Exit(0)
	}
	if os.Getenv("NANOCODEX_TEST_ATOMIC_ENCODER") == "1" {
		_, _ = io.WriteString(os.Stdout, chunkedH264Magic)
		frame := append([]byte{0, 0, 0, 1, 0x65}, bytes.Repeat([]byte{0x35}, 1024*1024)...)
		if err := writeEncodedFrame(os.Stdout, frame); err != nil {
			os.Exit(1)
		}
		os.Exit(0)
	}
	if os.Getenv(encoderHelperEnv) == "1" {
		if err := runScreenEncoder(); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		os.Exit(0)
	}
	os.Exit(m.Run())
}

func TestScreenEncoderPreservesVideoContractWithoutPacingTwice(t *testing.T) {
	input := strings.Fields("-hide_banner -f rawvideo -pixel_format bgra -video_size 1600x900 -framerate 60 -re -i pipe:0 -an -c:v libx264 -preset ultrafast -tune zerolatency -profile:v baseline -pix_fmt yuv420p -b:v 6000k -maxrate 12000k -bufsize 12000k -g 60 -keyint_min 60 -sc_threshold 0 -bf 0 -x264-params aud=1:repeat-headers=1 -vf scale=1600:900 -f h264 pipe:1")
	for _, hardware := range []bool{false, true} {
		args, err := screenEncoderArgs(input, hardware)
		if err != nil {
			t.Fatal(err)
		}
		wire := " " + strings.Join(args, " ") + " "
		for _, required := range []string{" -bufsize 100k ", " -g 30 ", " -bf 0 ", " -flush_packets 1 ", " pipe:1 "} {
			if !strings.Contains(wire, required) {
				t.Fatalf("missing %q: %s", required, wire)
			}
		}
		if strings.Contains(wire, " -re ") || strings.Contains(wire, " -vf ") {
			t.Fatal("capture was paced or resized twice")
		}
		if hardware {
			if !strings.Contains(wire, " -c:v h264_nvenc ") || !strings.Contains(wire, " -aud 1 ") || strings.Contains(wire, " -x264-params ") {
				t.Fatal("invalid NVENC framing")
			}
		} else if !strings.Contains(wire, " -c:v libx264 ") || !strings.Contains(wire, " -x264-params aud=1:repeat-headers=1 ") {
			t.Fatal("software fallback lost H.264 framing")
		}
	}
}

func TestScreenEncoderRejectsMissingClockOrOutput(t *testing.T) {
	for _, args := range [][]string{nil, {"-framerate", "0"}, {"-framerate", "60", "-b:v", "6000k", "-f", "mp4", "pipe:1"}} {
		if _, err := screenEncoderArgs(args, false); err == nil {
			t.Fatal("invalid screen stream accepted")
		}
	}
}

func TestScreenNetworkPortBounds(t *testing.T) {
	for _, config := range []hostConfig{{UDPPortMin: 50000}, {UDPPortMax: 50031}, {UDPPortMin: 50031, UDPPortMax: 50000}, {UDPPortMin: 1, UDPPortMax: 65536}, {Interface: "nanocodex-no-such-interface"}} {
		if config.validateNetwork() == nil {
			t.Fatal("invalid network configuration accepted")
		}
	}
	for _, config := range []hostConfig{{}, {UDPPortMin: 50000, UDPPortMax: 50031}} {
		if err := config.validateNetwork(); err != nil {
			t.Fatal(err)
		}
	}
}

// Keep raw input open after one frame: an Annex-B lookahead parser would stall
// forever here. This exercises real encoder, tee flushes, framing, and reader.
func TestEncoderForwardsOneFrameBeforeNextCapture(t *testing.T) {
	ffmpeg, err := exec.LookPath("ffmpeg")
	if err != nil {
		t.Skip("FFmpeg unavailable")
	}
	input, feed := io.Pipe()
	output, sink := io.Pipe()
	defer input.Close()
	defer feed.Close()
	defer output.Close()
	defer sink.Close()
	args := strings.Fields("-hide_banner -loglevel error -f rawvideo -pixel_format bgra -video_size 64x64 -framerate 60 -i pipe:0 -an -c:v libx264 -preset ultrafast -tune zerolatency -pix_fmt yuv420p -x264-params aud=1:repeat-headers=1 -f h264 pipe:1")
	done := make(chan error, 1)
	go func() { done <- runFramedScreenEncoder(ffmpeg, args, input, sink); sink.Close() }()
	frames := make(chan []byte, 2)
	go func() {
		_ = readH264Frames(output, func(frame []byte) error { frames <- append([]byte{}, frame...); return nil })
	}()
	if _, err := feed.Write(make([]byte, 64*64*4)); err != nil {
		t.Fatal(err)
	}
	select {
	case frame := <-frames:
		if len(frame) < 10 {
			t.Fatal("empty encoded frame")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("encoder waited for another capture")
	}
	feed.Close()
	select {
	case err := <-done:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("encoder did not exit")
	}
}

func TestEncoderMetadataRejectsMalformedAndTruncatedFrames(t *testing.T) {
	for _, line := range []string{"garbage", "1,0,0,1,4,0x00", "0,0,0,1,0,0x00", "0,0,0,1,8388609,0x00", "0,0,0,1,8,0x00"} {
		if err := forwardEncodedFrames(strings.NewReader(line+"\n"), strings.NewReader("tiny"), io.Discard); err == nil {
			t.Fatalf("accepted %q", line)
		}
	}
}

func TestKilledEncoderRestartsAtAtomicRecordBoundary(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("Unix Waymote process boundary")
	}
	reader, writer, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	defer reader.Close()
	defer writer.Close()
	child := exec.Command(os.Args[0])
	child.Env = append(os.Environ(), "NANOCODEX_TEST_ATOMIC_ENCODER=1")
	child.Stdout = writer
	if err := child.Start(); err != nil {
		t.Fatal(err)
	}
	defer child.Process.Kill()
	// Read just one complete nonfinal record, then SIGKILL while the large
	// frame writer is backpressured by the pipe. Keep the shared pipe open.
	prefix := make([]byte, 12)
	if _, err := io.ReadFull(reader, prefix); err != nil {
		t.Fatal(err)
	}
	size := int(binary.BigEndian.Uint32(prefix[8:]))
	if string(prefix[:8]) != chunkedH264Magic || size < 1 || size > maxH264ChunkPayload {
		t.Fatal("invalid initial atomic record")
	}
	first := make([]byte, size)
	if _, err := io.ReadFull(reader, first); err != nil {
		t.Fatal(err)
	}
	if err := child.Process.Kill(); err != nil {
		t.Fatal(err)
	}
	_ = child.Wait()
	second := []byte{0, 0, 0, 1, 0x67, 0x42, 0, 0x34, 0, 0, 1, 0x68, 0x22, 0, 0, 1, 0x65, 0x44}
	done := make(chan error, 1)
	go func() {
		_, err := io.WriteString(writer, chunkedH264Magic)
		if err == nil {
			err = writeEncodedFrame(writer, second)
		}
		writer.Close()
		done <- err
	}()
	var frames [][]byte
	err = readH264Frames(io.MultiReader(bytes.NewReader(append(prefix, first...)), reader), func(frame []byte) error { frames = append(frames, append([]byte{}, frame...)); return nil })
	if err != nil || len(frames) != 1 || !bytes.Equal(frames[0], second) {
		t.Fatalf("killed frame corrupted replacement: %v, %d frames", err, len(frames))
	}
	if err := <-done; err != nil {
		t.Fatal(err)
	}
}
