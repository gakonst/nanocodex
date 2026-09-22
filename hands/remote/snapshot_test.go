package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"image"
	"image/jpeg"
	"image/png"
	"os"
	"path/filepath"
	"testing"
)

func TestSnapshotUsesDirectJPEGAndFallsBackForPNGOnlyGrim(t *testing.T) {
	directory := t.TempDir()
	frame := image.NewRGBA(image.Rect(0, 0, 4, 2))
	var jpg, baseline bytes.Buffer
	if err := jpeg.Encode(&jpg, frame, &jpeg.Options{Quality: 65}); err != nil {
		t.Fatal(err)
	}
	if err := png.Encode(&baseline, frame); err != nil {
		t.Fatal(err)
	}
	files := map[string][]byte{
		"frame.jpg": jpg.Bytes(), "frame.png": baseline.Bytes(), "invalid.jpg": []byte("not JPEG"),
		"grim": []byte("#!/bin/sh\ncase \"$2\" in\njpeg) test \"$GRIM_JPEG\" != unsupported || exit 1; cat \"$GRIM_JPEG\";;\npng) cat \"$GRIM_PNG\";;\n*) exit 2;;\nesac\n"),
	}
	for name, data := range files {
		if err := os.WriteFile(filepath.Join(directory, name), data, 0700); err != nil {
			t.Fatal(err)
		}
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("GRIM_PNG", filepath.Join(directory, "frame.png"))
	for _, variant := range []string{"frame.jpg", "invalid.jpg", "unsupported"} {
		t.Run(variant, func(t *testing.T) {
			path := variant
			if variant != "unsupported" {
				path = filepath.Join(directory, variant)
			}
			t.Setenv("GRIM_JPEG", path)
			result := snapshotDesktop(context.Background(), 4, 2)
			if result.Status != "ok" || result.Width != 4 || result.Height != 2 {
				t.Fatalf("capture failed: %s", result.Status)
			}
			data, err := base64.StdEncoding.DecodeString(result.JPEG)
			if err != nil {
				t.Fatal(err)
			}
			if _, err := jpeg.Decode(bytes.NewReader(data)); err != nil {
				t.Fatal(err)
			}
			if variant == "frame.jpg" && !bytes.Equal(data, jpg.Bytes()) {
				t.Fatal("direct JPEG was re-encoded")
			}
		})
	}
}

func TestCapturePolicyMatchesCanonicalRustPolicy(t *testing.T) {
	canonical, err := os.ReadFile("../../crates/nanocodex-hand/src/capture_policy.json")
	if os.IsNotExist(err) {
		t.Skip("standalone relay checkout")
	}
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(canonical, capturePolicyJSON) {
		t.Fatal("Go capture policy differs from canonical Rust policy")
	}
}

func TestSnapshotBase64Budget(t *testing.T) {
	output := &boundedSnapshot{}
	if _, err := output.Write(make([]byte, capturePolicy.MaxBase64Bytes/4*3)); err != nil {
		t.Fatal(err)
	}
	if base64.StdEncoding.EncodedLen(output.buffer.Len()) > capturePolicy.MaxBase64Bytes {
		t.Fatal("base64 budget exceeded")
	}
	if _, err := output.Write([]byte{0}); err == nil {
		t.Fatal("accepted bytes beyond base64 budget")
	}
}

func TestSnapshotRejectsInvalidSourceDimensions(t *testing.T) {
	for _, size := range [][2]int{{0, 1}, {1, 0}, {-1, 1}, {65537, 1}, {1, 65537}} {
		if result := snapshotDesktop(context.Background(), size[0], size[1]); result.Status != "unavailable" {
			t.Fatalf("accepted %v", size)
		}
	}
}

func TestNoisySnapshotFallbackFitsBase64Budget(t *testing.T) {
	directory := t.TempDir()
	frame := image.NewRGBA(image.Rect(0, 0, 1280, 1280))
	random := uint32(1)
	for i := 0; i < len(frame.Pix); i += 4 {
		for c := 0; c < 3; c++ {
			random ^= random << 13
			random ^= random >> 17
			random ^= random << 5
			frame.Pix[i+c] = byte(random)
		}
		frame.Pix[i+3] = 255
	}
	var jpg, baseline bytes.Buffer
	if err := jpeg.Encode(&jpg, frame, &jpeg.Options{Quality: capturePolicy.JPEGQualities[0]}); err != nil {
		t.Fatal(err)
	}
	if base64.StdEncoding.EncodedLen(jpg.Len()) <= capturePolicy.MaxBase64Bytes {
		t.Fatal("fixture must exceed direct JPEG budget")
	}
	if err := png.Encode(&baseline, frame); err != nil {
		t.Fatal(err)
	}
	for name, data := range map[string][]byte{
		"frame.jpg": jpg.Bytes(), "frame.png": baseline.Bytes(),
		"grim": []byte("#!/bin/sh\ncase \"$2\" in\njpeg) cat \"$GRIM_JPEG\";;\npng) cat \"$GRIM_PNG\";;\n*) exit 2;;\nesac\n"),
	} {
		if err := os.WriteFile(filepath.Join(directory, name), data, 0700); err != nil {
			t.Fatal(err)
		}
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("GRIM_JPEG", filepath.Join(directory, "frame.jpg"))
	t.Setenv("GRIM_PNG", filepath.Join(directory, "frame.png"))
	result := snapshotDesktop(context.Background(), 1280, 1280)
	if result.Status != "ok" || len(result.JPEG) > capturePolicy.MaxBase64Bytes {
		t.Fatalf("capture status %s, base64 bytes %d", result.Status, len(result.JPEG))
	}
	data, err := base64.StdEncoding.DecodeString(result.JPEG)
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := jpeg.Decode(bytes.NewReader(data))
	if err != nil {
		t.Fatal(err)
	}
	if decoded.Bounds().Dx() != 1280 || decoded.Bounds().Dy() != 1280 {
		t.Fatal("wrong decoded dimensions")
	}
}
