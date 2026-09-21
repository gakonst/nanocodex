package main

import (
	"bufio"
	"context"
	"encoding/binary"
	"fmt"
	"io"
	"os"
	"os/exec"
	"runtime"
	"strconv"
	"strings"
	"time"
)

// Waymote invokes this same executable as its FFmpeg child. Keep encoder policy
// in the distributed companion, so native hosts and both VM image architectures
// share it without a machine-local wrapper or an NVIDIA requirement.
const encoderHelperEnv = "NANOCODEX_SCREEN_ENCODER_HELPER"

func runScreenEncoder() error {
	prepareScreenEncoderPipe(os.Stdin)
	ffmpeg, err := exec.LookPath("ffmpeg")
	if err != nil {
		return err
	}
	mode := os.Getenv("NANOCODEX_VIDEO_ENCODER")
	if mode == "" {
		mode = "auto"
	}
	if mode != "auto" && mode != "software" && mode != "nvenc" {
		return fmt.Errorf("invalid screen encoder %q", mode)
	}
	hardware := false
	if mode != "software" {
		candidate, err := screenEncoderArgs(os.Args[1:], true)
		if err != nil {
			return err
		}
		var size string
		var outputOptions []string
		for i := 0; i+1 < len(candidate); i++ {
			if candidate[i] == "-video_size" {
				size = candidate[i+1]
			}
			if candidate[i] == "-i" {
				outputOptions = candidate[i+2 : len(candidate)-1]
				break
			}
		}
		if size == "" || outputOptions == nil {
			return fmt.Errorf("missing raw screen dimensions")
		}
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		// Exercise the actual dimensions, pixel format and encoder options;
		// an encoder listed by FFmpeg may still lack usable hardware/drivers.
		probeArgs := append([]string{"-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=size=" + size + ":rate=60"}, outputOptions...)
		probe := exec.CommandContext(ctx, ffmpeg, append(probeArgs, "-frames:v", "1", "pipe:1")...)
		hardware = probe.Run() == nil
		cancel()
		if mode == "nvenc" && !hardware {
			return fmt.Errorf("NVENC screen encoder unavailable")
		}
	}
	args, err := screenEncoderArgs(os.Args[1:], hardware)
	if err != nil {
		return err
	}
	if hardware {
		fmt.Fprintln(os.Stderr, "Screen encoder: NVIDIA NVENC")
	} else {
		fmt.Fprintln(os.Stderr, "Screen encoder: software H.264")
	}
	if os.Getenv("NANOCODEX_SCREEN_FRAME_BOUNDARIES") == "annexb" {
		command := exec.Command(ffmpeg, args...)
		command.Stdin, command.Stdout, command.Stderr = os.Stdin, os.Stdout, os.Stderr
		return command.Run()
	}
	return runFramedScreenEncoder(ffmpeg, args, os.Stdin, os.Stdout)
}

func screenEncoderArgs(original []string, hardware bool) ([]string, error) {
	values := map[string]string{}
	for i := 0; i+1 < len(original); i++ {
		values[original[i]] = original[i+1]
	}
	fps, err := strconv.Atoi(values["-framerate"])
	if err != nil || fps < 1 || fps > 240 || values["-f"] != "h264" || original[len(original)-1] != "pipe:1" {
		return nil, fmt.Errorf("unsupported screen encoder input")
	}
	bitrate := values["-b:v"]
	kbps, err := strconv.Atoi(trimK(bitrate))
	if err != nil || kbps < 1 {
		return nil, fmt.Errorf("invalid screen bitrate")
	}
	replace := map[string]string{"-maxrate": bitrate, "-bufsize": fmt.Sprintf("%dk", max(1, kbps/fps)), "-g": strconv.Itoa(max(1, fps/2)), "-keyint_min": strconv.Itoa(max(1, fps/2))}
	if hardware {
		replace["-c:v"], replace["-preset"], replace["-tune"] = "h264_nvenc", "p3", "ull"
		// NVENC converts Waymote's BGRA on the GPU. Software keeps yuv420p.
		replace["-pix_fmt"] = "bgra"
	}
	args := make([]string, 0, len(original)+16)
	for i := 0; i < len(original)-1; i++ {
		key := original[i]
		// Capture is already paced by the compositor. -re adds another clock
		// and can queue stale frames after stalls.
		if key == "-re" {
			continue
		}
		if key == "-vf" && i+1 < len(original) && original[i+1] == "scale="+strings.ReplaceAll(values["-video_size"], "x", ":") {
			i++
			continue
		}
		if hardware && (key == "-x264-params" || key == "-sc_threshold" || key == "-keyint_min") {
			i++
			continue
		}
		if value, ok := replace[key]; ok {
			args = append(args, key, value)
			i++
			continue
		}
		args = append(args, key)
	}
	if hardware {
		args = append(args, "-rc", "cbr", "-rc-lookahead", "0", "-zerolatency", "1", "-delay", "0", "-aud", "1")
	}
	return append(args, "-flush_packets", "1", "pipe:1"), nil
}

func trimK(value string) string {
	if len(value) > 0 && value[len(value)-1] == 'k' {
		return value[:len(value)-1]
	}
	return ""
}

// The tee's first slave reports each encoded packet length before the second
// slave writes its bytes. Both flush every packet, so forwarding never waits
// for the next capture. Unlike pipe-read boundaries, these are real access-unit
// boundaries supplied by the encoder. Keep FFmpeg diagnostics on stderr.
func runFramedScreenEncoder(ffmpeg string, args []string, input io.Reader, output io.Writer) error {
	metadata, metadataWriter, err := os.Pipe()
	if err != nil {
		return err
	}
	defer metadata.Close()
	defer metadataWriter.Close()
	// Raw capture metadata is fully specified; avoid probing additional frames.
	args = append([]string{"-probesize", "32", "-analyzeduration", "0"}, args...)
	for i := len(args) - 2; i > 0; i-- {
		if args[i-1] == "-f" && args[i] == "h264" {
			args[i] = "tee"
			break
		}
	}
	args = append(args[:len(args)-1], "-map", "0:v:0", "[f=framecrc:flush_packets=1]pipe:3|[f=h264:flush_packets=1]pipe:1")
	command := exec.Command(ffmpeg, args...)
	command.Stdin, command.Stderr = input, os.Stderr
	command.ExtraFiles = []*os.File{metadataWriter}
	video, err := command.StdoutPipe()
	if err != nil {
		return err
	}
	if err = command.Start(); err != nil {
		return err
	}
	metadataWriter.Close()
	err = forwardEncodedFrames(metadata, video, output)
	if err != nil {
		_ = command.Process.Kill()
	}
	waitErr := command.Wait()
	if err != nil {
		return err
	}
	return waitErr
}

func forwardEncodedFrames(metadata io.Reader, video io.Reader, output io.Writer) error {
	if _, err := io.WriteString(output, chunkedH264Magic); err != nil {
		return err
	}
	scanner := bufio.NewScanner(metadata)
	scanner.Buffer(make([]byte, 1024), 16*1024)
	var frame []byte
	for scanner.Scan() {
		line := scanner.Text()
		if strings.HasPrefix(line, "#") {
			continue
		}
		fields := strings.Split(line, ",")
		if len(fields) < 6 || strings.TrimSpace(fields[0]) != "0" {
			return fmt.Errorf("invalid encoder frame metadata")
		}
		size, err := strconv.Atoi(strings.TrimSpace(fields[4]))
		if err != nil || size < 1 || size > maxH264Frame {
			return fmt.Errorf("invalid encoder frame size")
		}
		if cap(frame) < size {
			frame = make([]byte, size)
		} else {
			frame = frame[:size]
		}
		if _, err := io.ReadFull(video, frame); err != nil {
			return err
		}
		if err := writeEncodedFrame(output, frame); err != nil {
			return err
		}
	}
	return scanner.Err()
}

// One Write contains an entire record. Linux guarantees pipe writes <=4096
// bytes are atomic, including when Waymote kills a blocked encoder. Other Unix
// platforms use POSIX's conservative 512-byte lower bound. A replacement child
// can therefore reset a partial frame at the next record boundary safely.
func writeEncodedFrame(output io.Writer, frame []byte) error {
	payloadLimit := 512 - 4
	if runtime.GOOS == "linux" {
		payloadLimit = maxH264ChunkPayload
	}
	var record [maxH264ChunkPayload + 4]byte
	for len(frame) > 0 {
		size := min(len(frame), payloadLimit)
		value := uint32(size)
		if size == len(frame) {
			value |= h264FinalChunk
		}
		binary.BigEndian.PutUint32(record[:4], value)
		copy(record[4:], frame[:size])
		n, err := output.Write(record[:size+4])
		if err != nil {
			return err
		}
		if n != size+4 {
			return io.ErrShortWrite
		}
		frame = frame[size:]
	}
	return nil
}
