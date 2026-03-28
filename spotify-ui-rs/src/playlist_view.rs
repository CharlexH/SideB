use crate::constants::*;
use crate::drawing;
use crate::favorites::FavoriteEntry;
use crate::font::FontSet;

/// Render the full-screen playlist overlay onto the back buffer.
pub fn render_playlist_overlay(
    buf: &mut [u8],
    entries: &[FavoriteEntry],
    selected: usize,
    playing_uri: Option<&str>,
    fonts: &FontSet,
) {
    // Black background
    drawing::fill_rect(buf, 0, 0, SCREEN_W as i32, SCREEN_H as i32, 0, 0, 0, 255);

    // Header
    let title = format!("FAVORITES ({})", entries.len());
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
            if is_playing {
                // Small triangle ▶
                let tri_x = text_start_x;
                let tri_y = item_y + PLAYLIST_ITEM_HEIGHT / 2;
                for dy in -5i32..=5 {
                    let width = 5 - dy.abs();
                    for dx in 0..width {
                        drawing::blend_pixel(
                            buf,
                            tri_x + dx,
                            tri_y + dy,
                            100,
                            255,
                            100,
                            255,
                        );
                    }
                }
            }

            let name_x = text_start_x + 16;

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
                item_y + 30,
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
                item_y + 30,
                art_r,
                art_g,
                art_b,
                fonts.scale_large,
            );

            // Download status indicator (right edge)
            let indicator_x = PLAYLIST_X + PLAYLIST_W - 32;
            let indicator_y = item_y + PLAYLIST_ITEM_HEIGHT / 2;
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
    let footer_y = SCREEN_H as i32 - PLAYLIST_MARGIN - 4;
    drawing::fill_rect(
        buf,
        PLAYLIST_X,
        footer_y - PLAYLIST_FOOTER_HEIGHT,
        PLAYLIST_W,
        1,
        100,
        100,
        100,
        255,
    );

    let hints = "UP/DOWN Navigate    A Play    X Delete    B Close";
    let hints_w = fonts.measure_text(hints, fonts.scale_small);
    let hints_x = (SCREEN_W as i32 - hints_w) / 2;
    fonts.draw_text(
        buf,
        hints,
        hints_x,
        footer_y - 8,
        150,
        150,
        150,
        fonts.scale_small,
    );
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
