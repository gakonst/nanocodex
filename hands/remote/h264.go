package main

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"errors"
	"io"
	"time"

	"github.com/pion/rtp"
	"github.com/pion/rtp/codecs"
)

const maxH264Frame = 8 * 1024 * 1024
const framedH264Magic = "NCH264F1"
const chunkedH264Magic = "NCH264C1"
const maxH264ChunkPayload = 4092
const h264FinalChunk = uint32(1 << 31)

// Waymote enables x264 access-unit delimiters and a fixed 60 Hz encoder.
// Packetize its Annex-B stdout without decoding or re-encoding. The pipe works
// with libkrun TSI, which cannot receive the daemon's loopback UDP stream.
type h264Forwarder struct {
	encoder   codecs.H264Payloader
	sequence  uint16
	timestamp uint32
	now       func() time.Time
}

func (forwarder *h264Forwarder) read(reader io.Reader, write func(*rtp.Packet) error) error {
	if forwarder.now == nil {
		forwarder.now = time.Now
	}
	started := forwarder.now()
	var lastTicks int64
	return readH264Frames(reader, func(frame []byte) error {
		payloads := forwarder.encoder.Payload(1180, frame)
		// A compositor or busy encoder may skip frames. Advancing a fixed
		// 1/60 second per delivered frame makes playback fall behind wall time.
		elapsed := forwarder.now().Sub(started)
		// Split whole seconds before multiplication so long-running publishers
		// do not overflow a nanoseconds * 90 kHz intermediate after ~28 hours.
		ticks := max(lastTicks+1, int64(elapsed/time.Second)*90000+int64(elapsed%time.Second)*90000/int64(time.Second))
		lastTicks, forwarder.timestamp = ticks, uint32(ticks)
		for index, payload := range payloads {
			forwarder.sequence++
			packet := &rtp.Packet{Header: rtp.Header{Version: 2, PayloadType: 96,
				SequenceNumber: forwarder.sequence, Timestamp: forwarder.timestamp,
				SSRC: 1, Marker: index == len(payloads)-1}, Payload: payload}
			if err := write(packet); err != nil {
				return err
			}
		}
		return nil
	})
}

func readH264Frames(reader io.Reader, emit func([]byte) error) error {
	buffered := bufio.NewReader(reader)
	prefix, _ := buffered.Peek(len(framedH264Magic))
	if string(prefix) != framedH264Magic && string(prefix) != chunkedH264Magic {
		scanner := bufio.NewScanner(buffered)
		scanner.Buffer(make([]byte, 64*1024), maxH264Frame)
		scanner.Split(h264AccessUnit)
		for scanner.Scan() {
			if err := emit(scanner.Bytes()); err != nil {
				return err
			}
		}
		return scanner.Err()
	}
	chunked := string(prefix) == chunkedH264Magic
	_, _ = buffered.Discard(len(framedH264Magic))
	var header [4]byte
	var frame []byte
	for {
		_, err := io.ReadFull(buffered, header[:])
		if err == io.EOF && len(frame) == 0 {
			return nil
		}
		if err != nil {
			if err == io.EOF {
				return io.ErrUnexpectedEOF
			}
			return err
		}
		// Waymote SIGKILLs/replaces the encoder on configuration changes while
		// keeping stdout open. C1 records are atomic pipe writes, so a new
		// stream starts at a record boundary even if the last frame was partial.
		// Never search arbitrary encoded payload for a synchronization marker.
		if string(header[:]) == framedH264Magic[:4] {
			var suffix [4]byte
			if _, err := io.ReadFull(buffered, suffix[:]); err != nil {
				return err
			}
			switch string(suffix[:]) {
			case framedH264Magic[4:]:
				chunked = false
			case chunkedH264Magic[4:]:
				chunked = true
			default:
				return errors.New("invalid framed H.264 restart header")
			}
			frame = frame[:0]
			continue
		}
		value := binary.BigEndian.Uint32(header[:])
		final := !chunked || value&h264FinalChunk != 0
		size := int(value)
		if chunked {
			size = int(value & ^h264FinalChunk)
		}
		if size < 1 || size > maxH264Frame || (chunked && size > maxH264ChunkPayload) || len(frame)+size > maxH264Frame {
			return errors.New("invalid framed H.264 size")
		}
		start := len(frame)
		frame = append(frame, make([]byte, size)...)
		if _, err := io.ReadFull(buffered, frame[start:]); err != nil {
			return err
		}
		if !final {
			continue
		}
		if !bytes.HasPrefix(frame, []byte{0, 0, 1}) && !bytes.HasPrefix(frame, []byte{0, 0, 0, 1}) {
			return errors.New("invalid framed H.264 payload")
		}
		if err := emit(frame); err != nil {
			return err
		}
		frame = frame[:0]
	}
}

func h264AccessUnit(data []byte, atEOF bool) (int, []byte, error) {
	if len(data) < 5 && !atEOF {
		return 0, nil, nil
	}
	if len(data) == 0 {
		return 0, nil, nil
	}
	if !bytes.HasPrefix(data, []byte{0, 0, 1}) && !bytes.HasPrefix(data, []byte{0, 0, 0, 1}) {
		return 0, nil, errors.New("invalid H.264 capture stream")
	}
	// A start code cannot occur inside a NAL's escaped payload. Accept both
	// three- and four-byte prefixes, including when a pipe read splits them.
	if len(data) > 4 {
		if index := bytes.Index(data[4:], []byte{0, 0, 1, 9}); index >= 0 {
			end := index + 4
			if data[end-1] == 0 {
				end--
			}
			return end, data[:end], nil
		}
	}
	if atEOF {
		return len(data), data, nil
	}
	return 0, nil, nil
}
