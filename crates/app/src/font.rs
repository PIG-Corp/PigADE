//! Pinned terminal-rendering choices for the GPUI cell painter.
//!
//! This is deliberately separate from the host's semantic CORE_PROFILE: fonts
//! affect pixels, not parsing, journal identity, or checkpoint compatibility.
use std::{borrow::Cow, sync::Arc};

use gpui::{App, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, font};

pub const PROFILE_ID: &str = "pigade-terminal-render-profile-aliases-v1";
pub const PRIMARY_FAMILY: &str = "PigADE Terminal Mono v1";
pub const CJK_FAMILY: &str = "PigADE Terminal CJK JP v1";
pub const SIZE: f32 = 14.;

fn features() -> FontFeatures {
    FontFeatures(Arc::new(vec![("liga".into(), 0), ("calt".into(), 0)]))
}

const REGULAR: &[u8] =
    include_bytes!("../assets/fonts/derived-terminal-v1/PigADETerminalMonoV1-Regular.ttf");
const BOLD: &[u8] =
    include_bytes!("../assets/fonts/derived-terminal-v1/PigADETerminalMonoV1-Bold.ttf");
const ITALIC: &[u8] =
    include_bytes!("../assets/fonts/derived-terminal-v1/PigADETerminalMonoV1-Italic.ttf");
const BOLD_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/derived-terminal-v1/PigADETerminalMonoV1-BoldItalic.ttf");
const CJK_JP_REGULAR: &[u8] =
    include_bytes!("../assets/fonts/derived-terminal-v1/PigADETerminalCJKJPV1-Regular.otf");

/// Register deterministic PigADE-only family aliases for the bundled static
/// faces. The aliases are checked before loading so a preexisting family cannot
/// become a source-selection tie with these embedded bytes.
pub fn install(app: &App) -> Result<(), String> {
    let existing_names = app.text_system().all_font_names();
    for family in [PRIMARY_FAMILY, CJK_FAMILY] {
        if existing_names.iter().any(|name| name == family) {
            return Err(format!(
                "{PROFILE_ID}: font family alias {family} is already registered"
            ));
        }
    }

    app.text_system()
        .add_fonts(vec![
            Cow::Borrowed(REGULAR),
            Cow::Borrowed(BOLD),
            Cow::Borrowed(ITALIC),
            Cow::Borrowed(BOLD_ITALIC),
            Cow::Borrowed(CJK_JP_REGULAR),
        ])
        .map_err(|error| {
            format!("{PROFILE_ID}: failed to register embedded terminal fonts: {error}")
        })?;

    let names = app.text_system().all_font_names();
    if !names.iter().any(|name| name == PRIMARY_FAMILY) {
        return Err(format!(
            "{PROFILE_ID}: bundled font family {PRIMARY_FAMILY} did not register"
        ));
    }
    if !names.iter().any(|name| name == CJK_FAMILY) {
        return Err(format!(
            "{PROFILE_ID}: bundled fallback family {CJK_FAMILY} did not register"
        ));
    }

    for (weight, style) in [
        (FontWeight::NORMAL, FontStyle::Normal),
        (FontWeight::BOLD, FontStyle::Normal),
        (FontWeight::NORMAL, FontStyle::Italic),
        (FontWeight::BOLD, FontStyle::Italic),
    ] {
        let mut face = terminal_font();
        face.weight = weight;
        face.style = style;
        app.text_system().resolve_font(&face);
    }
    let mut cjk_face = font(CJK_FAMILY);
    cjk_face.features = features();
    app.text_system().resolve_font(&cjk_face);
    Ok(())
}

/// The host owns glyph origins and spans. Ligatures are disabled explicitly.
/// The CJK fallback is the pinned Japanese regional form, not a universal
/// Han-language choice. Other missing glyphs remain platform-dependent.
pub fn terminal_font() -> Font {
    let mut face = font(PRIMARY_FAMILY);
    face.features = features();
    face.fallbacks = Some(FontFallbacks::from_fonts(vec![CJK_FAMILY.into()]));
    face
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_profile_pins_primary_family_and_disables_liga_and_calt() {
        assert_eq!(PROFILE_ID, "pigade-terminal-render-profile-aliases-v1");
        assert_eq!(SIZE, 14.);
        let face = terminal_font();
        assert_eq!(face.family.as_ref(), PRIMARY_FAMILY);
        assert_eq!(
            face.features.tag_value_list(),
            [("liga".into(), 0), ("calt".into(), 0)]
        );
        assert_eq!(
            face.fallbacks.unwrap().fallback_list(),
            [CJK_FAMILY.to_owned()]
        );
    }
}
