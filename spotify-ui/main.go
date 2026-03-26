package main

import (
	"encoding/binary"
	"encoding/json"
	"fmt"
	"image"
	"image/color"
	"image/draw"
	_ "image/jpeg"
	_ "image/png"
	"io"
	"log"
	"math"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"
	"unsafe"

	"github.com/gorilla/websocket"
	"golang.org/x/image/font"
	"golang.org/x/image/font/opentype"
	"golang.org/x/image/math/fixed"
)

const (
	screenW = 1024
	screenH = 768
	bpp     = 4 // 32-bit BGRA
	fbSize  = screenW * screenH * bpp

	animFPS                = 30
	rotationFrameCount     = 60
	taperollFrameCount     = 12
	taperollSizeStep       = 24
	wheelRotationPeriod    = 2 * time.Second
	soundwaveTargetRefresh = 66 * time.Millisecond
	soundwaveEase          = 0.35
	soundwaveIdleEase      = 0.20
	soundwaveMinHeight     = 8
	soundwaveMaxHeight     = 36

	tapeBaseX        = 16
	tapeBaseY        = 28
	windowX          = 68
	windowY          = 68
	coverX           = 68
	coverY           = 68
	windowW          = 888
	windowH          = 384
	coverMaskOffsetY = 0

	leftRollCenterX  = 308
	rightRollCenterX = 716
	rollCenterY      = 292
	leftRollMinSize  = 200
	leftRollMaxSize  = 432
	rightRollMinSize = 200
	rightRollMaxSize = 432

	leftWheelX  = 248
	leftWheelY  = 232
	rightWheelX = 656
	rightWheelY = 232

	statusDotX      = 28
	statusDotY      = 636
	statusTextX     = 68
	statusBaselineY = 677
	statusLampX     = 11
	statusLampY     = 637
	soundwaveX      = 372
	soundwaveY      = 656
	hintsBaselineY  = 736

	apiBase = "http://127.0.0.1:3678"

	// Input event constants
	evKey    = 0x01
	evAbs    = 0x03
	btnA     = 305 // A button (BTN_EAST on this device)
	btnB     = 304 // B button (BTN_SOUTH on this device)
	absHat0X = 0x10
	absHat0Y = 0x11
	btnStart = 315
	keyMenu  = 139
)

// PlayerStatus from go-librespot /status API
type PlayerStatus struct {
	Username    string `json:"username"`
	DeviceName  string `json:"device_name"`
	Stopped     bool   `json:"stopped"`
	Paused      bool   `json:"paused"`
	Buffering   bool   `json:"buffering"`
	Volume      int    `json:"volume"`
	VolumeSteps int    `json:"volume_steps"`
	Track       *Track `json:"track"`
}

type Track struct {
	URI           string   `json:"uri"`
	Name          string   `json:"name"`
	ArtistNames   []string `json:"artist_names"`
	AlbumName     string   `json:"album_name"`
	AlbumCoverURL string   `json:"album_cover_url"`
	Duration      int64    `json:"duration"`
	Position      int64    `json:"position"`
}

type WSEvent struct {
	Type string          `json:"type"`
	Data json.RawMessage `json:"data"`
}

type MetadataEvent struct {
	URI           string   `json:"uri"`
	Name          string   `json:"name"`
	ArtistNames   []string `json:"artist_names"`
	AlbumName     string   `json:"album_name"`
	AlbumCoverURL string   `json:"album_cover_url"`
	Position      int64    `json:"position"`
	Duration      int64    `json:"duration"`
}

type VolumeEvent struct {
	Value int `json:"value"`
	Max   int `json:"max"`
}

type InputEvent struct {
	Time  syscall.Timeval
	Type  uint16
	Code  uint16
	Value int32
}

// HTTP client with timeout — prevents goroutine leaks from hung requests
var httpClient = &http.Client{Timeout: 5 * time.Second}

// App holds all application state
type App struct {
	fb      []byte // mmap'd framebuffer (visible)
	backBuf []byte // 1024x768 off-screen buffer
	fbFile  *os.File

	fontMono7 font.Face
	fontMono5 font.Face

	imgTapeBase        image.Image
	imgTaperoll        image.Image
	imgTapeA           image.Image
	imgCoverMask       image.Image
	imgWheel           image.Image
	imgPlaying         image.Image
	imgPaused          image.Image
	sceneBase          []byte
	scenePlaying       []byte
	sceneWaiting       []byte
	sceneForeground    *image.RGBA
	sceneCover         *image.RGBA
	overlayWindow      image.Image
	wheelFrames        []*image.RGBA
	taperollFrameCache map[int][]*image.RGBA
	fullRedraw         bool

	mu            sync.Mutex // protects state fields below
	trackName     string
	artistName    string
	albumName     string
	coverImg      image.Image
	paused        bool
	volume        int
	volumeMax     int
	connected     bool
	position      int64
	duration      int64
	lastPosTime   time.Time
	lastAction    time.Time // debounce for A button
	wheelAngle    float64
	soundwaveBars [24]float64
	soundwaveGoal [24]float64

	renderMu sync.Mutex // serializes render() calls
	quit     chan struct{}
	quitOnce sync.Once
}

func main() {
	app := &App{
		quit:      make(chan struct{}),
		volume:    80,
		volumeMax: 100,
	}
	app.resetSoundwaveIdle()

	if err := app.initFramebuffer(); err != nil {
		log.Fatalf("framebuffer init: %v", err)
	}
	defer app.fbFile.Close()

	if err := app.initFonts(); err != nil {
		log.Fatalf("font init: %v", err)
	}

	app.initAssets()

	// Draw initial screen
	app.render()

	// Start input handler
	go func() {
		defer func() {
			if r := recover(); r != nil {
				log.Printf("FATAL: handleInput panic: %v", r)
			}
		}()
		app.handleInput()
	}()

	// Start WebSocket listener
	go func() {
		defer func() {
			if r := recover(); r != nil {
				log.Printf("FATAL: listenEvents panic: %v", r)
			}
		}()
		app.listenEvents()
	}()

	// Start render loop (for progress bar updates)
	go func() {
		defer func() {
			if r := recover(); r != nil {
				log.Printf("FATAL: renderLoop panic: %v", r)
			}
		}()
		app.renderLoop()
	}()

	// Wait for quit signal
	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)

	select {
	case <-sigCh:
	case <-app.quit:
	}

	// Clear screen on exit
	for i := range app.backBuf {
		app.backBuf[i] = 0
	}
	app.swapBuffers()

	log.Println("exiting")
}

func (app *App) initFramebuffer() error {
	f, err := os.OpenFile("/dev/fb0", os.O_RDWR, 0)
	if err != nil {
		return fmt.Errorf("open /dev/fb0: %w", err)
	}
	app.fbFile = f

	data, err := syscall.Mmap(int(f.Fd()), 0, fbSize, syscall.PROT_READ|syscall.PROT_WRITE, syscall.MAP_SHARED)
	if err != nil {
		return fmt.Errorf("mmap: %w", err)
	}
	app.fb = data

	// Allocate render and output buffers for double buffering.
	app.backBuf = make([]byte, fbSize)
	return nil
}

// swapBuffers copies the back buffer to the visible framebuffer in one shot
func (app *App) swapBuffers() {
	copy(app.fb, app.backBuf)
}

func (app *App) initFonts() error {
	fontPaths := []string{
		"resources/font_mono.ttf",
		"resources/font.ttf",
		"../package/SpotifyConnect/resources/font_mono.ttf",
		"../package/SpotifyConnect/resources/font.ttf",
		"/usr/trimui/res/font/CJKFont.ttf",
		"/usr/trimui/apps/bookreader/regular.ttf",
	}

	var fontData []byte
	var err error
	for _, p := range fontPaths {
		fontData, err = os.ReadFile(p)
		if err == nil {
			log.Printf("using font: %s", p)
			break
		}
	}
	if fontData == nil {
		return fmt.Errorf("no font found, tried: %v", fontPaths)
	}

	parsedFont, err := opentype.Parse(fontData)
	if err != nil {
		return fmt.Errorf("parse font: %w", err)
	}

	app.fontMono7, err = opentype.NewFace(parsedFont, &opentype.FaceOptions{
		Size: 28, DPI: 72, Hinting: font.HintingFull,
	})
	if err != nil {
		return err
	}

	app.fontMono5, err = opentype.NewFace(parsedFont, &opentype.FaceOptions{
		Size: 20, DPI: 72, Hinting: font.HintingFull,
	})
	return err
}

func (app *App) initAssets() {
	app.imgTapeBase = loadImageResource("tapeBase.png")
	app.imgTaperoll = loadImageResource("taperoll.png")
	app.imgTapeA = loadImageResource("tapeA.png")
	app.imgCoverMask = loadImageResource("cover_mask.png")
	app.imgWheel = loadImageResource("wheel.png")
	app.imgPlaying = loadImageResource("playing.png")
	app.imgPaused = loadImageResource("paused.png")
	app.initRenderCaches()
}

func loadImageResource(name string) image.Image {
	for _, path := range resourceCandidates(name) {
		f, err := os.Open(path)
		if err != nil {
			continue
		}
		img, _, err := image.Decode(f)
		f.Close()
		if err != nil {
			log.Printf("decode %s failed: %v", path, err)
			return nil
		}
		log.Printf("using image resource: %s", path)
		return img
	}
	log.Printf("image resource not found: %s", name)
	return nil
}

func resourceCandidates(name string) []string {
	seen := map[string]struct{}{}
	var paths []string
	add := func(path string) {
		if path == "" {
			return
		}
		cleaned := filepath.Clean(path)
		if _, ok := seen[cleaned]; ok {
			return
		}
		seen[cleaned] = struct{}{}
		paths = append(paths, cleaned)
	}

	add(filepath.Join("resources", name))
	add(filepath.Join("package", "SpotifyConnect", "resources", name))
	add(filepath.Join("..", "package", "SpotifyConnect", "resources", name))

	if exe, err := os.Executable(); err == nil {
		add(filepath.Join(filepath.Dir(exe), "resources", name))
	}

	return paths
}

func (app *App) initRenderCaches() {
	app.renderMu.Lock()
	defer app.renderMu.Unlock()

	app.sceneBase = make([]byte, fbSize)
	app.scenePlaying = make([]byte, fbSize)
	app.sceneWaiting = make([]byte, fbSize)
	app.overlayWindow = buildOverlayWindow(app.imgTapeA, app.imgCoverMask)
	app.wheelFrames = buildRotatedFrames(app.imgWheel, rotationFrameCount)
	app.taperollFrameCache = buildTaperollFrameCache(app.imgTaperoll, taperollFrameCount)
	app.sceneForeground = app.buildCassetteForeground()
	app.fullRedraw = true

	app.rebuildBaseSceneLocked()
	app.rebuildPlayingSceneLocked(nil)
	app.rebuildWaitingSceneLocked()
}

func (app *App) withTargetBuffer(buf []byte, fn func()) {
	original := app.backBuf
	app.backBuf = buf
	fn()
	app.backBuf = original
}

func clearBuffer(buf []byte, c color.RGBA) {
	for i := 0; i < len(buf); i += bpp {
		buf[i+0] = c.B
		buf[i+1] = c.G
		buf[i+2] = c.R
		buf[i+3] = c.A
	}
}

func buildOverlayWindow(tapeA image.Image, coverMask image.Image) image.Image {
	dst := image.NewRGBA(image.Rect(0, 0, windowW, windowH))
	if tapeA != nil {
		draw.Draw(dst, tapeA.Bounds(), tapeA, tapeA.Bounds().Min, draw.Over)
	}
	return dst
}

func buildMaskedCover(img image.Image, mask image.Image) *image.RGBA {
	if img == nil {
		return nil
	}
	bounds := img.Bounds()
	srcW := float64(bounds.Dx())
	srcH := float64(bounds.Dy())
	if srcW == 0 || srcH == 0 {
		return nil
	}

	dst := image.NewRGBA(image.Rect(0, 0, windowW, windowH))
	scale := math.Max(float64(windowW)/srcW, float64(windowH)/srcH)
	cropW := float64(windowW) / scale
	cropH := float64(windowH) / scale
	srcX0 := float64(bounds.Min.X) + (srcW-cropW)/2
	srcY0 := float64(bounds.Min.Y) + (srcH-cropH)/2

	var maskBounds image.Rectangle
	if mask != nil {
		maskBounds = mask.Bounds()
	}

	for dy := 0; dy < windowH; dy++ {
		srcY := srcY0 + (float64(dy)+0.5)*cropH/float64(windowH)
		srcYi := clampInt(int(srcY), bounds.Min.Y, bounds.Max.Y-1)
		for dx := 0; dx < windowW; dx++ {
			srcX := srcX0 + (float64(dx)+0.5)*cropW/float64(windowW)
			srcXi := clampInt(int(srcX), bounds.Min.X, bounds.Max.X-1)
			r, g, b, a := rgba8(img.At(srcXi, srcYi))
			if mask != nil {
				mx := maskBounds.Min.X + dx*maskBounds.Dx()/windowW
				my := maskBounds.Min.Y + dy*maskBounds.Dy()/windowH
				mr, mg, mb, ma := rgba8(mask.At(mx, my))
				luma := (299*int(mr) + 587*int(mg) + 114*int(mb)) / 1000
				maskValue := luma * int(ma) / 255
				a = uint8(int(a) * maskValue / 255)
			}
			dst.SetRGBA(dx, dy, color.RGBA{R: r, G: g, B: b, A: a})
		}
	}
	return dst
}

func (app *App) buildCassetteForeground() *image.RGBA {
	dst := image.NewRGBA(image.Rect(0, 0, 992, 584))
	if app.imgTapeBase != nil {
		draw.Draw(dst, app.imgTapeBase.Bounds(), app.imgTapeBase, app.imgTapeBase.Bounds().Min, draw.Over)
	}
	if app.overlayWindow != nil {
		overlayPos := image.Pt(windowX-tapeBaseX, windowY-tapeBaseY)
		draw.Draw(dst, app.overlayWindow.Bounds().Add(overlayPos), app.overlayWindow, app.overlayWindow.Bounds().Min, draw.Over)
	}
	return dst
}

func splitOverlayWindow(img image.Image) (image.Image, image.Image) {
	if img == nil {
		return nil, nil
	}
	rect := img.Bounds()
	mid := rect.Min.X + rect.Dx()/2
	type subImager interface {
		SubImage(r image.Rectangle) image.Image
	}
	if sub, ok := img.(subImager); ok {
		return sub.SubImage(image.Rect(rect.Min.X, rect.Min.Y, mid, rect.Max.Y)), sub.SubImage(image.Rect(mid, rect.Min.Y, rect.Max.X, rect.Max.Y))
	}
	return img, img
}

func buildRotatedFrames(img image.Image, count int) []*image.RGBA {
	if img == nil || count <= 0 {
		return nil
	}
	frames := make([]*image.RGBA, count)
	for i := 0; i < count; i++ {
		angle := 2 * math.Pi * float64(i) / float64(count)
		frames[i] = rotateImage(img, angle)
	}
	return frames
}

func scaleImageNearest(img image.Image, size int) *image.RGBA {
	if img == nil || size <= 0 {
		return nil
	}
	bounds := img.Bounds()
	dst := image.NewRGBA(image.Rect(0, 0, size, size))
	srcW := bounds.Dx()
	srcH := bounds.Dy()
	for dy := 0; dy < size; dy++ {
		srcY := bounds.Min.Y + dy*srcH/size
		for dx := 0; dx < size; dx++ {
			srcX := bounds.Min.X + dx*srcW/size
			dst.Set(dx, dy, img.At(srcX, srcY))
		}
	}
	return dst
}

func buildScaledRotatedFrames(img image.Image, size, count int) []*image.RGBA {
	scaled := scaleImageNearest(img, size)
	if scaled == nil {
		return nil
	}
	return buildRotatedFrames(scaled, count)
}

func quantizeRollSize(size int) int {
	size = clampInt(size, leftRollMinSize, leftRollMaxSize)
	if size == leftRollMaxSize {
		return leftRollMaxSize
	}
	steps := int(math.Round(float64(size-leftRollMinSize) / float64(taperollSizeStep)))
	quantized := leftRollMinSize + steps*taperollSizeStep
	if quantized > leftRollMaxSize {
		return leftRollMaxSize
	}
	return quantized
}

func rollCacheSizes() []int {
	var sizes []int
	for size := leftRollMinSize; size < leftRollMaxSize; size += taperollSizeStep {
		sizes = append(sizes, size)
	}
	sizes = append(sizes, leftRollMaxSize)
	return sizes
}

func buildTaperollFrameCache(img image.Image, frameCount int) map[int][]*image.RGBA {
	cache := make(map[int][]*image.RGBA)
	if img == nil || frameCount <= 0 {
		return cache
	}
	for _, size := range rollCacheSizes() {
		cache[size] = buildScaledRotatedFrames(img, size, frameCount)
	}
	return cache
}

func copyRect(dst, src []byte, rect image.Rectangle) {
	if len(dst) < fbSize || len(src) < fbSize {
		return
	}
	if rect.Min.X < 0 {
		rect.Min.X = 0
	}
	if rect.Min.Y < 0 {
		rect.Min.Y = 0
	}
	if rect.Max.X > screenW {
		rect.Max.X = screenW
	}
	if rect.Max.Y > screenH {
		rect.Max.Y = screenH
	}
	rowBytes := rect.Dx() * bpp
	for y := rect.Min.Y; y < rect.Max.Y; y++ {
		start := (y*screenW + rect.Min.X) * bpp
		copy(dst[start:start+rowBytes], src[start:start+rowBytes])
	}
}

func rotateImage(img image.Image, angle float64) *image.RGBA {
	bounds := img.Bounds()
	dst := image.NewRGBA(image.Rect(0, 0, bounds.Dx(), bounds.Dy()))
	cx := float64(bounds.Dx()-1) / 2
	cy := float64(bounds.Dy()-1) / 2
	sinA := math.Sin(angle)
	cosA := math.Cos(angle)

	for dy := 0; dy < bounds.Dy(); dy++ {
		for dx := 0; dx < bounds.Dx(); dx++ {
			relX := float64(dx) - cx
			relY := float64(dy) - cy
			srcX := cosA*relX + sinA*relY + cx
			srcY := -sinA*relX + cosA*relY + cy
			srcXi := int(math.Round(srcX))
			srcYi := int(math.Round(srcY))
			if srcXi < 0 || srcXi >= bounds.Dx() || srcYi < 0 || srcYi >= bounds.Dy() {
				continue
			}
			dst.Set(dx, dy, img.At(bounds.Min.X+srcXi, bounds.Min.Y+srcYi))
		}
	}
	return dst
}

func rollSizesForProgress(progress float64) (int, int) {
	if progress < 0 {
		progress = 0
	}
	if progress > 1 {
		progress = 1
	}
	left := leftRollMinSize + int(math.Round(float64(leftRollMaxSize-leftRollMinSize)*progress))
	right := rightRollMaxSize - int(math.Round(float64(rightRollMaxSize-rightRollMinSize)*progress))
	return left, right
}

func frameIndexForAngle(angle float64, count int) int {
	if count <= 0 {
		return 0
	}
	turn := math.Mod(angle, 2*math.Pi)
	if turn < 0 {
		turn += 2 * math.Pi
	}
	return int(math.Round(turn/(2*math.Pi)*float64(count))) % count
}

func (app *App) taperollFramesForSize(size int) []*image.RGBA {
	if len(app.taperollFrameCache) == 0 {
		return nil
	}
	return app.taperollFrameCache[quantizeRollSize(size)]
}

func (app *App) rebuildBaseSceneLocked() {
	clearBuffer(app.sceneBase, color.RGBA{0, 0, 0, 255})
	app.withTargetBuffer(app.sceneBase, func() {
		hintColor := color.RGBA{R: 0x3D, G: 0x3D, B: 0x3D, A: 0xFF}
		hintLabels := []string{
			"PREV [←]",
			"NEXT [→]",
			"VOL+ [↑]",
			"VOL- [↓]",
			"PLAY / PAUSE [A]",
			"EXIT [B]",
		}
		totalWidth := 0
		for _, label := range hintLabels {
			totalWidth += measureText(app.fontMono5, label)
		}
		startX := 28
		gap := 4
		if len(hintLabels) > 1 {
			available := (screenW - 56) - totalWidth
			if available > 0 {
				gap = available / (len(hintLabels) - 1)
			}
		}
		x := startX
		for _, label := range hintLabels {
			app.drawText(app.fontMono5, label, x, hintsBaselineY, hintColor)
			x += measureText(app.fontMono5, label) + gap
		}
	})
}

func (app *App) rebuildPlayingSceneLocked(cover image.Image) {
	copy(app.scenePlaying, app.sceneBase)
	app.sceneCover = buildMaskedCover(cover, app.imgCoverMask)
	app.fullRedraw = true
}

func (app *App) rebuildWaitingSceneLocked() {
	copy(app.sceneWaiting, app.sceneBase)
	app.withTargetBuffer(app.sceneWaiting, func() {
		white := color.RGBA{R: 255, G: 255, B: 255, A: 255}
		black := color.RGBA{R: 0, G: 0, B: 0, A: 255}
		if leftFrames := app.taperollFramesForSize(leftRollMinSize); len(leftFrames) > 0 {
			app.drawImageAlpha(leftFrames[0], leftRollCenterX-leftRollMinSize/2, rollCenterY-leftRollMinSize/2)
		}
		if rightFrames := app.taperollFramesForSize(rightRollMaxSize); len(rightFrames) > 0 {
			app.drawImageAlpha(rightFrames[0], rightRollCenterX-rightRollMaxSize/2, rollCenterY-rightRollMaxSize/2)
		}
		if app.sceneForeground != nil {
			app.drawImageAlpha(app.sceneForeground, tapeBaseX, tapeBaseY)
		}
		if len(app.wheelFrames) > 0 {
			app.drawImageAlpha(app.wheelFrames[0], leftWheelX, leftWheelY)
			app.drawImageAlpha(app.wheelFrames[0], rightWheelX, rightWheelY)
		}
		msg := "Waiting for Spotify Connect..."
		exitHint := waitingExitHintText()
		app.fillRect(0, hintsBaselineY-28, screenW, 48, black)
		app.drawText(app.fontMono7, msg, screenW/2-measureText(app.fontMono7, msg)/2, statusBaselineY, white)
		app.drawText(app.fontMono5, exitHint, screenW/2-measureText(app.fontMono5, exitHint)/2, hintsBaselineY, white)
	})
	app.fullRedraw = true
}

func (app *App) rebuildPlayingScene(cover image.Image) {
	app.renderMu.Lock()
	defer app.renderMu.Unlock()
	app.rebuildPlayingSceneLocked(cover)
}

// --- Drawing primitives (all write to backBuf, NOT fb) ---

func (app *App) fillRect(x, y, w, h int, c color.Color) {
	r, g, b, a := rgba8(c)
	for dy := 0; dy < h; dy++ {
		py := y + dy
		if py < 0 || py >= screenH {
			continue
		}
		for dx := 0; dx < w; dx++ {
			px := x + dx
			if px < 0 || px >= screenW {
				continue
			}
			app.setBackPixel(px, py, r, g, b, a)
		}
	}
}

func (app *App) drawImage(img image.Image, x, y int) {
	if img == nil {
		return
	}
	bounds := img.Bounds()
	for dy := 0; dy < bounds.Dy(); dy++ {
		py := y + dy
		if py < 0 || py >= screenH {
			continue
		}
		for dx := 0; dx < bounds.Dx(); dx++ {
			px := x + dx
			if px < 0 || px >= screenW {
				continue
			}
			r, g, b, a := rgba8(img.At(bounds.Min.X+dx, bounds.Min.Y+dy))
			app.setBackPixel(px, py, r, g, b, a)
		}
	}
}

func (app *App) drawImageAlpha(img image.Image, x, y int) {
	if img == nil {
		return
	}
	if rgba, ok := img.(*image.RGBA); ok {
		app.drawRGBAAlpha(rgba, x, y)
		return
	}
	bounds := img.Bounds()
	for dy := 0; dy < bounds.Dy(); dy++ {
		py := y + dy
		if py < 0 || py >= screenH {
			continue
		}
		for dx := 0; dx < bounds.Dx(); dx++ {
			px := x + dx
			if px < 0 || px >= screenW {
				continue
			}
			r, g, b, a := rgba8(img.At(bounds.Min.X+dx, bounds.Min.Y+dy))
			app.blendBackPixel(px, py, r, g, b, a)
		}
	}
}

func (app *App) drawRGBAAlpha(img *image.RGBA, x, y int) {
	if img == nil {
		return
	}
	bounds := img.Bounds()
	startX := x
	startY := y
	srcX0 := 0
	srcY0 := 0
	width := bounds.Dx()
	height := bounds.Dy()

	if startX < 0 {
		srcX0 = -startX
		width += startX
		startX = 0
	}
	if startY < 0 {
		srcY0 = -startY
		height += startY
		startY = 0
	}
	if startX+width > screenW {
		width = screenW - startX
	}
	if startY+height > screenH {
		height = screenH - startY
	}
	if width <= 0 || height <= 0 {
		return
	}

	for row := 0; row < height; row++ {
		srcOff := img.PixOffset(bounds.Min.X+srcX0, bounds.Min.Y+srcY0+row)
		dstOff := ((startY+row)*screenW + startX) * bpp
		for col := 0; col < width; col++ {
			si := srcOff + col*4
			sa := img.Pix[si+3]
			if sa == 0 {
				dstOff += 4
				continue
			}

			sr := int(img.Pix[si+0])
			sg := int(img.Pix[si+1])
			sb := int(img.Pix[si+2])
			if sa == 255 {
				app.backBuf[dstOff+0] = byte(sb)
				app.backBuf[dstOff+1] = byte(sg)
				app.backBuf[dstOff+2] = byte(sr)
				app.backBuf[dstOff+3] = 255
				dstOff += 4
				continue
			}

			a := int(sa)
			inv := 255 - a
			db := int(app.backBuf[dstOff+0])
			dg := int(app.backBuf[dstOff+1])
			dr := int(app.backBuf[dstOff+2])
			app.backBuf[dstOff+0] = byte((sb*a + db*inv) / 255)
			app.backBuf[dstOff+1] = byte((sg*a + dg*inv) / 255)
			app.backBuf[dstOff+2] = byte((sr*a + dr*inv) / 255)
			app.backBuf[dstOff+3] = 255
			dstOff += 4
		}
	}
}

func (app *App) drawImageScaled(img image.Image, centerX, centerY, size int) {
	if img == nil || size <= 0 {
		return
	}
	bounds := img.Bounds()
	startX := centerX - size/2
	startY := centerY - size/2
	srcW := bounds.Dx()
	srcH := bounds.Dy()

	for dy := 0; dy < size; dy++ {
		py := startY + dy
		if py < 0 || py >= screenH {
			continue
		}
		srcY := bounds.Min.Y + dy*srcH/size
		for dx := 0; dx < size; dx++ {
			px := startX + dx
			if px < 0 || px >= screenW {
				continue
			}
			srcX := bounds.Min.X + dx*srcW/size
			r, g, b, a := rgba8(img.At(srcX, srcY))
			app.blendBackPixel(px, py, r, g, b, a)
		}
	}
}

func (app *App) drawImageCrop(img image.Image, x, y, w, h int, mask image.Image) {
	if img == nil || w <= 0 || h <= 0 {
		return
	}
	bounds := img.Bounds()
	srcW := float64(bounds.Dx())
	srcH := float64(bounds.Dy())
	if srcW == 0 || srcH == 0 {
		return
	}

	scale := math.Max(float64(w)/srcW, float64(h)/srcH)
	cropW := float64(w) / scale
	cropH := float64(h) / scale
	srcX0 := float64(bounds.Min.X) + (srcW-cropW)/2
	srcY0 := float64(bounds.Min.Y) + (srcH-cropH)/2

	var maskBounds image.Rectangle
	if mask != nil {
		maskBounds = mask.Bounds()
	}

	for dy := 0; dy < h; dy++ {
		py := y + dy
		if py < 0 || py >= screenH {
			continue
		}
		srcY := srcY0 + (float64(dy)+0.5)*cropH/float64(h)
		srcYi := clampInt(int(srcY), bounds.Min.Y, bounds.Max.Y-1)
		for dx := 0; dx < w; dx++ {
			px := x + dx
			if px < 0 || px >= screenW {
				continue
			}
			srcX := srcX0 + (float64(dx)+0.5)*cropW/float64(w)
			srcXi := clampInt(int(srcX), bounds.Min.X, bounds.Max.X-1)
			r, g, b, a := rgba8(img.At(srcXi, srcYi))
			if mask != nil {
				mx := maskBounds.Min.X + dx*maskBounds.Dx()/w
				my := maskBounds.Min.Y + dy*maskBounds.Dy()/h
				_, _, _, ma := mask.At(mx, my).RGBA()
				a = byte(uint16(a) * uint16(ma>>8) / 255)
			}
			if a == 0 {
				continue
			}
			app.blendBackPixel(px, py, r, g, b, a)
		}
	}
}

func (app *App) drawImageRotated(img image.Image, x, y int, angle float64) {
	if img == nil {
		return
	}
	bounds := img.Bounds()
	w := bounds.Dx()
	h := bounds.Dy()
	cx := float64(w-1) / 2
	cy := float64(h-1) / 2
	sinA := math.Sin(angle)
	cosA := math.Cos(angle)

	for dy := 0; dy < h; dy++ {
		py := y + dy
		if py < 0 || py >= screenH {
			continue
		}
		for dx := 0; dx < w; dx++ {
			px := x + dx
			if px < 0 || px >= screenW {
				continue
			}
			relX := float64(dx) - cx
			relY := float64(dy) - cy
			srcX := cosA*relX + sinA*relY + cx
			srcY := -sinA*relX + cosA*relY + cy
			srcXi := int(math.Round(srcX))
			srcYi := int(math.Round(srcY))
			if srcXi < 0 || srcXi >= w || srcYi < 0 || srcYi >= h {
				continue
			}
			r, g, b, a := rgba8(img.At(bounds.Min.X+srcXi, bounds.Min.Y+srcYi))
			app.blendBackPixel(px, py, r, g, b, a)
		}
	}
}

func (app *App) drawSoundwave(x, y int, bars [24]float64, active bool) {
	waveColor := color.RGBA{255, 255, 255, 255}
	if !active {
		waveColor = color.RGBA{255, 255, 255, 160}
	}
	for i, height := range bars {
		if height < soundwaveMinHeight {
			height = soundwaveMinHeight
		}
		barHeight := int(math.Round(height))
		app.fillRect(x+i*12, y-barHeight, 4, barHeight, waveColor)
	}
}

func (app *App) drawStatusDot(x, y int) {
	app.fillRect(x-8, y-8, 40, 40, color.RGBA{255, 40, 40, 64})
	app.fillRect(x, y, 24, 24, color.RGBA{255, 40, 40, 255})
}

func (app *App) statusIndicator(paused bool) image.Image {
	if paused {
		return app.imgPaused
	}
	return app.imgPlaying
}

func waitingExitHintText() string {
	return "EXIT [B]"
}

// backBufImage wraps the back buffer as draw.Image for font rendering.
func (app *App) backBufImage() *FBImage {
	return &FBImage{buf: app.backBuf, w: screenW, h: screenH}
}

type FBImage struct {
	buf []byte
	w   int
	h   int
}

func (f *FBImage) ColorModel() color.Model { return color.RGBAModel }
func (f *FBImage) Bounds() image.Rectangle {
	return image.Rect(0, 0, f.w, f.h)
}
func (f *FBImage) At(x, y int) color.Color {
	if x < 0 || x >= f.w || y < 0 || y >= f.h {
		return color.RGBA{}
	}
	offset := (y*f.w + x) * bpp
	return color.RGBA{
		R: f.buf[offset+2], G: f.buf[offset+1],
		B: f.buf[offset+0], A: f.buf[offset+3],
	}
}
func (f *FBImage) Set(x, y int, c color.Color) {
	if x < 0 || x >= f.w || y < 0 || y >= f.h {
		return
	}
	r, g, b, a := c.RGBA()
	offset := (y*f.w + x) * bpp
	f.buf[offset+0] = byte(b >> 8)
	f.buf[offset+1] = byte(g >> 8)
	f.buf[offset+2] = byte(r >> 8)
	f.buf[offset+3] = byte(a >> 8)
}

var _ draw.Image = (*FBImage)(nil)

func (app *App) drawText(face font.Face, text string, x, y int, c color.Color) {
	d := &font.Drawer{
		Dst:  app.backBufImage(),
		Src:  image.NewUniform(c),
		Face: face,
		Dot:  fixed.P(x, y),
	}
	d.DrawString(text)
}

func rgba8(c color.Color) (uint8, uint8, uint8, uint8) {
	r, g, b, a := c.RGBA()
	return byte(r >> 8), byte(g >> 8), byte(b >> 8), byte(a >> 8)
}

func (app *App) setBackPixel(x, y int, r, g, b, a uint8) {
	if x < 0 || x >= screenW || y < 0 || y >= screenH {
		return
	}
	offset := (y*screenW + x) * bpp
	app.backBuf[offset+0] = b
	app.backBuf[offset+1] = g
	app.backBuf[offset+2] = r
	app.backBuf[offset+3] = a
}

func (app *App) blendBackPixel(x, y int, r, g, b, a uint8) {
	if a == 0 || x < 0 || x >= screenW || y < 0 || y >= screenH {
		return
	}
	offset := (y*screenW + x) * bpp
	dr := int(app.backBuf[offset+2])
	dg := int(app.backBuf[offset+1])
	db := int(app.backBuf[offset+0])
	da := int(app.backBuf[offset+3])
	sa := int(a)
	outA := sa + da*(255-sa)/255
	if outA == 0 {
		return
	}
	outR := (int(r)*sa + dr*da*(255-sa)/255) / outA
	outG := (int(g)*sa + dg*da*(255-sa)/255) / outA
	outB := (int(b)*sa + db*da*(255-sa)/255) / outA
	app.backBuf[offset+0] = byte(outB)
	app.backBuf[offset+1] = byte(outG)
	app.backBuf[offset+2] = byte(outR)
	app.backBuf[offset+3] = byte(outA)
}

func clampInt(v, minV, maxV int) int {
	if v < minV {
		return minV
	}
	if v > maxV {
		return maxV
	}
	return v
}

func (app *App) resetSoundwaveIdle() {
	for i := range app.soundwaveBars {
		app.soundwaveBars[i] = soundwaveMinHeight
		app.soundwaveGoal[i] = soundwaveMinHeight
	}
}

func (app *App) setSoundwaveIdleGoal() {
	for i := range app.soundwaveGoal {
		app.soundwaveGoal[i] = soundwaveMinHeight
	}
}

func (app *App) refreshSoundwaveGoal(now time.Time) {
	phaseBase := float64(now.UnixNano()) / float64(time.Second)
	for i := range app.soundwaveGoal {
		phase := phaseBase*3.2 + float64(i)*0.45
		value := 0.5 + 0.5*math.Sin(phase+0.8*math.Sin(phase*0.73))
		app.soundwaveGoal[i] = soundwaveMinHeight + value*float64(soundwaveMaxHeight-soundwaveMinHeight)
	}
}

func (app *App) stepSoundwave(ease float64) {
	for i := range app.soundwaveBars {
		app.soundwaveBars[i] += (app.soundwaveGoal[i] - app.soundwaveBars[i]) * ease
	}
}

// --- Render (draws to back buffer, then swaps) ---

func (app *App) render() {
	app.renderMu.Lock()
	defer app.renderMu.Unlock()

	app.mu.Lock()
	paused := app.paused
	connected := app.connected
	position := app.position
	duration := app.duration
	wheelAngle := app.wheelAngle
	soundwaveBars := app.soundwaveBars
	app.mu.Unlock()

	white := color.RGBA{R: 255, G: 255, B: 255, A: 255}

	if !connected {
		copy(app.backBuf, app.sceneWaiting)
		app.swapBuffers()
		app.fullRedraw = false
		return
	}

	leftRect := image.Rect(88, 64, 536, 520)
	rightRect := image.Rect(488, 64, 936, 520)
	infoRect := image.Rect(0, 620, screenW, 690)
	dirtyRects := []image.Rectangle{leftRect, rightRect, infoRect}

	if app.fullRedraw {
		copy(app.backBuf, app.scenePlaying)
	} else {
		for _, rect := range dirtyRects {
			copyRect(app.backBuf, app.scenePlaying, rect)
		}
	}

	progress := 0.0
	if duration > 0 {
		progress = float64(position) / float64(duration)
	}
	leftSize, rightSize := rollSizesForProgress(progress)
	wheelFrameIdx := frameIndexForAngle(wheelAngle, len(app.wheelFrames))
	leftRollFrames := app.taperollFramesForSize(leftSize)
	rightRollFrames := app.taperollFramesForSize(rightSize)
	rollFrameIdx := frameIndexForAngle(wheelAngle, taperollFrameCount)
	leftDrawSize := quantizeRollSize(leftSize)
	rightDrawSize := quantizeRollSize(rightSize)
	if len(leftRollFrames) > 0 {
		app.drawImageAlpha(leftRollFrames[rollFrameIdx%len(leftRollFrames)], leftRollCenterX-leftDrawSize/2, rollCenterY-leftDrawSize/2)
	}
	if len(rightRollFrames) > 0 {
		app.drawImageAlpha(rightRollFrames[rollFrameIdx%len(rightRollFrames)], rightRollCenterX-rightDrawSize/2, rollCenterY-rightDrawSize/2)
	}
	if app.sceneForeground != nil {
		app.drawImageAlpha(app.sceneForeground, tapeBaseX, tapeBaseY)
	}
	if app.sceneCover != nil {
		app.drawImageAlpha(app.sceneCover, coverX, coverY)
	}
	if len(app.wheelFrames) > 0 {
		app.drawImageAlpha(app.wheelFrames[wheelFrameIdx], leftWheelX, leftWheelY)
		app.drawImageAlpha(app.wheelFrames[wheelFrameIdx], rightWheelX, rightWheelY)
	}

	status := "PLAYING"
	if paused {
		status = "PAUSED"
	}
	if indicator := app.statusIndicator(paused); indicator != nil {
		app.drawImageAlpha(indicator, statusLampX, statusLampY)
	} else {
		app.drawStatusDot(statusDotX, statusDotY)
	}
	app.drawText(app.fontMono7, status, statusTextX, statusBaselineY, white)
	timeRemaining := formatDuration(duration - position)
	app.drawText(app.fontMono7, timeRemaining, screenW-28-measureText(app.fontMono7, timeRemaining), statusBaselineY, white)
	_ = soundwaveBars

	if app.fullRedraw {
		app.swapBuffers()
		app.fullRedraw = false
		return
	}
	for _, rect := range dirtyRects {
		copyRect(app.fb, app.backBuf, rect)
	}
}

func (app *App) renderLoop() {
	ticker := time.NewTicker(time.Second / animFPS)
	defer ticker.Stop()
	lastFrame := time.Now()

	for {
		select {
		case <-ticker.C:
			now := time.Now()
			dt := now.Sub(lastFrame)
			lastFrame = now
			app.mu.Lock()
			shouldRender := app.connected
			if app.connected && !app.paused {
				app.wheelAngle = math.Mod(app.wheelAngle+(2*math.Pi*dt.Seconds()/wheelRotationPeriod.Seconds()), 2*math.Pi)
			} else {
			}
			if app.connected && !app.paused && app.duration > 0 && !app.lastPosTime.IsZero() {
				app.position += dt.Milliseconds()
				if app.position > app.duration {
					app.position = app.duration
				}
				app.lastPosTime = now
			}
			app.mu.Unlock()

			if shouldRender {
				app.render()
			}
		case <-app.quit:
			return
		}
	}
}

func (app *App) handleInput() {
	inputPaths := []string{"/dev/input/event3", "/dev/input/event0"}

	var wg sync.WaitGroup
	for _, path := range inputPaths {
		wg.Add(1)
		go func(p string) {
			defer wg.Done()
			// Retry open up to 5 times
			var f *os.File
			for attempt := 0; attempt < 5; attempt++ {
				var err error
				f, err = os.Open(p)
				if err == nil {
					break
				}
				log.Printf("input open %s failed (attempt %d): %v", p, attempt+1, err)
				select {
				case <-app.quit:
					return
				case <-time.After(1 * time.Second):
				}
			}
			if f == nil {
				log.Printf("giving up on input device %s", p)
				return
			}
			defer f.Close()
			log.Printf("reading input from %s", p)
			app.readInputDevice(f)
		}(path)
	}
	wg.Wait()
}

func (app *App) readInputDevice(f *os.File) {
	defer func() {
		if r := recover(); r != nil {
			log.Printf("input reader panic: %v", r)
		}
	}()

	eventSize := int(unsafe.Sizeof(InputEvent{}))
	buf := make([]byte, eventSize)

	// Use a channel+goroutine to make reads interruptible by quit
	type readResult struct {
		err error
	}
	readCh := make(chan readResult, 1)

	for {
		// Launch blocking read in background
		go func() {
			_, err := io.ReadFull(f, buf)
			readCh <- readResult{err: err}
		}()

		// Wait for either read completion or quit
		select {
		case <-app.quit:
			return
		case res := <-readCh:
			if res.err != nil {
				log.Printf("input read error: %v", res.err)
				return
			}
		}

		ev := InputEvent{
			Type:  binary.LittleEndian.Uint16(buf[16:18]),
			Code:  binary.LittleEndian.Uint16(buf[18:20]),
			Value: int32(binary.LittleEndian.Uint32(buf[20:24])),
		}

		// B and MENU always exit, regardless of state
		if ev.Type == evKey && ev.Value == 1 {
			if ev.Code == btnB || ev.Code == keyMenu {
				log.Println("exit requested via button")
				app.quitOnce.Do(func() { close(app.quit) })
				return
			}
		}

		if ev.Type == evKey && ev.Value == 1 {
			switch ev.Code {
			case btnA:
				// Debounce: ignore if <500ms since last action
				app.mu.Lock()
				since := time.Since(app.lastAction)
				if since > 500*time.Millisecond {
					app.lastAction = time.Now()
					app.mu.Unlock()
					go app.togglePlayPause()
				} else {
					app.mu.Unlock()
					log.Printf("debounce: ignored A press (%dms)", since.Milliseconds())
				}
			}
		}

		if ev.Type == evAbs {
			switch ev.Code {
			case absHat0X:
				if ev.Value < 0 {
					go app.safeAPIPost("/player/prev")
				} else if ev.Value > 0 {
					go app.safeAPIPost("/player/next")
				}
			case absHat0Y:
				if ev.Value < 0 {
					go app.safeAPIPostVolume(5)
				} else if ev.Value > 0 {
					go app.safeAPIPostVolume(-5)
				}
			}
		}
	}
}

func (app *App) togglePlayPause() {
	defer func() {
		if r := recover(); r != nil {
			log.Printf("recovered panic in togglePlayPause: %v", r)
		}
	}()

	app.mu.Lock()
	isPaused := app.paused
	app.mu.Unlock()

	if isPaused {
		log.Println("action: resume")
		app.apiPost("/player/resume")
	} else {
		log.Println("action: pause")
		app.apiPost("/player/pause")
	}
}

func (app *App) safeAPIPost(path string) {
	defer func() {
		if r := recover(); r != nil {
			log.Printf("recovered panic in apiPost: %v", r)
		}
	}()
	app.apiPost(path)
}

func (app *App) safeAPIPostVolume(delta int) {
	defer func() {
		if r := recover(); r != nil {
			log.Printf("recovered panic in apiPostVolume: %v", r)
		}
	}()
	app.apiPostVolume(delta)
}

func (app *App) apiPost(path string) {
	req, err := http.NewRequest("POST", apiBase+path, nil)
	if err != nil {
		log.Printf("api request error: %v", err)
		return
	}
	resp, err := httpClient.Do(req)
	if err != nil {
		log.Printf("api error: %v", err)
		return
	}
	resp.Body.Close()
}

func (app *App) apiPostVolume(delta int) {
	body := strings.NewReader(fmt.Sprintf(`{"volume":%d,"relative":true}`, delta))
	req, err := http.NewRequest("POST", apiBase+"/player/volume", body)
	if err != nil {
		return
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := httpClient.Do(req)
	if err != nil {
		return
	}
	resp.Body.Close()
}

func (app *App) listenEvents() {
	for {
		select {
		case <-app.quit:
			return
		default:
		}
		app.connectWebSocket()
		time.Sleep(2 * time.Second)
	}
}

func (app *App) connectWebSocket() {
	app.fetchStatus()

	c, _, err := websocket.DefaultDialer.Dial("ws://127.0.0.1:3678/events", nil)
	if err != nil {
		log.Printf("ws connect error: %v", err)
		return
	}
	defer c.Close()
	log.Println("websocket connected")

	for {
		select {
		case <-app.quit:
			return
		default:
		}

		_, message, err := c.ReadMessage()
		if err != nil {
			log.Printf("ws read error: %v", err)
			return
		}

		var ev WSEvent
		if err := json.Unmarshal(message, &ev); err != nil {
			continue
		}
		app.handleEvent(ev)
	}
}

func (app *App) fetchStatus() {
	resp, err := httpClient.Get(apiBase + "/status")
	if err != nil {
		return
	}
	defer resp.Body.Close()

	var status PlayerStatus
	if err := json.NewDecoder(resp.Body).Decode(&status); err != nil {
		return
	}

	app.mu.Lock()
	if status.Track != nil {
		app.trackName = status.Track.Name
		app.artistName = strings.Join(status.Track.ArtistNames, ", ")
		app.albumName = status.Track.AlbumName
		app.coverImg = nil
		app.duration = status.Track.Duration
		app.position = status.Track.Position
		app.lastPosTime = time.Now()
		app.connected = true
		app.paused = status.Paused
		app.volume = status.Volume
		app.volumeMax = status.VolumeSteps
		coverURL := status.Track.AlbumCoverURL
		app.mu.Unlock()
		if coverURL != "" {
			go app.fetchCover(coverURL)
		} else {
			app.rebuildPlayingScene(nil)
		}
	} else {
		app.connected = status.Username != ""
		app.coverImg = nil
		app.mu.Unlock()
	}
}

func (app *App) handleEvent(ev WSEvent) {
	defer func() {
		if r := recover(); r != nil {
			log.Printf("recovered panic in handleEvent(%s): %v", ev.Type, r)
		}
	}()
	log.Printf("event: %s", ev.Type)
	switch ev.Type {
	case "metadata":
		var meta MetadataEvent
		if err := json.Unmarshal(ev.Data, &meta); err != nil {
			return
		}
		app.mu.Lock()
		app.trackName = meta.Name
		app.artistName = strings.Join(meta.ArtistNames, ", ")
		app.albumName = meta.AlbumName
		app.coverImg = nil
		app.duration = meta.Duration
		app.position = meta.Position
		app.lastPosTime = time.Now()
		app.connected = true
		coverURL := meta.AlbumCoverURL
		app.mu.Unlock()
		if coverURL != "" {
			go app.fetchCover(coverURL)
		} else {
			app.rebuildPlayingScene(nil)
		}
		app.render()

	case "playing":
		app.mu.Lock()
		app.paused = false
		app.lastPosTime = time.Now()
		app.mu.Unlock()
		app.render()

	case "paused", "not_playing":
		app.mu.Lock()
		app.paused = true
		app.mu.Unlock()
		app.render()

	case "stopped":
		app.mu.Lock()
		app.paused = true
		app.trackName = ""
		app.artistName = ""
		app.coverImg = nil
		app.connected = false
		app.mu.Unlock()
		app.rebuildPlayingScene(nil)
		app.render()

	case "volume":
		var vol VolumeEvent
		if err := json.Unmarshal(ev.Data, &vol); err != nil {
			return
		}
		app.mu.Lock()
		app.volume = vol.Value
		app.volumeMax = vol.Max
		app.mu.Unlock()
		app.render()

	case "seek":
		var meta MetadataEvent
		if err := json.Unmarshal(ev.Data, &meta); err != nil {
			return
		}
		app.mu.Lock()
		app.position = meta.Position
		app.lastPosTime = time.Now()
		app.mu.Unlock()
		app.render()

	case "active":
		app.mu.Lock()
		app.connected = true
		app.mu.Unlock()
		app.fetchStatus()
		app.render()

	case "inactive":
		app.mu.Lock()
		app.connected = false
		app.trackName = ""
		app.artistName = ""
		app.coverImg = nil
		app.mu.Unlock()
		app.rebuildPlayingScene(nil)
		app.render()
	}
}

func (app *App) fetchCover(url string) {
	defer func() {
		if r := recover(); r != nil {
			log.Printf("recovered panic in fetchCover: %v", r)
		}
	}()
	resp, err := httpClient.Get(url)
	if err != nil {
		log.Printf("cover fetch error: %v", err)
		return
	}
	defer resp.Body.Close()

	img, _, err := image.Decode(resp.Body)
	if err != nil {
		log.Printf("cover decode error: %v", err)
		return
	}

	app.mu.Lock()
	app.coverImg = img
	app.mu.Unlock()
	app.rebuildPlayingScene(img)
	app.render()
}

func measureText(face font.Face, text string) int {
	return font.MeasureString(face, text).Round()
}

func truncateStr(s string, maxLen int) string {
	runes := []rune(s)
	if len(runes) <= maxLen {
		return s
	}
	return string(runes[:maxLen-1]) + "…"
}

func formatDuration(ms int64) string {
	if ms < 0 {
		ms = 0
	}
	totalSec := ms / 1000
	return fmt.Sprintf("%d:%02d", totalSec/60, totalSec%60)
}

func maxInt(a, b int) int {
	if a > b {
		return a
	}
	return b
}
