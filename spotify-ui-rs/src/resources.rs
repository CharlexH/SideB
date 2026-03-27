use std::path::{Path, PathBuf};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read};

use crate::types::RgbaImage;

/// Build candidate paths for a resource file, matching Go logic.
pub fn resource_candidates(name: &str) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    let mut add = |p: PathBuf| {
        let canonical = p.to_string_lossy().to_string();
        if seen.insert(canonical) {
            paths.push(p);
        }
    };

    add(Path::new("resources").join(name));
    add(Path::new("package/SpotifyConnect/resources").join(name));
    add(Path::new("../package/SpotifyConnect/resources").join(name));

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            add(dir.join("resources").join(name));
        }
    }

    paths
}

/// Load a PNG image from resource candidates.
pub fn load_image_resource(name: &str) -> Option<RgbaImage> {
    for path in resource_candidates(name) {
        if let Ok(img) = load_png(&path) {
            eprintln!("using image resource: {}", path.display());
            return Some(img);
        }
    }
    eprintln!("image resource not found: {name}");
    None
}

/// Decode a PNG file into an RgbaImage.
fn load_png(path: &Path) -> Result<RgbaImage, Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    // Auto-expand indexed/grayscale/16-bit to 8-bit RGBA
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    buf.truncate(info.buffer_size());

    let width = info.width;
    let height = info.height;

    // After EXPAND+ALPHA transforms, output is either RGB or RGBA
    let pixels = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity((width * height * 4) as usize);
            for chunk in buf.chunks(3) {
                rgba.extend_from_slice(chunk);
                rgba.push(255);
            }
            rgba
        }
        other => {
            return Err(format!("unexpected color type after expand: {other:?}").into());
        }
    };

    Ok(RgbaImage {
        pixels,
        width,
        height,
    })
}

/// Decode JPEG or PNG bytes into an RgbaImage (for cover art fetched over HTTP).
pub fn decode_image_bytes(data: &[u8]) -> Option<RgbaImage> {
    // Try PNG first
    if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        let decoder = png::Decoder::new(data);
        if let Ok(mut reader) = decoder.read_info() {
            let mut buf = vec![0u8; reader.output_buffer_size()];
            if let Ok(info) = reader.next_frame(&mut buf) {
                buf.truncate(info.buffer_size());
                let pixels = if info.color_type == png::ColorType::Rgba {
                    buf
                } else if info.color_type == png::ColorType::Rgb {
                    let mut rgba = Vec::with_capacity((info.width * info.height * 4) as usize);
                    for chunk in buf.chunks(3) {
                        rgba.extend_from_slice(chunk);
                        rgba.push(255);
                    }
                    rgba
                } else {
                    return None;
                };
                return Some(RgbaImage {
                    pixels,
                    width: info.width,
                    height: info.height,
                });
            }
        }
        return None;
    }

    // Try JPEG
    let mut decoder = jpeg_decoder::Decoder::new(data);
    if let Ok(pixels_rgb) = decoder.decode() {
        let info = decoder.info().unwrap();
        let w = info.width as u32;
        let h = info.height as u32;
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for chunk in pixels_rgb.chunks(3) {
            rgba.extend_from_slice(chunk);
            rgba.push(255);
        }
        return Some(RgbaImage {
            pixels: rgba,
            width: w,
            height: h,
        });
    }

    None
}

/// Load font data from candidate paths. Returns the raw bytes.
pub fn load_font_data() -> Option<Vec<u8>> {
    let paths = [
        "resources/font_mono.ttf",
        "resources/font.ttf",
        "../package/SpotifyConnect/resources/font_mono.ttf",
        "../package/SpotifyConnect/resources/font.ttf",
        "/usr/trimui/res/font/CJKFont.ttf",
        "/usr/trimui/apps/bookreader/regular.ttf",
    ];

    for path in &paths {
        if let Ok(mut f) = File::open(path) {
            let mut data = Vec::new();
            if f.read_to_end(&mut data).is_ok() {
                eprintln!("using font: {path}");
                return Some(data);
            }
        }
    }
    None
}
