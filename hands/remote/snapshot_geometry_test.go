package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/binary"
	"fmt"
	"hash/crc32"
	"image"
	"image/color"
	"image/draw"
	"image/jpeg"
	"image/png"
	"os"
	"path/filepath"
	"testing"
)

func cornerFixture(width, height int) *image.RGBA {
	frame := image.NewRGBA(image.Rect(0, 0, width, height))
	draw.Draw(frame, frame.Bounds(), &image.Uniform{C: color.RGBA{100, 100, 100, 255}}, image.Point{}, draw.Src)
	colors := []color.RGBA{{240, 20, 20, 255}, {20, 240, 20, 255}, {20, 20, 240, 255}, {240, 240, 20, 255}}
	dx, dy := max(8, width/32), max(8, height/32)
	rects := []image.Rectangle{image.Rect(0, 0, dx, dy), image.Rect(width-dx, 0, width, dy), image.Rect(0, height-dy, dx, height), image.Rect(width-dx, height-dy, width, height)}
	for i, r := range rects {
		draw.Draw(frame, r, &image.Uniform{C: colors[i]}, image.Point{}, draw.Src)
	}
	return frame
}

func TestSnapshotNativeGeometryIgnoresConfiguredScale(t *testing.T) {
	dir := t.TempDir()
	// grim is only a fixture reader; reject any scale or region option so the
	// test cannot accidentally accept the old double-downscaling/cropping path.
	script := "#!/bin/sh\nfor arg in \"$@\"; do\ncase \"$arg\" in -s|-g) exit 9;; esac\ndone\ncase \"$2\" in\njpeg) [ \"$TEST_PNG_ONLY\" != 1 ] || exit 1; cat \"$TEST_NATIVE_JPEG\";;\npng) cat \"$TEST_NATIVE_PNG\";;\n*) exit 2;;\nesac\n"
	if err := os.WriteFile(filepath.Join(dir, "grim"), []byte(script), 0700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	for _, size := range [][2]int{{1920, 1080}, {2560, 1440}, {1080, 1920}, {801, 601}} {
		frame := cornerFixture(size[0], size[1])
		var jpg, pngData bytes.Buffer
		if err := jpeg.Encode(&jpg, frame, &jpeg.Options{Quality: 90}); err != nil {
			t.Fatal(err)
		}
		if err := png.Encode(&pngData, frame); err != nil {
			t.Fatal(err)
		}
		for name, data := range map[string][]byte{"native.jpg": jpg.Bytes(), "native.png": pngData.Bytes()} {
			if err := os.WriteFile(filepath.Join(dir, name), data, 0600); err != nil {
				t.Fatal(err)
			}
		}
		t.Setenv("TEST_NATIVE_JPEG", filepath.Join(dir, "native.jpg"))
		t.Setenv("TEST_NATIVE_PNG", filepath.Join(dir, "native.png"))
		scales := []float64{1, 1.25, 2, 3}
		if size[0] != 1920 {
			scales = []float64{2}
		}
		for _, scale := range scales {
			for _, pngOnly := range []string{"0", "1"} {
				t.Run(fmt.Sprintf("%dx%d/scale%g/png%s", size[0], size[1], scale, pngOnly), func(t *testing.T) {
					t.Setenv("TEST_PNG_ONLY", pngOnly)
					// Deliberately stale/configured mode, independent of live raster size.
					result := snapshotDesktop(context.Background(), int(3840*scale), int(2160*scale))
					wantW, wantH := size[0], size[1]
					if size[0] == 1920 {
						wantW, wantH = 1280, 720
					}
					if size[0] == 2560 {
						wantW, wantH = 1280, 720
					}
					if size[1] == 1920 {
						wantW, wantH = 720, 1280
					}
					if result.Status != "ok" || result.Width != wantW || result.Height != wantH || len(result.JPEG) > capturePolicy.MaxBase64Bytes {
						t.Fatalf("status=%s size=%dx%d", result.Status, result.Width, result.Height)
					}
					data, err := base64.StdEncoding.DecodeString(result.JPEG)
					if err != nil {
						t.Fatal(err)
					}
					decoded, err := jpeg.Decode(bytes.NewReader(data))
					if err != nil {
						t.Fatal(err)
					}
					if decoded.Bounds().Dx() != wantW || decoded.Bounds().Dy() != wantH {
						t.Fatal("encoded geometry differs from reported geometry")
					}
					assertSnapshotCorners(t, decoded)
				})
			}
		}
	}
}

func assertSnapshotCorners(t *testing.T, frame image.Image) {
	t.Helper()
	w, h := frame.Bounds().Dx(), frame.Bounds().Dy()
	points := []image.Point{{0, 0}, {w - 1, 0}, {0, h - 1}, {w - 1, h - 1}}
	wants := []color.RGBA{{240, 20, 20, 255}, {20, 240, 20, 255}, {20, 20, 240, 255}, {240, 240, 20, 255}}
	for i, p := range points {
		got := color.RGBAModel.Convert(frame.At(p.X, p.Y)).(color.RGBA)
		for _, d := range []int{int(got.R) - int(wants[i].R), int(got.G) - int(wants[i].G), int(got.B) - int(wants[i].B)} {
			if d < -20 || d > 20 {
				t.Fatalf("corner %d changed: %v", i, got)
			}
		}
	}
}

func TestSnapshotGeometryBoundsAndRounding(t *testing.T) {
	for _, tc := range []struct {
		w, h, x, y int
		valid      bool
	}{
		{1920, 1080, 1280, 720, true}, {3840, 2160, 1280, 720, true}, {7680, 4320, 1280, 720, true},
		{1080, 1920, 720, 1280, true}, {1921, 1081, 1280, 720, true}, {1, 65536, 1, 1280, true},
		{0, 1080, 0, 0, false}, {65537, 1, 0, 0, false}, {65536, 65536, 0, 0, false},
	} {
		x, y, valid := snapshotDimensions(tc.w, tc.h)
		if x != tc.x || y != tc.y || valid != tc.valid {
			t.Fatalf("%dx%d => %dx%d %v", tc.w, tc.h, x, y, valid)
		}
	}
}

// Native JPEG -> bounded JPEG only; excludes grim, network, and display.
func BenchmarkSnapshotNative1080p(b *testing.B) {
	var data bytes.Buffer
	if err := jpeg.Encode(&data, cornerFixture(1920, 1080), &jpeg.Options{Quality: 65}); err != nil {
		b.Fatal(err)
	}
	b.ReportAllocs()
	for b.Loop() {
		r := encodeSnapshot(context.Background(), data.Bytes(), "jpeg")
		if r.Status != "ok" || r.Width != 1280 || r.Height != 720 {
			b.Fatal(r.Status)
		}
	}
}

func TestSnapshotDecodeGuards(t *testing.T) {
	var source bytes.Buffer
	if err := png.Encode(&source, cornerFixture(32, 32)); err != nil {
		t.Fatal(err)
	}
	oversized := append([]byte(nil), source.Bytes()...)
	// A structurally valid PNG header with dimensions exceeding the decode cap.
	binary.BigEndian.PutUint32(oversized[16:20], 65536)
	binary.BigEndian.PutUint32(oversized[20:24], 65536)
	binary.BigEndian.PutUint32(oversized[29:33], crc32.ChecksumIEEE(oversized[12:29]))
	if _, err := png.DecodeConfig(bytes.NewReader(oversized)); err != nil {
		t.Fatal(err)
	}
	if encodeSnapshot(context.Background(), oversized, "png").Status != "unavailable" {
		t.Fatal("oversized header accepted")
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if encodeSnapshot(ctx, source.Bytes(), "png").Status != "unavailable" {
		t.Fatal("cancelled capture accepted")
	}
}
