use crate::constants::*;
use crate::drawing;
use crate::favorites::FavoriteEntry;
use crate::font::FontSet;

struct PlaylistItemLayout {
    text_y: i32,
    indicator_y: i32,
    triangle_center_y: i32,
    triangle_height: i32,
    triangle_width: i32,
    triangle_text_gap: i32,
}

struct PlaylistFooterStyle {
    text_y: i32,
    color: (u8, u8, u8),
    use_small_font: bool,
}

/// Render the full-screen playlist overlay onto the back buffer.
pub fn render_playlist_overlay(
    buf: &mut [u8],
    entries: &[FavoriteEntry],
    selected: usize,
    playing_uri: Option<&str>,
    confirm_message: Option<&str>,
    fonts: &FontSet,
) {
    // Black background
    drawing::fill_rect(buf, 0, 0, SCREEN_W as i32, SCREEN_H as i32, 0, 0, 0, 255);

    // Header
    let title = playlist_title(entries.len());
    let title_w = fonts.measure_text(&title, fonts.scale_large);
    let title_x = (SCREEN_W as i32 - title_w) / 2;
    fonts.draw_text(
        buf,
        &title,
        title_x,
        PLAYLIST_Y + 40,
        255,
        255,
        255,
        fonts.scale_large,
    );

    // Header underline
    drawing::fill_rect(
        buf,
        PLAYLIST_X,
        PLAYLIST_Y + PLAYLIST_HEADER_HEIGHT - 2,
        PLAYLIST_W,
        1,
        100,
        100,
        100,
        255,
    );

    if entries.is_empty() {
        // Empty state
        let msg = "No favorites yet. Press X to add songs.";
        let msg_w = fonts.measure_text(msg, fonts.scale_large);
        let msg_x = (SCREEN_W as i32 - msg_w) / 2;
        fonts.draw_text(
            buf,
            msg,
            msg_x,
            SCREEN_H as i32 / 2,
            150,
            150,
            150,
            fonts.scale_large,
        );
    } else {
        // Calculate scroll offset to keep selection visible and centered
        let scroll_offset = if entries.len() <= PLAYLIST_VISIBLE_ITEMS {
            0
        } else if selected < PLAYLIST_VISIBLE_ITEMS / 2 {
            0
        } else if selected >= entries.len() - PLAYLIST_VISIBLE_ITEMS / 2 {
            entries.len() - PLAYLIST_VISIBLE_ITEMS
        } else {
            selected - PLAYLIST_VISIBLE_ITEMS / 2
        };

        let list_y_start = PLAYLIST_Y + PLAYLIST_HEADER_HEIGHT + 4;

        for i in 0..PLAYLIST_VISIBLE_ITEMS {
            let entry_idx = scroll_offset + i;
            if entry_idx >= entries.len() {
                break;
            }

            let entry = &entries[entry_idx];
            let item_y = list_y_start + (i as i32) * PLAYLIST_ITEM_HEIGHT;
            let layout = playlist_item_layout(item_y);

            // Highlight selected item
            if entry_idx == selected {
                drawing::fill_rect(
                    buf,
                    PLAYLIST_X,
                    item_y,
                    PLAYLIST_W,
                    PLAYLIST_ITEM_HEIGHT - 2,
                    255,
                    255,
                    255,
                    35,
                );
            }

            // Playing indicator
            let text_start_x = PLAYLIST_X + 16;
            let is_playing = playing_uri.map_or(false, |uri| uri == entry.uri);
            let triangle_color = playlist_triangle_color(entry_idx == selected);
            if is_playing {
                draw_playlist_play_triangle(buf, text_start_x, &layout, triangle_color);
            }

            let name_x = playlist_name_x(text_start_x, &layout);

            // Track name (truncated) — black text when selected, white otherwise
            let display_name = truncate_str(&entry.name, 28);
            let (name_r, name_g, name_b) = if entry_idx == selected {
                (0, 0, 0)
            } else {
                (255, 255, 255)
            };
            fonts.draw_text(
                buf,
                &display_name,
                name_x,
                layout.text_y,
                name_r,
                name_g,
                name_b,
                fonts.scale_large,
            );

            // Artist (right-aligned, gray — darker when selected)
            let artist_display = truncate_str(&entry.artist, 16);
            let artist_w = fonts.measure_text(&artist_display, fonts.scale_large);
            let artist_x = PLAYLIST_X + PLAYLIST_W - 60 - artist_w;
            let (art_r, art_g, art_b) = if entry_idx == selected {
                (60, 60, 60)
            } else {
                (170, 170, 170)
            };
            fonts.draw_text(
                buf,
                &artist_display,
                artist_x,
                layout.text_y,
                art_r,
                art_g,
                art_b,
                fonts.scale_large,
            );

            // Download status indicator (right edge)
            let indicator_x = PLAYLIST_X + PLAYLIST_W - 32;
            let indicator_y = layout.indicator_y;
            if entry.downloaded {
                // Green filled circle
                draw_circle_filled(buf, indicator_x, indicator_y, 6, 80, 200, 80, 255);
            } else {
                // Gray hollow circle
                draw_circle_outline(buf, indicator_x, indicator_y, 6, 120, 120, 120, 180);
            }
        }

        // Scroll indicator if needed
        if entries.len() > PLAYLIST_VISIBLE_ITEMS {
            let bar_h = PLAYLIST_VISIBLE_ITEMS as i32 * PLAYLIST_ITEM_HEIGHT;
            let thumb_h = (bar_h * PLAYLIST_VISIBLE_ITEMS as i32 / entries.len() as i32).max(20);
            let track_h = bar_h - thumb_h;
            let thumb_offset = if entries.len() > PLAYLIST_VISIBLE_ITEMS {
                track_h * scroll_offset as i32 / (entries.len() - PLAYLIST_VISIBLE_ITEMS) as i32
            } else {
                0
            };

            let bar_x = PLAYLIST_X + PLAYLIST_W - 6;
            let bar_y = list_y_start;

            // Track
            drawing::fill_rect(buf, bar_x, bar_y, 4, bar_h, 60, 60, 60, 200);
            // Thumb
            drawing::fill_rect(
                buf,
                bar_x,
                bar_y + thumb_offset,
                4,
                thumb_h,
                180,
                180,
                180,
                220,
            );
        }
    }

    // Footer with control hints
    let footer_style = playlist_footer_style_for(confirm_message);
    drawing::fill_rect(
        buf,
        PLAYLIST_X,
        playlist_footer_divider_y(),
        PLAYLIST_W,
        1,
        100,
        100,
        100,
        255,
    );

    let hints = confirm_message.unwrap_or(playlist_footer_hints());
    let hints_scale = if footer_style.use_small_font {
        fonts.scale_small
    } else {
        fonts.scale_large
    };
    let hints_w = fonts.measure_text(hints, hints_scale);
    let hints_x = (SCREEN_W as i32 - hints_w) / 2;
    fonts.draw_text(
        buf,
        hints,
        hints_x,
        footer_style.text_y,
        footer_style.color.0,
        footer_style.color.1,
        footer_style.color.2,
        hints_scale,
    );
}

fn playlist_item_layout(item_y: i32) -> PlaylistItemLayout {
    PlaylistItemLayout {
        text_y: item_y + 32,
        indicator_y: item_y + PLAYLIST_ITEM_HEIGHT / 2 - 2,
        triangle_center_y: item_y + PLAYLIST_ITEM_HEIGHT / 2 - 1,
        triangle_height: 20,
        triangle_width: 10,
        triangle_text_gap: 8,
    }
}

fn playlist_name_x(triangle_x: i32, layout: &PlaylistItemLayout) -> i32 {
    triangle_x + layout.triangle_width + layout.triangle_text_gap
}

fn draw_playlist_play_triangle(
    buf: &mut [u8],
    triangle_x: i32,
    layout: &PlaylistItemLayout,
    color: (u8, u8, u8),
) {
    let tri_half_height = layout.triangle_height / 2;
    for dy in -tri_half_height..=tri_half_height {
        let row_width = triangle_row_width(dy, layout);
        for dx in 0..row_width {
            if triangle_pixel_visible(dx, dy, layout) {
                drawing::blend_pixel(
                    buf,
                    triangle_x + dx,
                    layout.triangle_center_y + dy,
                    color.0,
                    color.1,
                    color.2,
                    255,
                );
            }
        }
    }
}

fn playlist_triangle_color(selected: bool) -> (u8, u8, u8) {
    if selected {
        (0, 0, 0)
    } else {
        (255, 255, 255)
    }
}

fn triangle_row_width(dy: i32, layout: &PlaylistItemLayout) -> i32 {
    let tri_half_height = layout.triangle_height / 2;
    ((tri_half_height + 1 - dy.abs()) * layout.triangle_width) / (tri_half_height + 1)
}

fn triangle_pixel_visible(dx: i32, dy: i32, layout: &PlaylistItemLayout) -> bool {
    let tri_half_height = layout.triangle_height / 2;
    if dy < -tri_half_height || dy > tri_half_height {
        return false;
    }

    let row_width = triangle_row_width(dy, layout);
    if dx < 0 || dx >= row_width {
        return false;
    }

    let top_corner = dy <= -tri_half_height + 1 && dx == 0;
    let bottom_corner = dy >= tri_half_height - 1 && dx == 0;
    let tip_corner = dx == row_width - 1 && dy.abs() <= 1;

    !(top_corner || bottom_corner || tip_corner)
}

fn playlist_title(count: usize) -> String {
    format!("FAV LIST ({count})")
}

fn playlist_footer_hints() -> &'static str {
    "NAVIGATE (↑/↓)   PLAY (A)   DELETE (X)   BACK (B)"
}

fn playlist_footer_style() -> PlaylistFooterStyle {
    playlist_footer_style_for(None)
}

fn playlist_footer_style_for(confirm_message: Option<&str>) -> PlaylistFooterStyle {
    let color = if confirm_message.is_some() {
        (255, 255, 255)
    } else {
        (0x3D, 0x3D, 0x3D)
    };

    PlaylistFooterStyle {
        text_y: HINTS_BASELINE_Y,
        color,
        use_small_font: true,
    }
}

fn playlist_footer_divider_y() -> i32 {
    SCREEN_H as i32 - PLAYLIST_MARGIN - 4 - PLAYLIST_FOOTER_HEIGHT + 12
}

/// Truncate a string to max_chars, appending "..." if truncated.
fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars - 3).collect();
        format!("{}...", truncated)
    }
}

fn draw_circle_filled(buf: &mut [u8], cx: i32, cy: i32, r: i32, red: u8, g: u8, b: u8, a: u8) {
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r * r {
                drawing::blend_pixel(buf, cx + dx, cy + dy, red, g, b, a);
            }
        }
    }
}

fn draw_circle_outline(buf: &mut [u8], cx: i32, cy: i32, r: i32, red: u8, g: u8, b: u8, a: u8) {
    for dy in -r..=r {
        for dx in -r..=r {
            let dist_sq = dx * dx + dy * dy;
            let inner = (r - 1) * (r - 1);
            let outer = r * r;
            if dist_sq >= inner && dist_sq <= outer {
                drawing::blend_pixel(buf, cx + dx, cy + dy, red, g, b, a);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_item_layout_offsets_match_visual_tuning() {
        let item_y = 200;
        let layout = playlist_item_layout(item_y);

        assert_eq!(layout.text_y, item_y + 32);
        assert_eq!(layout.indicator_y, item_y + PLAYLIST_ITEM_HEIGHT / 2 - 2);
        assert_eq!(
            layout.triangle_center_y,
            item_y + PLAYLIST_ITEM_HEIGHT / 2 - 1
        );
        assert_eq!(layout.triangle_height, 20);
        assert_eq!(layout.triangle_width, 10);
        assert_eq!(layout.triangle_text_gap, 8);
    }

    #[test]
    fn playlist_triangle_keeps_eight_pixel_gap_before_text() {
        let layout = playlist_item_layout(200);

        assert_eq!(playlist_name_x(64, &layout), 82);
    }

    #[test]
    fn rounded_triangle_clips_all_three_corners() {
        let layout = playlist_item_layout(200);

        assert!(!triangle_pixel_visible(0, -9, &layout));
        assert!(!triangle_pixel_visible(0, 9, &layout));
        assert!(!triangle_pixel_visible(
            layout.triangle_width - 1,
            0,
            &layout
        ));
        assert!(triangle_pixel_visible(1, -8, &layout));
        assert!(triangle_pixel_visible(
            layout.triangle_width - 2,
            0,
            &layout
        ));
    }

    #[test]
    fn playlist_triangle_color_matches_track_name_state() {
        assert_eq!(playlist_triangle_color(false), (255, 255, 255));
        assert_eq!(playlist_triangle_color(true), (0, 0, 0));
    }

    #[test]
    fn playlist_overlay_copy_matches_updated_ui_language() {
        assert_eq!(playlist_title(3), "FAV LIST (3)");
        assert_eq!(
            playlist_footer_hints(),
            "NAVIGATE (↑/↓)   PLAY (A)   DELETE (X)   BACK (B)"
        );
    }

    #[test]
    fn playlist_footer_style_matches_playback_auxiliary_text() {
        let style = playlist_footer_style();

        assert_eq!(style.text_y, HINTS_BASELINE_Y);
        assert_eq!(style.color, (0x3D, 0x3D, 0x3D));
        assert!(style.use_small_font);
    }

    #[test]
    fn playlist_footer_divider_moves_down_twelve_pixels() {
        assert_eq!(
            playlist_footer_divider_y(),
            SCREEN_H as i32 - PLAYLIST_MARGIN - 4 - PLAYLIST_FOOTER_HEIGHT + 12
        );
    }

    #[test]
    fn playlist_confirmation_footer_uses_white_text() {
        let normal = playlist_footer_style();
        let confirm = playlist_footer_style_for(Some("Press X again to delete"));

        assert_eq!(normal.color, (0x3D, 0x3D, 0x3D));
        assert_eq!(confirm.color, (255, 255, 255));
        assert_eq!(confirm.text_y, HINTS_BASELINE_Y);
        assert!(confirm.use_small_font);
    }
}
