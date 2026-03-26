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
	"net/http"
	"os"
	"os/signal"
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
	fb       []byte // mmap'd framebuffer (visible)
	backBuf  []byte // off-screen back buffer (draw here)
	fbFile   *os.File
	fontFace font.Face
	fontBig  font.Face
	fontMed  font.Face

	mu          sync.Mutex // protects state fields below
	trackName   string
	artistName  string
	albumName   string
	coverImg    image.Image
	paused      bool
	volume      int
	volumeMax   int
	connected   bool
	position    int64
	duration    int64
	lastPosTime time.Time
	lastAction  time.Time // debounce for A button

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

	if err := app.initFramebuffer(); err != nil {
		log.Fatalf("framebuffer init: %v", err)
	}
	defer app.fbFile.Close()

	if err := app.initFonts(); err != nil {
		log.Fatalf("font init: %v", err)
	}

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

	// Allocate back buffer for double buffering
	app.backBuf = make([]byte, fbSize)
	return nil
}

// swapBuffers copies the back buffer to the visible framebuffer in one shot
func (app *App) swapBuffers() {
	copy(app.fb, app.backBuf)
}

func (app *App) initFonts() error {
	fontPaths := []string{
		"resources/font.ttf",
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

	app.fontBig, err = opentype.NewFace(parsedFont, &opentype.FaceOptions{
		Size: 36, DPI: 72, Hinting: font.HintingFull,
	})
	if err != nil {
		return err
	}

	app.fontMed, err = opentype.NewFace(parsedFont, &opentype.FaceOptions{
		Size: 24, DPI: 72, Hinting: font.HintingFull,
	})
	if err != nil {
		return err
	}

	app.fontFace, err = opentype.NewFace(parsedFont, &opentype.FaceOptions{
		Size: 18, DPI: 72, Hinting: font.HintingFull,
	})
	if err != nil {
		return err
	}

	return nil
}

// --- Drawing primitives (all write to backBuf, NOT fb) ---

func (app *App) fillRect(x, y, w, h int, c color.Color) {
	r, g, b, a := c.RGBA()
	br, bg, bb, ba := byte(r>>8), byte(g>>8), byte(b>>8), byte(a>>8)
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
			offset := (py*screenW + px) * bpp
			app.backBuf[offset+0] = bb
			app.backBuf[offset+1] = bg
			app.backBuf[offset+2] = br
			app.backBuf[offset+3] = ba
		}
	}
}

func (app *App) drawImage(img image.Image, x, y, w, h int) {
	if img == nil {
		return
	}
	bounds := img.Bounds()
	srcW := bounds.Dx()
	srcH := bounds.Dy()

	for dy := 0; dy < h; dy++ {
		srcY := bounds.Min.Y + dy*srcH/h
		py := y + dy
		if py < 0 || py >= screenH {
			continue
		}
		for dx := 0; dx < w; dx++ {
			srcX := bounds.Min.X + dx*srcW/w
			px := x + dx
			if px < 0 || px >= screenW {
				continue
			}
			r, g, b, a := img.At(srcX, srcY).RGBA()
			offset := (py*screenW + px) * bpp
			app.backBuf[offset+0] = byte(b >> 8)
			app.backBuf[offset+1] = byte(g >> 8)
			app.backBuf[offset+2] = byte(r >> 8)
			app.backBuf[offset+3] = byte(a >> 8)
		}
	}
}

func (app *App) drawCircle(cx, cy, r int, c color.Color) {
	cr, cg, cb, ca := c.RGBA()
	br, bg, bb, ba := byte(cr>>8), byte(cg>>8), byte(cb>>8), byte(ca>>8)
	for dy := -r; dy <= r; dy++ {
		for dx := -r; dx <= r; dx++ {
			if dx*dx+dy*dy <= r*r {
				px, py := cx+dx, cy+dy
				if px >= 0 && px < screenW && py >= 0 && py < screenH {
					offset := (py*screenW + px) * bpp
					app.backBuf[offset+0] = bb
					app.backBuf[offset+1] = bg
					app.backBuf[offset+2] = br
					app.backBuf[offset+3] = ba
				}
			}
		}
	}
}

// backBufImage wraps the back buffer as draw.Image for font rendering
func (app *App) backBufImage() *FBImage {
	return &FBImage{buf: app.backBuf}
}

type FBImage struct {
	buf []byte
}

func (f *FBImage) ColorModel() color.Model { return color.RGBAModel }
func (f *FBImage) Bounds() image.Rectangle {
	return image.Rect(0, 0, screenW, screenH)
}
func (f *FBImage) At(x, y int) color.Color {
	if x < 0 || x >= screenW || y < 0 || y >= screenH {
		return color.RGBA{}
	}
	offset := (y*screenW + x) * bpp
	return color.RGBA{
		R: f.buf[offset+2], G: f.buf[offset+1],
		B: f.buf[offset+0], A: f.buf[offset+3],
	}
}
func (f *FBImage) Set(x, y int, c color.Color) {
	if x < 0 || x >= screenW || y < 0 || y >= screenH {
		return
	}
	r, g, b, a := c.RGBA()
	offset := (y*screenW + x) * bpp
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

// --- Render (draws to back buffer, then swaps) ---

func (app *App) render() {
	app.renderMu.Lock()
	defer app.renderMu.Unlock()

	app.mu.Lock()
	trackName := app.trackName
	artistName := app.artistName
	coverImg := app.coverImg
	paused := app.paused
	volume := app.volume
	volumeMax := app.volumeMax
	connected := app.connected
	position := app.position
	duration := app.duration
	app.mu.Unlock()

	// Clear back buffer with background color
	bgR, bgG, bgB, bgA := byte(18), byte(18), byte(18), byte(255)
	for i := 0; i < fbSize; i += bpp {
		app.backBuf[i+0] = bgB
		app.backBuf[i+1] = bgG
		app.backBuf[i+2] = bgR
		app.backBuf[i+3] = bgA
	}

	white := color.RGBA{R: 255, G: 255, B: 255, A: 255}
	gray := color.RGBA{R: 170, G: 170, B: 170, A: 255}
	dimGray := color.RGBA{R: 80, G: 80, B: 80, A: 255}
	green := color.RGBA{R: 29, G: 185, B: 84, A: 255}

	if !connected {
		app.drawText(app.fontBig, "TrimUI Brick", screenW/2-120, 340, white)
		app.drawText(app.fontMed, "Waiting for Spotify Connect...", screenW/2-180, 400, gray)
		app.drawText(app.fontFace, "Open Spotify and select \"TrimUI Brick\"", screenW/2-200, 440, dimGray)
		// Exit hint at bottom
		exitHint := "[B] Exit"
		app.drawText(app.fontFace, exitHint, screenW/2-measureText(app.fontFace, exitHint)/2, screenH-20, dimGray)
		app.swapBuffers()
		return
	}

	// Album cover: centered, 400x400
	coverSize := 400
	coverX := (screenW - coverSize) / 2
	coverY := 60

	if coverImg != nil {
		app.drawImage(coverImg, coverX, coverY, coverSize, coverSize)
	} else {
		app.fillRect(coverX, coverY, coverSize, coverSize, dimGray)
		app.drawText(app.fontMed, "No Cover", coverX+140, coverY+210, gray)
	}

	// Track name (centered)
	textY := coverY + coverSize + 50
	tn := truncateStr(trackName, 35)
	app.drawText(app.fontBig, tn, screenW/2-measureText(app.fontBig, tn)/2, textY, white)

	// Artist name (centered)
	textY += 40
	an := truncateStr(artistName, 50)
	app.drawText(app.fontMed, an, screenW/2-measureText(app.fontMed, an)/2, textY, gray)

	// Progress bar
	textY += 40
	barW := 500
	barH := 6
	barX := (screenW - barW) / 2
	app.fillRect(barX, textY, barW, barH, dimGray)

	if duration > 0 {
		progress := float64(position) / float64(duration)
		if progress > 1 {
			progress = 1
		}
		filledW := int(float64(barW) * progress)
		app.fillRect(barX, textY, filledW, barH, green)
		app.drawCircle(barX+filledW, textY+barH/2, 6, white)
	}

	// Time labels
	textY += 20
	posStr := formatDuration(position)
	durStr := formatDuration(duration)
	app.drawText(app.fontFace, posStr, barX, textY, gray)
	app.drawText(app.fontFace, durStr, barX+barW-measureText(app.fontFace, durStr), textY, gray)

	// Playback controls
	controlY := textY + 40
	cx := screenW / 2
	app.drawText(app.fontBig, "<<", cx-120, controlY, white)
	if paused {
		app.drawText(app.fontBig, "PLAY", cx-30, controlY, green)
	} else {
		app.drawText(app.fontBig, "PAUSE", cx-40, controlY, green)
	}
	app.drawText(app.fontBig, ">>", cx+80, controlY, white)

	// Volume bar
	volY := controlY + 40
	volBarW := 300
	volBarX := (screenW - volBarW) / 2
	app.fillRect(volBarX, volY, volBarW, 6, dimGray)
	if volumeMax > 0 {
		app.fillRect(volBarX, volY, volBarW*volume/volumeMax, 6, green)
	}
	volLabel := fmt.Sprintf("VOL %d%%", volume*100/maxInt(volumeMax, 1))
	app.drawText(app.fontFace, volLabel, screenW/2-measureText(app.fontFace, volLabel)/2, volY+25, gray)

	// Key hints
	hintY := screenH - 20
	hints := "[A] Play/Pause   [<>] Prev/Next   [Up/Down] Volume   [MENU] Exit"
	app.drawText(app.fontFace, hints, screenW/2-measureText(app.fontFace, hints)/2, hintY, dimGray)

	// Single atomic swap — no flicker
	app.swapBuffers()
}

func (app *App) renderLoop() {
	ticker := time.NewTicker(500 * time.Millisecond)
	defer ticker.Stop()

	for {
		select {
		case <-ticker.C:
			app.mu.Lock()
			needsRender := !app.paused && app.duration > 0 && !app.lastPosTime.IsZero()
			if needsRender {
				elapsed := time.Since(app.lastPosTime).Milliseconds()
				app.position += elapsed
				if app.position > app.duration {
					app.position = app.duration
				}
				app.lastPosTime = time.Now()
			}
			app.mu.Unlock()

			if needsRender {
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
		}
	} else {
		app.connected = status.Username != ""
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
		app.duration = meta.Duration
		app.position = meta.Position
		app.lastPosTime = time.Now()
		app.connected = true
		coverURL := meta.AlbumCoverURL
		app.mu.Unlock()
		if coverURL != "" {
			go app.fetchCover(coverURL)
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
