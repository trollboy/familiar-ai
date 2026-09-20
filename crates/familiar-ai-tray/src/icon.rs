use familiar_ai_core::FamiliarError;

const ICON_BYTES: &[u8] = include_bytes!("../assets/icon.png");

pub struct LoadedIcon {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub fn load_icon() -> Result<LoadedIcon, FamiliarError> {
    let img = image::load_from_memory(ICON_BYTES)
        .map_err(|e| FamiliarError::Config(format!("failed to decode tray icon: {e}")))?
        .into_rgba8();
    let (width, height) = img.dimensions();
    Ok(LoadedIcon {
        rgba: img.into_raw(),
        width,
        height,
    })
}

pub fn load_tray_icon() -> Result<tray_icon::Icon, FamiliarError> {
    tray_icon_with_count(0)
}

/// The tray icon carrying the pending-gate count. Zero is the base icon
/// unchanged.
pub fn tray_icon_with_count(count: usize) -> Result<tray_icon::Icon, FamiliarError> {
    let loaded = compose_with_count(&load_icon()?, count);
    tray_icon::Icon::from_rgba(loaded.rgba, loaded.width, loaded.height)
        .map_err(|e| FamiliarError::Config(format!("failed to build tray icon: {e}")))
}

/// How many pending gates the badge will render before switching to the cap
/// form. A count wider than two glyphs stops being legible at tray size.
pub const BADGE_CAP: usize = 9;

/// 3x5 glyphs for the digits and the cap's `+`. A bitmap font costs nothing
/// and keeps this crate's dependency list where it is; a real font crate
/// would be a new system-facing dependency for eleven shapes.
const GLYPH_ROWS: usize = 5;
const GLYPH_COLS: usize = 3;
const GLYPHS: [(char, [u8; GLYPH_ROWS]); 11] = [
    ('0', [0b111, 0b101, 0b101, 0b101, 0b111]),
    ('1', [0b010, 0b110, 0b010, 0b010, 0b111]),
    ('2', [0b111, 0b001, 0b111, 0b100, 0b111]),
    ('3', [0b111, 0b001, 0b111, 0b001, 0b111]),
    ('4', [0b101, 0b101, 0b111, 0b001, 0b001]),
    ('5', [0b111, 0b100, 0b111, 0b001, 0b111]),
    ('6', [0b111, 0b100, 0b111, 0b101, 0b111]),
    ('7', [0b111, 0b001, 0b001, 0b001, 0b001]),
    ('8', [0b111, 0b101, 0b111, 0b101, 0b111]),
    ('9', [0b111, 0b101, 0b111, 0b001, 0b111]),
    ('+', [0b000, 0b010, 0b111, 0b010, 0b000]),
];

fn glyph(symbol: char) -> Option<[u8; GLYPH_ROWS]> {
    GLYPHS
        .iter()
        .find(|(candidate, _)| *candidate == symbol)
        .map(|(_, rows)| *rows)
}

/// What the badge spells for a count. Above the cap it says `9+` rather than
/// growing: the exact number past that point is the list's job, not the
/// icon's.
pub fn badge_text(count: usize) -> String {
    if count > BADGE_CAP {
        format!("{BADGE_CAP}+")
    } else {
        count.to_string()
    }
}

/// Draw the pending-gate count onto the tray icon.
///
/// Zero returns the base icon unchanged — byte for byte. A surface that
/// signals when nothing is wrong is a surface its operator stops reading, so
/// the quiet state must be genuinely identical rather than merely similar.
pub fn compose_with_count(base: &LoadedIcon, count: usize) -> LoadedIcon {
    if count == 0 {
        return LoadedIcon {
            rgba: base.rgba.clone(),
            width: base.width,
            height: base.height,
        };
    }

    let mut rgba = base.rgba.clone();
    let (w, h) = (base.width as i64, base.height as i64);
    let text = badge_text(count);

    // Badge geometry, proportional so this survives an asset resize — the
    // icon silently grew from 32x32 to 512x512 once already.
    let radius = (w.min(h) as f64 * 0.22) as i64;
    let cx = w - radius - (w as f64 * 0.04) as i64;
    let cy = h - radius - (h as f64 * 0.04) as i64;

    let mut put = |x: i64, y: i64, colour: [u8; 4]| {
        if x < 0 || y < 0 || x >= w || y >= h {
            return;
        }
        let i = ((y * w + x) * 4) as usize;
        rgba[i..i + 4].copy_from_slice(&colour);
    };

    // Filled disc with a light rim so the badge reads against a dark tray as
    // well as a light one.
    const FILL: [u8; 4] = [0xD6, 0x3B, 0x2F, 0xFF];
    const RIM: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];
    let rim = (radius as f64 * 0.12).max(1.0) as i64;
    for y in (cy - radius)..=(cy + radius) {
        for x in (cx - radius)..=(cx + radius) {
            let d2 = (x - cx).pow(2) + (y - cy).pow(2);
            if d2 <= radius.pow(2) {
                put(
                    x,
                    y,
                    if d2 >= (radius - rim).pow(2) {
                        RIM
                    } else {
                        FILL
                    },
                );
            }
        }
    }

    // Glyphs, centred in the disc.
    let scale = ((radius * 2) as f64 * 0.46 / GLYPH_ROWS as f64).max(1.0) as i64;
    let advance = (GLYPH_COLS as i64 + 1) * scale;
    let text_w = advance * text.chars().count() as i64 - scale;
    let mut pen_x = cx - text_w / 2;
    let pen_y = cy - (GLYPH_ROWS as i64 * scale) / 2;
    for symbol in text.chars() {
        if let Some(rows) = glyph(symbol) {
            for (row, bits) in rows.iter().enumerate() {
                for col in 0..GLYPH_COLS {
                    if bits & (1 << (GLYPH_COLS - 1 - col)) == 0 {
                        continue;
                    }
                    for dy in 0..scale {
                        for dx in 0..scale {
                            put(
                                pen_x + col as i64 * scale + dx,
                                pen_y + row as i64 * scale + dy,
                                RIM,
                            );
                        }
                    }
                }
            }
        }
        pen_x += advance;
    }

    LoadedIcon {
        rgba,
        width: base.width,
        height: base.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_icon_successfully() {
        let icon = load_icon().unwrap();
        assert!(icon.width > 0);
        assert!(icon.height > 0);
        assert_eq!(icon.rgba.len() as u32, icon.width * icon.height * 4);
    }

    /// Pins the shipped asset so an accidental swap is caught. The pin read
    /// 32x32 until it was checked against the file for the first time in a
    /// long while: the asset became 512x512 in ddd3d7a and nothing noticed,
    /// because this crate sits outside the workspace and its tests were never
    /// part of any run. The tray host scales the icon, so 512 is fine — the
    /// stale number was the defect, not the artwork.
    #[test]
    fn icon_has_expected_dimensions() {
        let icon = load_icon().unwrap();
        assert_eq!(icon.width, 512);
        assert_eq!(icon.height, 512);
    }
}

#[cfg(test)]
mod badge_tests {
    use super::*;

    #[test]
    fn zero_pending_gates_leaves_the_icon_byte_for_byte_unchanged() {
        // A surface that signals when nothing is wrong trains its operator to
        // ignore it. Quiet must be genuinely identical, not merely similar.
        let base = load_icon().unwrap();
        let composed = compose_with_count(&base, 0);
        assert_eq!(composed.rgba, base.rgba);
        assert_eq!(composed.width, base.width);
        assert_eq!(composed.height, base.height);
    }

    #[test]
    fn a_pending_gate_changes_the_icon() {
        let base = load_icon().unwrap();
        for count in [1, 2, 9, 10, 250] {
            let composed = compose_with_count(&base, count);
            assert_ne!(
                composed.rgba, base.rgba,
                "a count of {count} must be visible on the icon"
            );
            assert_eq!(composed.width, base.width, "geometry must not change");
            assert_eq!(composed.rgba.len(), base.rgba.len());
        }
    }

    #[test]
    fn counts_past_the_cap_render_the_cap_form_rather_than_overflowing() {
        assert_eq!(badge_text(1), "1");
        assert_eq!(badge_text(9), "9");
        assert_eq!(badge_text(10), "9+");
        assert_eq!(badge_text(4000), "9+");
        // Every glyph the badge can ask for must exist, or a count would
        // silently render blank.
        for count in [0, 1, 5, 9, 10, 99] {
            for symbol in badge_text(count).chars() {
                assert!(glyph(symbol).is_some(), "no glyph for {symbol:?}");
            }
        }
    }

    #[test]
    fn different_counts_are_distinguishable_from_each_other() {
        // One pending gate and two must not look the same, or the count is
        // decoration rather than information.
        let base = load_icon().unwrap();
        let one = compose_with_count(&base, 1);
        let two = compose_with_count(&base, 2);
        assert_ne!(one.rgba, two.rgba);
        // And everything past the cap is deliberately the same picture.
        assert_eq!(
            compose_with_count(&base, 10).rgba,
            compose_with_count(&base, 400).rgba
        );
    }
}
