use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};

use crate::constants::*;

pub struct FontSet {
    font: FontVec,
    pub scale_large: PxScale,  // 28pt @ 72dpi
    pub scale_small: PxScale,  // 20pt @ 72dpi
}

impl FontSet {
    pub fn load(data: Vec<u8>) -> Result<Self, String> {
        let font = FontVec::try_from_vec(data).map_err(|e| format!("parse font: {e}"))?;

        // ab_glyph PxScale is based on (ascent - descent), not units_per_em.
        // Go's opentype uses ppem (pixels per em = Size * DPI / 72).
        // To match Go's sizing: scale = desired_ppem * (ascent - descent) / units_per_em
        let upem = font.units_per_em().unwrap_or(1000.0);
        let height = font.ascent_unscaled() - font.descent_unscaled();
        let ratio = height / upem;

        Ok(Self {
            font,
            scale_large: PxScale::from(28.0 * ratio),
            scale_small: PxScale::from(20.0 * ratio),
        })
    }

    /// Measure text width in pixels.
    pub fn measure_text(&self, text: &str, scale: PxScale) -> i32 {
        let scaled = self.font.as_scaled(scale);
        let mut width = 0.0f32;
        let mut last_glyph_id = None;

        for ch in text.chars() {
            let glyph_id = scaled.glyph_id(ch);
            if let Some(last) = last_glyph_id {
                width += scaled.kern(last, glyph_id);
            }
            width += scaled.h_advance(glyph_id);
            last_glyph_id = Some(glyph_id);
        }
        width.round() as i32
    }

    /// Draw text onto a BGRA framebuffer at baseline position (x, y).
    pub fn draw_text(
        &self,
        buf: &mut [u8],
        text: &str,
        x: i32,
        y: i32,
        r: u8,
        g: u8,
        b: u8,
        scale: PxScale,
    ) {
        let scaled = self.font.as_scaled(scale);
        let mut cursor_x = x as f32;
        let mut last_glyph_id = None;

        for ch in text.chars() {
            let glyph_id = scaled.glyph_id(ch);
            if let Some(last) = last_glyph_id {
                cursor_x += scaled.kern(last, glyph_id);
            }

            let glyph = glyph_id.with_scale_and_position(scale, point(cursor_x, y as f32));

            if let Some(outlined) = self.font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                outlined.draw(|gx, gy, coverage| {
                    let px = bounds.min.x as i32 + gx as i32;
                    let py = bounds.min.y as i32 + gy as i32;
                    if px < 0 || px >= SCREEN_W as i32 || py < 0 || py >= SCREEN_H as i32 {
                        return;
                    }
                    let alpha = (coverage * 255.0) as u8;
                    if alpha == 0 {
                        return;
                    }
                    let offset = ((py as usize) * SCREEN_W + (px as usize)) * BPP;
                    if alpha == 255 {
                        buf[offset] = b;
                        buf[offset + 1] = g;
                        buf[offset + 2] = r;
                        buf[offset + 3] = 255;
                    } else {
                        // Alpha blend
                        let sa = alpha as i32;
                        let inv = 255 - sa;
                        let db = buf[offset] as i32;
                        let dg = buf[offset + 1] as i32;
                        let dr = buf[offset + 2] as i32;
                        buf[offset] = ((b as i32 * sa + db * inv) / 255) as u8;
                        buf[offset + 1] = ((g as i32 * sa + dg * inv) / 255) as u8;
                        buf[offset + 2] = ((r as i32 * sa + dr * inv) / 255) as u8;
                        buf[offset + 3] = 255;
                    }
                });
            }

            cursor_x += scaled.h_advance(glyph_id);
            last_glyph_id = Some(glyph_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_font_metrics() {
        let data = std::fs::read("/Users/charlex/Projects/trimui-spotify/package/SideB/resources/font_mono.ttf").unwrap();
        let font = FontVec::try_from_vec(data).unwrap();
        let upem = font.units_per_em().unwrap();
        let ascent = font.ascent_unscaled();
        let descent = font.descent_unscaled();
        let height = ascent - descent;
        eprintln!("units_per_em={upem}, ascent={ascent}, descent={descent}, height={height}");
        eprintln!("ratio height/upem = {}", height / upem as f32);
    }
}
