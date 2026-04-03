use crate::constants::*;
use crate::types::RgbaImage;

/// Fast approximation of `x / 255` using bit shifts.
/// Accurate to within 1 for all inputs in [0, 65025].
#[inline(always)]
fn div255(x: i32) -> i32 {
    (x + 128 + ((x + 128) >> 8)) >> 8
}

/// Clear entire buffer to a solid BGRA color.
pub fn clear_buffer(buf: &mut [u8], r: u8, g: u8, b: u8, a: u8) {
    let pixel = [b, g, r, a];
    for chunk in buf.chunks_exact_mut(BPP) {
        chunk.copy_from_slice(&pixel);
    }
}

/// Set a single pixel in BGRA buffer (no blending).
#[inline]
pub fn set_pixel(buf: &mut [u8], x: i32, y: i32, r: u8, g: u8, b: u8, a: u8) {
    if x < 0 || x >= SCREEN_W as i32 || y < 0 || y >= SCREEN_H as i32 {
        return;
    }
    let offset = ((y as usize) * SCREEN_W + (x as usize)) * BPP;
    buf[offset] = b;
    buf[offset + 1] = g;
    buf[offset + 2] = r;
    buf[offset + 3] = a;
}

/// Alpha-blend a single pixel onto BGRA buffer (Porter-Duff over).
#[inline]
pub fn blend_pixel(buf: &mut [u8], x: i32, y: i32, r: u8, g: u8, b: u8, a: u8) {
    if a == 0 || x < 0 || x >= SCREEN_W as i32 || y < 0 || y >= SCREEN_H as i32 {
        return;
    }
    let offset = ((y as usize) * SCREEN_W + (x as usize)) * BPP;
    let sa = a as i32;
    let dr = buf[offset + 2] as i32;
    let dg = buf[offset + 1] as i32;
    let db = buf[offset] as i32;
    let da = buf[offset + 3] as i32;
    let out_a = sa + div255(da * (255 - sa));
    if out_a == 0 {
        return;
    }
    let out_r = (r as i32 * sa + div255(dr * da * (255 - sa))) / out_a;
    let out_g = (g as i32 * sa + div255(dg * da * (255 - sa))) / out_a;
    let out_b = (b as i32 * sa + div255(db * da * (255 - sa))) / out_a;
    buf[offset] = out_b as u8;
    buf[offset + 1] = out_g as u8;
    buf[offset + 2] = out_r as u8;
    buf[offset + 3] = out_a as u8;
}

/// Fill a rectangle with a solid BGRA color.
pub fn fill_rect(buf: &mut [u8], x: i32, y: i32, w: i32, h: i32, r: u8, g: u8, b: u8, a: u8) {
    let x0 = x.max(0) as usize;
    let y0 = y.max(0) as usize;
    let x1 = ((x + w) as usize).min(SCREEN_W);
    let y1 = ((y + h) as usize).min(SCREEN_H);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let pixel = [b, g, r, a];
    let row_w = x1 - x0;
    for py in y0..y1 {
        let row_off = (py * SCREEN_W + x0) * BPP;
        let row = &mut buf[row_off..row_off + row_w * BPP];
        for chunk in row.chunks_exact_mut(BPP) {
            chunk.copy_from_slice(&pixel);
        }
    }
}

/// Draw an RGBA image onto BGRA buffer with alpha blending.
/// Generic version for any RgbaImage.
pub fn draw_image_alpha(buf: &mut [u8], img: &RgbaImage, x: i32, y: i32) {
    let w = img.width as i32;
    let h = img.height as i32;

    // Clip to screen
    let start_x = x.max(0);
    let start_y = y.max(0);
    let end_x = (x + w).min(SCREEN_W as i32);
    let end_y = (y + h).min(SCREEN_H as i32);
    if start_x >= end_x || start_y >= end_y {
        return;
    }

    let src_x0 = (start_x - x) as usize;
    let src_y0 = (start_y - y) as usize;
    let draw_w = (end_x - start_x) as usize;
    let draw_h = (end_y - start_y) as usize;
    let img_w = img.width as usize;

    for row in 0..draw_h {
        let sy = src_y0 + row;
        let dy = start_y as usize + row;
        let src_row_off = (sy * img_w + src_x0) * 4;
        let dst_row_off = (dy * SCREEN_W + start_x as usize) * BPP;
        let src_row = &img.pixels[src_row_off..src_row_off + draw_w * 4];
        let dst_row = &mut buf[dst_row_off..dst_row_off + draw_w * BPP];

        let mut col = 0;
        while col < draw_w {
            let si = col * 4;
            let sa = src_row[si + 3];

            if sa == 0 {
                // Skip runs of fully transparent pixels
                col += 1;
                while col < draw_w && src_row[col * 4 + 3] == 0 {
                    col += 1;
                }
                continue;
            }

            let di = col * BPP;
            if sa == 255 {
                // Opaque: direct copy (RGB→BGR swap)
                dst_row[di] = src_row[si + 2];
                dst_row[di + 1] = src_row[si + 1];
                dst_row[di + 2] = src_row[si];
                dst_row[di + 3] = 255;
            } else {
                // Alpha blend
                let a = sa as i32;
                let inv = 255 - a;
                let sb = src_row[si + 2] as i32;
                let sg = src_row[si + 1] as i32;
                let sr = src_row[si] as i32;
                dst_row[di] = div255(sb * a + dst_row[di] as i32 * inv) as u8;
                dst_row[di + 1] = div255(sg * a + dst_row[di + 1] as i32 * inv) as u8;
                dst_row[di + 2] = div255(sr * a + dst_row[di + 2] as i32 * inv) as u8;
                dst_row[di + 3] = 255;
            }
            col += 1;
        }
    }
}

/// Draw an RGBA image with nearest-neighbor scaling, centered at (cx, cy).
pub fn draw_image_scaled(buf: &mut [u8], img: &RgbaImage, center_x: i32, center_y: i32, size: i32) {
    if size <= 0 {
        return;
    }
    let start_x = center_x - size / 2;
    let start_y = center_y - size / 2;
    let src_w = img.width as i32;
    let src_h = img.height as i32;

    // Clip to screen
    let clip_y0 = 0.max(-start_y) as i32;
    let clip_y1 = size.min(SCREEN_H as i32 - start_y);
    let clip_x0 = 0.max(-start_x) as i32;
    let clip_x1 = size.min(SCREEN_W as i32 - start_x);
    if clip_x0 >= clip_x1 || clip_y0 >= clip_y1 {
        return;
    }

    for dy in clip_y0..clip_y1 {
        let py = (start_y + dy) as usize;
        let src_y = (dy * src_h / size) as u32;
        let dst_row_off = (py * SCREEN_W + (start_x + clip_x0) as usize) * BPP;
        let row_pixels = (clip_x1 - clip_x0) as usize;
        let dst_row = &mut buf[dst_row_off..dst_row_off + row_pixels * BPP];

        for dx in clip_x0..clip_x1 {
            let src_x = (dx * src_w / size) as u32;
            let (r, g, b, a) = img.pixel_at(src_x, src_y);
            if a == 0 {
                continue;
            }
            let di = ((dx - clip_x0) as usize) * BPP;
            if a == 255 {
                dst_row[di] = b;
                dst_row[di + 1] = g;
                dst_row[di + 2] = r;
                dst_row[di + 3] = 255;
            } else {
                let sa = a as i32;
                let inv = 255 - sa;
                dst_row[di] = div255(b as i32 * sa + dst_row[di] as i32 * inv) as u8;
                dst_row[di + 1] = div255(g as i32 * sa + dst_row[di + 1] as i32 * inv) as u8;
                dst_row[di + 2] = div255(r as i32 * sa + dst_row[di + 2] as i32 * inv) as u8;
                dst_row[di + 3] = 255;
            }
        }
    }
}

/// Copy a rectangular region between two framebuffer-sized buffers.
pub fn copy_rect(dst: &mut [u8], src: &[u8], min_x: i32, min_y: i32, max_x: i32, max_y: i32) {
    let min_x = min_x.max(0) as usize;
    let min_y = min_y.max(0) as usize;
    let max_x = (max_x as usize).min(SCREEN_W);
    let max_y = (max_y as usize).min(SCREEN_H);
    if min_x >= max_x || min_y >= max_y {
        return;
    }
    let row_bytes = (max_x - min_x) * BPP;
    for y in min_y..max_y {
        let start = (y * SCREEN_W + min_x) * BPP;
        let end = start + row_bytes;
        dst[start..end].copy_from_slice(&src[start..end]);
    }
}

/// Draw a status dot (red square with glow).
pub fn draw_status_dot(buf: &mut [u8], x: i32, y: i32) {
    fill_rect(buf, x - 8, y - 8, 40, 40, 255, 40, 40, 64);
    fill_rect(buf, x, y, 24, 24, 255, 40, 40, 255);
}

/// Draw a soundwave visualization.
pub fn draw_soundwave(buf: &mut [u8], x: i32, y: i32, bars: &[f64; 24], active: bool) {
    let a = if active { 255u8 } else { 160u8 };
    for (i, &height) in bars.iter().enumerate() {
        let bar_height = height.max(SOUNDWAVE_MIN_HEIGHT).round() as i32;
        fill_rect(
            buf,
            x + i as i32 * 12,
            y - bar_height,
            4,
            bar_height,
            255,
            255,
            255,
            a,
        );
    }
}

/// Test if a point (px, py) is inside a heart shape centered at (0,0) with given size.
/// Uses the implicit heart curve: (x^2 + y^2 - 1)^3 - x^2 * y^3 <= 0
/// Coordinates are normalized so the heart fits in a `size x size` box.
#[inline]
fn heart_contains(px: f64, py: f64) -> bool {
    let x2 = px * px;
    let y2 = py * py;
    let t = x2 + y2 - 1.0;
    t * t * t - x2 * y2 * py <= 0.0
}

/// Draw a filled heart at (x, y) with given size.
pub fn draw_heart_filled(buf: &mut [u8], x: i32, y: i32, size: i32, r: u8, g: u8, b: u8, a: u8) {
    let half = size as f64 / 2.0;
    for dy in 0..size {
        for dx in 0..size {
            // Map pixel to normalized heart coordinate space [-1.3, 1.3]
            let nx = (dx as f64 - half) / half * 1.3;
            let ny = -(dy as f64 - half) / half * 1.3 + 0.2; // offset to center vertically
            if heart_contains(nx, ny) {
                blend_pixel(buf, x + dx, y + dy, r, g, b, a);
            }
        }
    }
}

/// Draw an outline heart at (x, y) with given size.
pub fn draw_heart_outline(buf: &mut [u8], x: i32, y: i32, size: i32, r: u8, g: u8, b: u8, a: u8) {
    let half = size as f64 / 2.0;
    let thickness = 0.12; // border width in normalized coords
    for dy in 0..size {
        for dx in 0..size {
            let nx = (dx as f64 - half) / half * 1.3;
            let ny = -(dy as f64 - half) / half * 1.3 + 0.2;
            let inside = heart_contains(nx, ny);
            // Check if we're near the border by testing slightly inward
            let nx_inner = nx * (1.0 - thickness);
            let ny_inner = ny * (1.0 - thickness);
            let deep_inside = heart_contains(nx_inner, ny_inner);
            if inside && !deep_inside {
                blend_pixel(buf, x + dx, y + dy, r, g, b, a);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_draw_rgba_alpha_blends_over_opaque_buffer() {
        // 1x1 opaque red buffer
        let mut buf = vec![0u8; FB_SIZE];
        // Set pixel (0,0) to R=10, G=10, B=10, A=255 in BGRA
        buf[0] = 10; // B
        buf[1] = 10; // G
        buf[2] = 10; // R
        buf[3] = 255; // A

        // Opaque source: should replace
        let mut img = RgbaImage::new(1, 1);
        img.set_pixel(0, 0, 50, 50, 50, 255);
        draw_image_alpha(&mut buf, &img, 0, 0);
        assert_eq!(buf[2], 50); // R
        assert_eq!(buf[1], 50); // G
        assert_eq!(buf[0], 50); // B

        // Reset destination
        buf[0] = 10;
        buf[1] = 10;
        buf[2] = 10;
        buf[3] = 255;

        // Translucent source (a=128): blend
        img.set_pixel(0, 0, 50, 50, 50, 128);
        draw_image_alpha(&mut buf, &img, 0, 0);
        // Expected: (50*128 + 10*127) / 255 ≈ 30
        let expected = ((50i32 * 128 + 10 * 127) / 255) as u8;
        assert_eq!(buf[2], expected); // R
    }
}
