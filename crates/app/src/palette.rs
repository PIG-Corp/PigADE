//! Resolve host-serialized terminal colors for the native cell renderer.
//!
//! `Frame::palette_overrides` is sorted by color index and contains only
//! explicitly set terminal colors. An empty slice therefore restores the
//! renderer's built-in defaults without requiring the host to send the
//! defaults as overrides.

const ANSI: [u32; 16] = [
    0x111317, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4, 0x5c6370,
    0xff808b, 0xb4e88c, 0xffd68a, 0x88c0ff, 0xdf9fff, 0x7bdde4, 0xffffff,
];
const DEFAULT_FOREGROUND: u32 = 0xf2f4f7;
const DEFAULT_BACKGROUND: u32 = 0x111317;
const MAX_COLOR_TEXT_BYTES: usize = 64;
const MAX_NAMED_COLOR_INDEX: u16 = 268;

/// Resolve a debug-formatted host cell color into packed `0xRRGGBB`.
///
/// Overrides must be sorted by their unique color index, as they are in a
/// terminal `Frame`. Parsing is strict and bounded; malformed or unknown
/// strings use the same foreground/background fallback as the original app
/// renderer. Dim named colors default to their corresponding normal ANSI color
/// scaled by 66%, while an explicit host override takes precedence.
pub(crate) fn color(value: &str, foreground: bool, overrides: &[(u16, [u8; 3])]) -> u32 {
    if value.len() > MAX_COLOR_TEXT_BYTES {
        return fallback(foreground);
    }

    if let Some(rgb) = parse_spec(value) {
        return pack(rgb);
    }

    if let Some(index) = parse_indexed(value) {
        if let Some(rgb) = override_color(index as u16, overrides) {
            return pack(rgb);
        }
        return indexed_color(index);
    }

    if let Some(index) = parse_named_index(value) {
        if let Some(rgb) = override_color(index, overrides) {
            return pack(rgb);
        }
        return named_default(index, foreground);
    }

    fallback(foreground)
}

fn fallback(foreground: bool) -> u32 {
    if foreground {
        DEFAULT_FOREGROUND
    } else {
        DEFAULT_BACKGROUND
    }
}

fn pack(rgb: [u8; 3]) -> u32 {
    (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2])
}

fn override_color(index: u16, overrides: &[(u16, [u8; 3])]) -> Option<[u8; 3]> {
    if index > MAX_NAMED_COLOR_INDEX {
        return None;
    }
    overrides
        .binary_search_by_key(&index, |(override_index, _)| *override_index)
        .ok()
        .map(|position| overrides[position].1)
}

fn parse_spec(value: &str) -> Option<[u8; 3]> {
    let rest = value.strip_prefix("Spec(Rgb { r: ")?;
    let (r, rest) = rest.split_once(", g: ")?;
    let (g, rest) = rest.split_once(", b: ")?;
    let b = rest.strip_suffix(" })")?;
    Some([parse_u8(r)?, parse_u8(g)?, parse_u8(b)?])
}

fn parse_indexed(value: &str) -> Option<u8> {
    let index = value.strip_prefix("Indexed(")?.strip_suffix(')')?;
    parse_u8(index)
}

fn parse_u8(value: &str) -> Option<u8> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 3
        || (bytes.len() > 1 && bytes[0] == b'0')
        || !bytes.iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    value.parse().ok()
}

fn parse_named_index(value: &str) -> Option<u16> {
    let name = value.strip_prefix("Named(")?.strip_suffix(')')?;
    Some(match name {
        "Black" => 0,
        "Red" => 1,
        "Green" => 2,
        "Yellow" => 3,
        "Blue" => 4,
        "Magenta" => 5,
        "Cyan" => 6,
        "White" => 7,
        "BrightBlack" => 8,
        "BrightRed" => 9,
        "BrightGreen" => 10,
        "BrightYellow" => 11,
        "BrightBlue" => 12,
        "BrightMagenta" => 13,
        "BrightCyan" => 14,
        "BrightWhite" => 15,
        "Foreground" => 256,
        "Background" => 257,
        "Cursor" => 258,
        "DimBlack" => 259,
        "DimRed" => 260,
        "DimGreen" => 261,
        "DimYellow" => 262,
        "DimBlue" => 263,
        "DimMagenta" => 264,
        "DimCyan" => 265,
        "DimWhite" => 266,
        "BrightForeground" => 267,
        "DimForeground" => 268,
        _ => return None,
    })
}

fn named_default(index: u16, foreground: bool) -> u32 {
    match index {
        0..=15 => ANSI[index as usize],
        256 => DEFAULT_FOREGROUND,
        257 => DEFAULT_BACKGROUND,
        259..=266 => dim(ANSI[(index - 259) as usize]),
        267 => DEFAULT_FOREGROUND,
        268 => dim(DEFAULT_FOREGROUND),
        _ => fallback(foreground), // Cursor uses the caller's foreground context.
    }
}

fn dim(color: u32) -> u32 {
    let scale = |channel: u32| (channel * 66 + 50) / 100;
    (scale((color >> 16) & 0xff) << 16) | (scale((color >> 8) & 0xff) << 8) | scale(color & 0xff)
}

fn indexed_color(index: u8) -> u32 {
    match index {
        0..=15 => ANSI[index as usize],
        16..=231 => {
            let cube = u32::from(index - 16);
            let channel = |value: u32| if value == 0 { 0 } else { 55 + value * 40 };
            (channel(cube / 36) << 16) | (channel(cube / 6 % 6) << 8) | channel(cube % 6)
        }
        232..=255 => {
            let value = 8 + (u32::from(index) - 232) * 10;
            (value << 16) | (value << 8) | value
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OVERRIDES: &[(u16, [u8; 3])] = &[
        (1, [1, 2, 3]),
        (256, [4, 5, 6]),
        (257, [7, 8, 9]),
        (258, [10, 11, 12]),
        (259, [13, 14, 15]),
        (260, [16, 17, 18]),
        (261, [19, 20, 21]),
        (262, [22, 23, 24]),
        (263, [25, 26, 27]),
        (264, [28, 29, 30]),
        (265, [31, 32, 33]),
        (266, [34, 35, 36]),
        (267, [37, 38, 39]),
        (268, [40, 41, 42]),
    ];

    #[test]
    fn named_and_indexed_colors_share_explicit_override() {
        assert_eq!(color("Named(Red)", true, OVERRIDES), 0x010203);
        assert_eq!(color("Indexed(1)", false, OVERRIDES), 0x010203);
    }

    #[test]
    fn named_special_overrides_ignore_foreground_context() {
        for (value, expected) in [
            ("Named(Foreground)", 0x040506),
            ("Named(Background)", 0x070809),
            ("Named(Cursor)", 0x0a0b0c),
            ("Named(DimBlack)", 0x0d0e0f),
            ("Named(DimRed)", 0x101112),
            ("Named(DimGreen)", 0x131415),
            ("Named(DimYellow)", 0x161718),
            ("Named(DimBlue)", 0x191a1b),
            ("Named(DimMagenta)", 0x1c1d1e),
            ("Named(DimCyan)", 0x1f2021),
            ("Named(DimWhite)", 0x222324),
            ("Named(BrightForeground)", 0x252627),
            ("Named(DimForeground)", 0x28292a),
        ] {
            assert_eq!(color(value, true, OVERRIDES), expected);
            assert_eq!(color(value, false, OVERRIDES), expected);
        }
    }

    #[test]
    fn empty_overrides_restore_baseline_and_special_fallbacks() {
        assert_eq!(color("Named(Red)", true, &[]), ANSI[1]);
        assert_eq!(color("Indexed(1)", false, &[]), ANSI[1]);
        assert_eq!(color("Named(Foreground)", false, &[]), DEFAULT_FOREGROUND);
        assert_eq!(color("Named(Background)", true, &[]), DEFAULT_BACKGROUND);
        assert_eq!(color("Named(Cursor)", true, &[]), DEFAULT_FOREGROUND);
        assert_eq!(color("Named(DimRed)", false, &[]), dim(ANSI[1]));
        assert_eq!(
            color("Named(DimForeground)", false, &[]),
            dim(DEFAULT_FOREGROUND)
        );
        assert_eq!(color("invalid", true, &[]), 0xf2f4f7);
        assert_eq!(color("invalid", false, &[]), 0x111317);
    }

    #[test]
    fn truecolor_bypasses_indexed_and_named_overrides() {
        assert_eq!(
            color("Spec(Rgb { r: 1, g: 2, b: 3 })", true, OVERRIDES),
            0x010203
        );
        assert_eq!(
            color("Spec(Rgb { r: 255, g: 0, b: 127 })", false, OVERRIDES),
            0xff007f
        );
    }

    #[test]
    fn strict_malformed_and_out_of_range_values_fall_back_without_panicking() {
        for invalid in [
            "Spec(Rgb { r: 256, g: 2, b: 3 })",
            "Spec(Rgb { r: 1, g: 2 })",
            "Spec(Rgb { r: 01, g: 2, b: 3 })",
            "Spec(Rgb { r: 1, g: 2, b: 3 })junk",
            "Indexed(256)",
            "Indexed(-1)",
            "Indexed(01)",
            "Named(DimForeground)junk",
            "Named(NotAColor)",
            "x".repeat(MAX_COLOR_TEXT_BYTES + 1).as_str(),
        ] {
            assert_eq!(
                color(invalid, true, OVERRIDES),
                DEFAULT_FOREGROUND,
                "{invalid}"
            );
            assert_eq!(
                color(invalid, false, OVERRIDES),
                DEFAULT_BACKGROUND,
                "{invalid}"
            );
        }
    }

    #[test]
    fn xterm_cube_and_gray_ramps_include_endpoints() {
        assert_eq!(color("Indexed(16)", true, &[]), 0x000000);
        assert_eq!(color("Indexed(21)", true, &[]), 0x0000ff);
        assert_eq!(color("Indexed(196)", true, &[]), 0xff0000);
        assert_eq!(color("Indexed(231)", true, &[]), 0xffffff);
        assert_eq!(color("Indexed(232)", true, &[]), 0x080808);
        assert_eq!(color("Indexed(255)", true, &[]), 0xeeeeee);
    }

    #[test]
    fn special_override_indices_use_the_full_named_color_range() {
        assert_eq!(override_color(268, OVERRIDES), Some([40, 41, 42]));
        assert_eq!(override_color(269, OVERRIDES), None);
        assert_eq!(parse_named_index("Named(DimWhite)"), Some(266));
    }
}
