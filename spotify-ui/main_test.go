package main

import (
	"image"
	"image/color"
	"math"
	"testing"
)

func TestRollSizesForProgress(t *testing.T) {
	tests := []struct {
		name     string
		progress float64
		left     int
		right    int
	}{
		{name: "start", progress: 0, left: leftRollMinSize, right: rightRollMaxSize},
		{name: "middle", progress: 0.5, left: 316, right: 316},
		{name: "end", progress: 1, left: leftRollMaxSize, right: rightRollMinSize},
		{name: "clamped low", progress: -1, left: leftRollMinSize, right: rightRollMaxSize},
		{name: "clamped high", progress: 2, left: leftRollMaxSize, right: rightRollMinSize},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			left, right := rollSizesForProgress(tc.progress)
			if left != tc.left || right != tc.right {
				t.Fatalf("rollSizesForProgress(%v) = (%d, %d), want (%d, %d)", tc.progress, left, right, tc.left, tc.right)
			}
		})
	}
}

func TestFrameIndexForAngle(t *testing.T) {
	const frames = 60

	tests := []struct {
		name  string
		angle float64
		want  int
	}{
		{name: "zero", angle: 0, want: 0},
		{name: "quarter", angle: math.Pi / 2, want: 15},
		{name: "half", angle: math.Pi, want: 30},
		{name: "wrap", angle: 2 * math.Pi, want: 0},
		{name: "negative", angle: -math.Pi / 2, want: 45},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := frameIndexForAngle(tc.angle, frames)
			if got != tc.want {
				t.Fatalf("frameIndexForAngle(%v, %d) = %d, want %d", tc.angle, frames, got, tc.want)
			}
		})
	}
}

func TestQuantizeRollSize(t *testing.T) {
	tests := []struct {
		name string
		size int
		want int
	}{
		{name: "min", size: leftRollMinSize, want: leftRollMinSize},
		{name: "rounds down", size: 210, want: 200},
		{name: "rounds up", size: 214, want: 224},
		{name: "middle bucket", size: 317, want: 320},
		{name: "max", size: leftRollMaxSize, want: leftRollMaxSize},
		{name: "clamped low", size: 100, want: leftRollMinSize},
		{name: "clamped high", size: 999, want: leftRollMaxSize},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := quantizeRollSize(tc.size)
			if got != tc.want {
				t.Fatalf("quantizeRollSize(%d) = %d, want %d", tc.size, got, tc.want)
			}
		})
	}
}

func TestRollCacheSizes(t *testing.T) {
	got := rollCacheSizes()
	want := []int{200, 224, 248, 272, 296, 320, 344, 368, 392, 416, 432}
	if len(got) != len(want) {
		t.Fatalf("rollCacheSizes length = %d, want %d", len(got), len(want))
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("rollCacheSizes[%d] = %d, want %d", i, got[i], want[i])
		}
	}
}

func TestDrawRGBAAlphaBlendsOverOpaqueBuffer(t *testing.T) {
	app := &App{backBuf: make([]byte, fbSize)}
	for i := 0; i < len(app.backBuf); i += bpp {
		app.backBuf[i+0] = 10
		app.backBuf[i+1] = 20
		app.backBuf[i+2] = 30
		app.backBuf[i+3] = 255
	}

	src := image.NewRGBA(image.Rect(0, 0, 2, 1))
	src.SetRGBA(0, 0, colorRGBA(200, 100, 50, 255))
	src.SetRGBA(1, 0, colorRGBA(100, 200, 50, 128))

	app.drawRGBAAlpha(src, 0, 0)

	if got := app.backBuf[0:4]; got[0] != 50 || got[1] != 100 || got[2] != 200 || got[3] != 255 {
		t.Fatalf("opaque pixel = %v, want BGRA [50 100 200 255]", got)
	}

	got := app.backBuf[4:8]
	wantB := byte((50*128 + 10*127) / 255)
	wantG := byte((200*128 + 20*127) / 255)
	wantR := byte((100*128 + 30*127) / 255)
	if got[0] != wantB || got[1] != wantG || got[2] != wantR || got[3] != 255 {
		t.Fatalf("blended pixel = %v, want BGRA [%d %d %d 255]", got, wantB, wantG, wantR)
	}
}

func TestBuildOverlayWindowIgnoresMaskArtwork(t *testing.T) {
	tapeA := image.NewRGBA(image.Rect(0, 0, windowW, windowH))
	tapeA.SetRGBA(10, 10, color.RGBA{R: 20, G: 30, B: 40, A: 255})

	mask := image.NewRGBA(image.Rect(0, 0, windowW, windowH))
	mask.SetRGBA(0, 0, color.RGBA{R: 255, G: 0, B: 0, A: 255})

	got := buildOverlayWindow(tapeA, mask)

	r, g, b, a := got.At(0, coverMaskOffsetY).RGBA()
	if r != 0 || g != 0 || b != 0 || a != 0 {
		t.Fatalf("mask artwork leaked into overlay window: rgba=(%d,%d,%d,%d)", r>>8, g>>8, b>>8, a>>8)
	}

	r, g, b, a = got.At(10, 10).RGBA()
	if r>>8 != 20 || g>>8 != 30 || b>>8 != 40 || a>>8 != 255 {
		t.Fatalf("tapeA pixel lost from overlay window: rgba=(%d,%d,%d,%d)", r>>8, g>>8, b>>8, a>>8)
	}
}

func TestBuildMaskedCoverUsesMaskAlphaDirectly(t *testing.T) {
	cover := image.NewRGBA(image.Rect(0, 0, windowW, windowH))
	cover.SetRGBA(0, 0, color.RGBA{R: 10, G: 20, B: 30, A: 255})
	cover.SetRGBA(windowW/2, windowH/2, color.RGBA{R: 200, G: 150, B: 100, A: 255})

	mask := image.NewRGBA(image.Rect(0, 0, windowW, windowH))
	mask.SetRGBA(0, 0, color.RGBA{A: 0})
	mask.SetRGBA(windowW/2, windowH/2, color.RGBA{R: 255, G: 255, B: 255, A: 255})

	got := buildMaskedCover(cover, mask)

	_, _, _, a := got.At(0, 0).RGBA()
	if a != 0 {
		t.Fatalf("transparent mask area should hide cover, got alpha=%d", a>>8)
	}

	r, g, b, a := got.At(windowW/2, windowH/2).RGBA()
	if r>>8 != 200 || g>>8 != 150 || b>>8 != 100 || a>>8 != 255 {
		t.Fatalf("opaque mask area should show cover, got rgba=(%d,%d,%d,%d)", r>>8, g>>8, b>>8, a>>8)
	}
}

func TestBuildMaskedCoverUsesMaskLumaAndAlpha(t *testing.T) {
	cover := image.NewRGBA(image.Rect(0, 0, windowW, windowH))
	cover.SetRGBA(0, 0, color.RGBA{R: 255, G: 200, B: 100, A: 255})
	cover.SetRGBA(1, 0, color.RGBA{R: 255, G: 200, B: 100, A: 255})
	cover.SetRGBA(2, 0, color.RGBA{R: 255, G: 200, B: 100, A: 255})

	mask := image.NewRGBA(image.Rect(0, 0, windowW, windowH))
	mask.SetRGBA(0, 0, color.RGBA{R: 255, G: 255, B: 255, A: 255})
	mask.SetRGBA(1, 0, color.RGBA{R: 128, G: 128, B: 128, A: 255})
	mask.SetRGBA(2, 0, color.RGBA{R: 255, G: 255, B: 255, A: 128})

	got := buildMaskedCover(cover, mask)

	_, _, _, a0 := got.At(0, 0).RGBA()
	_, _, _, a1 := got.At(1, 0).RGBA()
	_, _, _, a2 := got.At(2, 0).RGBA()

	if a0>>8 != 255 {
		t.Fatalf("white opaque mask should keep full alpha, got %d", a0>>8)
	}
	if a1>>8 < 126 || a1>>8 > 129 {
		t.Fatalf("mid-gray opaque mask should halve alpha, got %d", a1>>8)
	}
	if a2>>8 < 126 || a2>>8 > 129 {
		t.Fatalf("white half-alpha mask should halve alpha, got %d", a2>>8)
	}
}

func TestStatusIndicatorSelectsExpectedImage(t *testing.T) {
	playing := image.NewRGBA(image.Rect(0, 0, 1, 1))
	paused := image.NewRGBA(image.Rect(0, 0, 1, 1))
	app := &App{imgPlaying: playing, imgPaused: paused}

	if got := app.statusIndicator(false); got != playing {
		t.Fatal("statusIndicator(false) should return playing image")
	}
	if got := app.statusIndicator(true); got != paused {
		t.Fatal("statusIndicator(true) should return paused image")
	}
}

func TestWaitingExitHintText(t *testing.T) {
	if got := waitingExitHintText(); got != "EXIT [B]" {
		t.Fatalf("waitingExitHintText() = %q, want %q", got, "EXIT [B]")
	}
}

func TestTapeAAndCoverWindowAlign(t *testing.T) {
	if windowX != coverX || windowY != coverY {
		t.Fatalf("tapeA window (%d,%d) should align with cover (%d,%d)", windowX, windowY, coverX, coverY)
	}
}

func colorRGBA(r, g, b, a uint8) color.RGBA {
	return color.RGBA{R: r, G: g, B: b, A: a}
}
