use std::path::{Path, PathBuf};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read};

use jpeg_decoder::{ImageInfo as JpegImageInfo, PixelFormat};

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
    add(Path::new("/mnt/SDCARD/Apps/SpotifyConnect/resources").join(name));

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            add(dir.join("resources").join(name));
        }
    }

    paths
}

/// Find the first existing resource file path.
pub fn find_resource(name: &str) -> Option<PathBuf> {
    for path in resource_candidates(name) {
        if path.exists() {
            return Some(path);
        }
    }
    None
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
    if let Ok(decoded_pixels) = decoder.decode() {
        if let Some(info) = decoder.info() {
            return decode_jpeg_to_rgba(decoded_pixels, info);
        }
    }

    None
}

fn decode_jpeg_to_rgba(decoded_pixels: Vec<u8>, info: JpegImageInfo) -> Option<RgbaImage> {
    let w = info.width as u32;
    let h = info.height as u32;
    let pixel_count = (w as usize).checked_mul(h as usize)?;
    let mut rgba = Vec::with_capacity(pixel_count.checked_mul(4)?);

    match info.pixel_format {
        PixelFormat::L8 => {
            if decoded_pixels.len() != pixel_count {
                return None;
            }
            for gray in decoded_pixels {
                rgba.extend_from_slice(&[gray, gray, gray, 255]);
            }
        }
        PixelFormat::RGB24 => {
            if decoded_pixels.len() != pixel_count.checked_mul(3)? {
                return None;
            }
            for chunk in decoded_pixels.chunks_exact(3) {
                rgba.extend_from_slice(chunk);
                rgba.push(255);
            }
        }
        PixelFormat::CMYK32 => {
            if decoded_pixels.len() != pixel_count.checked_mul(4)? {
                return None;
            }
            for chunk in decoded_pixels.chunks_exact(4) {
                let c = chunk[0] as u16;
                let m = chunk[1] as u16;
                let y = chunk[2] as u16;
                let k = chunk[3] as u16;
                let r = 255u16.saturating_sub((c + k).min(255)) as u8;
                let g = 255u16.saturating_sub((m + k).min(255)) as u8;
                let b = 255u16.saturating_sub((y + k).min(255)) as u8;
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        PixelFormat::L16 => return None,
    }

    Some(RgbaImage {
        pixels: rgba,
        width: w,
        height: h,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use jpeg_decoder::CodingProcess;

    #[test]
    fn grayscale_jpeg_pixels_expand_to_rgba() {
        let info = JpegImageInfo {
            width: 2,
            height: 1,
            pixel_format: PixelFormat::L8,
            coding_process: CodingProcess::DctSequential,
        };

        let img = decode_jpeg_to_rgba(vec![0x11, 0x88], info).unwrap();

        assert_eq!(img.width, 2);
        assert_eq!(img.height, 1);
        assert_eq!(img.pixel_at(0, 0), (0x11, 0x11, 0x11, 255));
        assert_eq!(img.pixel_at(1, 0), (0x88, 0x88, 0x88, 255));
    }
}
