use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct PlayerStatus {
    pub username: String,
    pub device_name: String,
    pub stopped: bool,
    pub paused: bool,
    pub buffering: bool,
    pub volume: i32,
    pub volume_steps: i32,
    pub track: Option<Track>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Track {
    pub uri: String,
    pub name: String,
    pub artist_names: Vec<String>,
    pub album_name: String,
    pub album_cover_url: String,
    pub duration: i64,
    pub position: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WSEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: Option<Box<serde_json::value::RawValue>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetadataEvent {
    pub uri: String,
    pub name: String,
    pub artist_names: Vec<String>,
    pub album_name: String,
    pub album_cover_url: String,
    pub position: i64,
    pub duration: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VolumeEvent {
    pub value: i32,
    pub max: i32,
}

/// Simple RGBA image buffer (source images are RGBA; framebuffer is BGRA).
#[derive(Clone)]
pub struct RgbaImage {
    pub pixels: Vec<u8>, // RGBA, 4 bytes per pixel
    pub width: u32,
    pub height: u32,
}

impl RgbaImage {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            pixels: vec![0u8; (width as usize) * (height as usize) * 4],
            width,
            height,
        }
    }

    #[inline]
    pub fn pix_offset(&self, x: u32, y: u32) -> usize {
        ((y as usize) * (self.width as usize) + (x as usize)) * 4
    }

    #[inline]
    pub fn pixel_at(&self, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let off = self.pix_offset(x, y);
        (
            self.pixels[off],
            self.pixels[off + 1],
            self.pixels[off + 2],
            self.pixels[off + 3],
        )
    }

    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, r: u8, g: u8, b: u8, a: u8) {
        let off = self.pix_offset(x, y);
        self.pixels[off] = r;
        self.pixels[off + 1] = g;
        self.pixels[off + 2] = b;
        self.pixels[off + 3] = a;
    }
}

/// Raw Linux input event (24 bytes on aarch64).
#[derive(Debug, Clone, Copy)]
pub struct InputEvent {
    pub event_type: u16,
    pub code: u16,
    pub value: i32,
}
