package main

import (
	"bufio"
	"bytes"
	"errors"
	"io"

	"github.com/pion/rtp"
	"github.com/pion/rtp/codecs"
)

const maxH264Frame = 8 * 1024 * 1024

// Waymote enables x264 access-unit delimiters and a fixed 60 Hz encoder.
// Packetize its Annex-B stdout without decoding or re-encoding. The pipe works
// with libkrun TSI, which cannot receive the daemon's loopback UDP stream.
type h264Forwarder struct {
	encoder   codecs.H264Payloader
	sequence  uint16
	timestamp uint32
}

func (forwarder *h264Forwarder) read(reader io.Reader, write func(*rtp.Packet) error) error {
	scanner := bufio.NewScanner(reader)
	scanner.Buffer(make([]byte, 64*1024), maxH264Frame)
	scanner.Split(h264AccessUnit)
	for scanner.Scan() {
		payloads := forwarder.encoder.Payload(1180, scanner.Bytes())
		forwarder.timestamp += 90000 / 60
		for index, payload := range payloads {
			forwarder.sequence++
			packet := &rtp.Packet{Header: rtp.Header{Version: 2, PayloadType: 96,
				SequenceNumber: forwarder.sequence, Timestamp: forwarder.timestamp,
				SSRC: 1, Marker: index == len(payloads)-1}, Payload: payload}
			if err := write(packet); err != nil {
				return err
			}
		}
	}
	return scanner.Err()
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
