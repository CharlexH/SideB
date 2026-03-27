use std::collections::HashMap;

use crate::constants::*;
use crate::types::RgbaImage;

/// Rotate image by angle (radians) around center. Returns new RgbaImage.
pub fn rotate_image(img: &RgbaImage, angle: f64) -> RgbaImage {
    let w = img.width as usize;
    let h = img.height as usize;
    let mut dst = RgbaImage::new(img.width, img.height);
    let cx = (w as f64 - 1.0) / 2.0;
    let cy = (h as f64 - 1.0) / 2.0;
    let sin_a = angle.sin();
    let cos_a = angle.cos();

    for dy in 0..h {
        for dx in 0..w {
            let rel_x = dx as f64 - cx;
            let rel_y = dy as f64 - cy;
            let src_x = (cos_a * rel_x + sin_a * rel_y + cx).round() as i32;
            let src_y = (-sin_a * rel_x + cos_a * rel_y + cy).round() as i32;
            if src_x < 0 || src_x >= w as i32 || src_y < 0 || src_y >= h as i32 {
                continue;
            }
            let (r, g, b, a) = img.pixel_at(src_x as u32, src_y as u32);
            dst.set_pixel(dx as u32, dy as u32, r, g, b, a);
        }
    }
    dst
}

/// Scale image to size×size using nearest-neighbor.
pub fn scale_nearest(img: &RgbaImage, size: u32) -> RgbaImage {
    let mut dst = RgbaImage::new(size, size);
    let src_w = img.width;
    let src_h = img.height;
    for dy in 0..size {
        let src_y = dy * src_h / size;
        for dx in 0..size {
            let src_x = dx * src_w / size;
            let (r, g, b, a) = img.pixel_at(src_x, src_y);
            dst.set_pixel(dx, dy, r, g, b, a);
        }
    }
    dst
}

/// Pre-compute N rotated frames of an image.
pub fn build_rotated_frames(img: &RgbaImage, count: usize) -> Vec<RgbaImage> {
    (0..count)
        .map(|i| {
            let angle = 2.0 * std::f64::consts::PI * i as f64 / count as f64;
            rotate_image(img, angle)
        })
        .collect()
}

/// Scale then build rotated frames.
fn build_scaled_rotated_frames(img: &RgbaImage, size: u32, count: usize) -> Vec<RgbaImage> {
    let scaled = scale_nearest(img, size);
    build_rotated_frames(&scaled, count)
}

/// Quantize roll size to nearest cache bucket.
pub fn quantize_roll_size(size: i32) -> i32 {
    let size = clamp_i32(size, LEFT_ROLL_MIN_SIZE, LEFT_ROLL_MAX_SIZE);
    if size == LEFT_ROLL_MAX_SIZE {
        return LEFT_ROLL_MAX_SIZE;
    }
    let steps =
        ((size - LEFT_ROLL_MIN_SIZE) as f64 / TAPEROLL_SIZE_STEP as f64).round() as i32;
    let quantized = LEFT_ROLL_MIN_SIZE + steps * TAPEROLL_SIZE_STEP;
    if quantized > LEFT_ROLL_MAX_SIZE {
        LEFT_ROLL_MAX_SIZE
    } else {
        quantized
    }
}

/// All discrete roll sizes for caching.
pub fn roll_cache_sizes() -> Vec<i32> {
    let mut sizes = Vec::new();
    let mut size = LEFT_ROLL_MIN_SIZE;
    while size < LEFT_ROLL_MAX_SIZE {
        sizes.push(size);
        size += TAPEROLL_SIZE_STEP;
    }
    sizes.push(LEFT_ROLL_MAX_SIZE);
    sizes
}

/// Build cache of rotated taperoll frames for each discrete size.
pub fn build_taperoll_frame_cache(
    img: &RgbaImage,
    frame_count: usize,
) -> HashMap<i32, Vec<RgbaImage>> {
    let mut cache = HashMap::new();
    for size in roll_cache_sizes() {
        cache.insert(
            size,
            build_scaled_rotated_frames(img, size as u32, frame_count),
        );
    }
    cache
}

/// Calculate left and right roll sizes from playback progress (0.0 to 1.0).
pub fn roll_sizes_for_progress(progress: f64) -> (i32, i32) {
    let progress = progress.clamp(0.0, 1.0);
    let range = (LEFT_ROLL_MAX_SIZE - LEFT_ROLL_MIN_SIZE) as f64;
    let left = LEFT_ROLL_MIN_SIZE + (range * progress).round() as i32;
    let right = RIGHT_ROLL_MAX_SIZE - ((RIGHT_ROLL_MAX_SIZE - RIGHT_ROLL_MIN_SIZE) as f64 * progress).round() as i32;
    (left, right)
}

/// Map angle (radians) to frame index in a pre-computed rotation cache.
pub fn frame_index_for_angle(angle: f64, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let two_pi = 2.0 * std::f64::consts::PI;
    let mut turn = angle % two_pi;
    if turn < 0.0 {
        turn += two_pi;
    }
    ((turn / two_pi * count as f64).round() as usize) % count
}

/// Build the overlay window image (tapeA composited).
pub fn build_overlay_window(tape_a: &RgbaImage) -> RgbaImage {
    let mut dst = RgbaImage::new(WINDOW_W as u32, WINDOW_H as u32);
    // Composite tapeA onto the window
    let copy_w = (tape_a.width as usize).min(WINDOW_W);
    let copy_h = (tape_a.height as usize).min(WINDOW_H);
    for y in 0..copy_h {
        for x in 0..copy_w {
            let (r, g, b, a) = tape_a.pixel_at(x as u32, y as u32);
            dst.set_pixel(x as u32, y as u32, r, g, b, a);
        }
    }
    dst
}

/// Build masked cover art: scale to fill window, apply luma-based mask.
pub fn build_masked_cover(img: &RgbaImage, mask: Option<&RgbaImage>) -> RgbaImage {
    let src_w = img.width as f64;
    let src_h = img.height as f64;
    let mut dst = RgbaImage::new(WINDOW_W as u32, WINDOW_H as u32);
    if src_w == 0.0 || src_h == 0.0 {
        return dst;
    }

    let scale = (WINDOW_W as f64 / src_w).max(WINDOW_H as f64 / src_h);
    let crop_w = WINDOW_W as f64 / scale;
    let crop_h = WINDOW_H as f64 / scale;
    let src_x0 = (src_w - crop_w) / 2.0;
    let src_y0 = (src_h - crop_h) / 2.0;

    for dy in 0..WINDOW_H {
        let src_y = src_y0 + (dy as f64 + 0.5) * crop_h / WINDOW_H as f64;
        let src_yi = clamp_i32(src_y as i32, 0, img.height as i32 - 1) as u32;
        for dx in 0..WINDOW_W {
            let src_x = src_x0 + (dx as f64 + 0.5) * crop_w / WINDOW_W as f64;
            let src_xi = clamp_i32(src_x as i32, 0, img.width as i32 - 1) as u32;
            let (r, g, b, mut a) = img.pixel_at(src_xi, src_yi);

            if let Some(m) = mask {
                let mx = (dx as u32) * m.width / WINDOW_W as u32;
                let my = (dy as u32) * m.height / WINDOW_H as u32;
                let (mr, mg, mb, ma) = m.pixel_at(mx.min(m.width - 1), my.min(m.height - 1));
                let luma = (299 * mr as i32 + 587 * mg as i32 + 114 * mb as i32) / 1000;
                let mask_value = luma * ma as i32 / 255;
                a = (a as i32 * mask_value / 255) as u8;
            }

            dst.set_pixel(dx as u32, dy as u32, r, g, b, a);
        }
    }
    dst
}

/// Build cassette foreground composite (tapeBase + overlay window).
pub fn build_cassette_foreground(
    tape_base: &RgbaImage,
    overlay_window: &RgbaImage,
) -> RgbaImage {
    let mut dst = RgbaImage::new(992, 584);

    // Draw tapeBase
    let copy_w = (tape_base.width as usize).min(992);
    let copy_h = (tape_base.height as usize).min(584);
    for y in 0..copy_h {
        for x in 0..copy_w {
            let (r, g, b, a) = tape_base.pixel_at(x as u32, y as u32);
            dst.set_pixel(x as u32, y as u32, r, g, b, a);
        }
    }

    // Draw overlay window at offset
    let ox = (WINDOW_X - TAPE_BASE_X) as usize;
    let oy = (WINDOW_Y - TAPE_BASE_Y) as usize;
    let ow = overlay_window.width as usize;
    let oh = overlay_window.height as usize;
    for y in 0..oh {
        let dy = oy + y;
        if dy >= 584 {
            break;
        }
        for x in 0..ow {
            let dx = ox + x;
            if dx >= 992 {
                break;
            }
            let (sr, sg, sb, sa) = overlay_window.pixel_at(x as u32, y as u32);
            if sa == 0 {
                continue;
            }
            let (dr, dg, db, da) = dst.pixel_at(dx as u32, dy as u32);
            if sa == 255 {
                dst.set_pixel(dx as u32, dy as u32, sr, sg, sb, 255);
            } else {
                // Porter-Duff "over" compositing
                let a = sa as i32;
                let inv = 255 - a;
                let out_a = a + da as i32 * inv / 255;
                if out_a == 0 {
                    continue;
                }
                let out_r = (sr as i32 * a + dr as i32 * da as i32 * inv / 255) / out_a;
                let out_g = (sg as i32 * a + dg as i32 * da as i32 * inv / 255) / out_a;
                let out_b = (sb as i32 * a + db as i32 * da as i32 * inv / 255) / out_a;
                dst.set_pixel(dx as u32, dy as u32, out_r as u8, out_g as u8, out_b as u8, out_a as u8);
            }
        }
    }

    dst
}

#[inline]
pub fn clamp_i32(v: i32, min: i32, max: i32) -> i32 {
    if v < min {
        min
    } else if v > max {
        max
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roll_sizes_for_progress() {
        let (l, r) = roll_sizes_for_progress(0.0);
        assert_eq!(l, 200);
        assert_eq!(r, 432);

        let (l, r) = roll_sizes_for_progress(0.5);
        assert_eq!(l, 316);
        assert_eq!(r, 316);

        let (l, r) = roll_sizes_for_progress(1.0);
        assert_eq!(l, 432);
        assert_eq!(r, 200);
    }

    #[test]
    fn test_frame_index_for_angle() {
        assert_eq!(frame_index_for_angle(0.0, 60), 0);
        assert_eq!(
            frame_index_for_angle(std::f64::consts::FRAC_PI_2, 60),
            15
        );
        assert_eq!(frame_index_for_angle(std::f64::consts::PI, 60), 30);
        assert_eq!(
            frame_index_for_angle(2.0 * std::f64::consts::PI, 60),
            0
        );
        assert_eq!(
            frame_index_for_angle(-std::f64::consts::FRAC_PI_2, 60),
            45
        );
    }

    #[test]
    fn test_quantize_roll_size() {
        assert_eq!(quantize_roll_size(LEFT_ROLL_MIN_SIZE), LEFT_ROLL_MIN_SIZE);
        assert_eq!(
            quantize_roll_size(LEFT_ROLL_MIN_SIZE + TAPEROLL_SIZE_STEP / 2 - 1),
            LEFT_ROLL_MIN_SIZE
        );
        assert_eq!(
            quantize_roll_size(LEFT_ROLL_MIN_SIZE + TAPEROLL_SIZE_STEP / 2),
            LEFT_ROLL_MIN_SIZE + TAPEROLL_SIZE_STEP
        );
        assert_eq!(quantize_roll_size(LEFT_ROLL_MAX_SIZE), LEFT_ROLL_MAX_SIZE);
        assert_eq!(quantize_roll_size(100), LEFT_ROLL_MIN_SIZE);
        assert_eq!(quantize_roll_size(999), LEFT_ROLL_MAX_SIZE);
    }

    #[test]
    fn test_roll_cache_sizes() {
        let sizes = roll_cache_sizes();
        assert_eq!(sizes.first(), Some(&LEFT_ROLL_MIN_SIZE));
        assert_eq!(sizes.last(), Some(&LEFT_ROLL_MAX_SIZE));
        let expected_len =
            ((LEFT_ROLL_MAX_SIZE - LEFT_ROLL_MIN_SIZE + TAPEROLL_SIZE_STEP - 1) / TAPEROLL_SIZE_STEP)
                as usize
                + 1;
        assert_eq!(sizes.len(), expected_len);
        for window in sizes.windows(2) {
            let diff = window[1] - window[0];
            assert!(
                diff == TAPEROLL_SIZE_STEP
                    || (window[0] + TAPEROLL_SIZE_STEP > LEFT_ROLL_MAX_SIZE
                        && window[1] == LEFT_ROLL_MAX_SIZE)
            );
        }
    }

    #[test]
    fn test_animation_cache_frame_counts_are_fixed_to_thirty() {
        assert_eq!(ROTATION_FRAME_COUNT, 30);
        assert_eq!(TAPEROLL_FRAME_COUNT, 30);
    }

    #[test]
    fn test_build_masked_cover_transparent_mask_hides() {
        let mut img = RgbaImage::new(2, 2);
        img.set_pixel(0, 0, 255, 0, 0, 255);
        img.set_pixel(1, 0, 255, 0, 0, 255);
        img.set_pixel(0, 1, 255, 0, 0, 255);
        img.set_pixel(1, 1, 255, 0, 0, 255);

        // Fully transparent mask
        let mask = RgbaImage::new(2, 2); // all zeros = transparent
        let result = build_masked_cover(&img, Some(&mask));
        // All alpha should be 0
        let (_, _, _, a) = result.pixel_at(0, 0);
        assert_eq!(a, 0);
    }

    #[test]
    fn test_build_masked_cover_opaque_mask_shows() {
        let mut img = RgbaImage::new(2, 2);
        for y in 0..2u32 {
            for x in 0..2u32 {
                img.set_pixel(x, y, 255, 0, 0, 255);
            }
        }

        let mut mask = RgbaImage::new(2, 2);
        for y in 0..2u32 {
            for x in 0..2u32 {
                mask.set_pixel(x, y, 255, 255, 255, 255); // white opaque
            }
        }

        let result = build_masked_cover(&img, Some(&mask));
        let (_, _, _, a) = result.pixel_at(0, 0);
        assert_eq!(a, 255);
    }

    #[test]
    fn test_build_masked_cover_gray_mask_halves_alpha() {
        let mut img = RgbaImage::new(2, 2);
        for y in 0..2u32 {
            for x in 0..2u32 {
                img.set_pixel(x, y, 255, 0, 0, 255);
            }
        }

        let mut mask = RgbaImage::new(2, 2);
        for y in 0..2u32 {
            for x in 0..2u32 {
                mask.set_pixel(x, y, 128, 128, 128, 255);
            }
        }

        let result = build_masked_cover(&img, Some(&mask));
        let (_, _, _, a) = result.pixel_at(0, 0);
        // luma of (128,128,128) = 128, mask_value = 128*255/255 = 128
        // final alpha = 255 * 128 / 255 = 128
        assert!((a as i32 - 128).unsigned_abs() <= 1);
    }
}
