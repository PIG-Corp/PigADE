//! Shared visual tokens and embedded chrome assets.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString, Styled, Svg, px, rgb, svg};

pub mod palette {
    pub const BASE: u32 = 0x202022;
    pub const SIDEBAR: u32 = 0x252527;
    pub const RAISED: u32 = 0x2D2D31;
    pub const HOVER: u32 = 0x343438;
    pub const SELECTED: u32 = 0x393344;
    pub const BORDER: u32 = 0x3B3B40;
    pub const TEXT: u32 = 0xEEEEF1;
    pub const MUTED: u32 = 0xA6A6B0;
    pub const ACCENT: u32 = 0xB8A6EF;
    pub const STATUS_GREEN: u32 = 0x8DBBA0;
    pub const STATUS_AMBER: u32 = 0xDAB579;
}

pub use palette::*;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Icon {
    Folder,
    Plus,
    ChevronRight,
    ChevronDown,
    Terminal,
    Command,
    SplitRight,
    SplitDown,
    Focus,
    More,
    Branch,
    Layers,
    Close,
    Search,
    ArrowUpRight,
    LogoP,
}

impl Icon {
    pub const ALL: [Self; 16] = [
        Self::Folder,
        Self::Plus,
        Self::ChevronRight,
        Self::ChevronDown,
        Self::Terminal,
        Self::Command,
        Self::SplitRight,
        Self::SplitDown,
        Self::Focus,
        Self::More,
        Self::Branch,
        Self::Layers,
        Self::Close,
        Self::Search,
        Self::ArrowUpRight,
        Self::LogoP,
    ];

    pub const fn path(self) -> &'static str {
        match self {
            Self::Folder => "icons/folder.svg",
            Self::Plus => "icons/plus.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::Terminal => "icons/terminal.svg",
            Self::Command => "icons/command.svg",
            Self::SplitRight => "icons/split-right.svg",
            Self::SplitDown => "icons/split-down.svg",
            Self::Focus => "icons/focus.svg",
            Self::More => "icons/more.svg",
            Self::Branch => "icons/branch.svg",
            Self::Layers => "icons/layers.svg",
            Self::Close => "icons/close.svg",
            Self::Search => "icons/search.svg",
            Self::ArrowUpRight => "icons/arrow-up-right.svg",
            Self::LogoP => "icons/logo-p.svg",
        }
    }
}

/// An SVG icon tinted with the current text color and sized in logical pixels.
/// `name` is one of the kebab-case names in [`Icon`], without the `.svg` suffix.
pub fn icon(name: &'static str, size: f32, tint: u32) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .w(px(size))
        .h(px(size))
        .text_color(rgb(tint))
}

/// Embedded SVG assets for GPUI's asset-backed `Svg::path` renderer.
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let bytes: &'static [u8] = match path {
            "icons/folder.svg" => include_bytes!("../assets/icons/folder.svg"),
            "icons/plus.svg" => include_bytes!("../assets/icons/plus.svg"),
            "icons/chevron-right.svg" => include_bytes!("../assets/icons/chevron-right.svg"),
            "icons/chevron-down.svg" => include_bytes!("../assets/icons/chevron-down.svg"),
            "icons/terminal.svg" => include_bytes!("../assets/icons/terminal.svg"),
            "icons/command.svg" => include_bytes!("../assets/icons/command.svg"),
            "icons/split-right.svg" => include_bytes!("../assets/icons/split-right.svg"),
            "icons/split-down.svg" => include_bytes!("../assets/icons/split-down.svg"),
            "icons/focus.svg" => include_bytes!("../assets/icons/focus.svg"),
            "icons/more.svg" => include_bytes!("../assets/icons/more.svg"),
            "icons/branch.svg" => include_bytes!("../assets/icons/branch.svg"),
            "icons/layers.svg" => include_bytes!("../assets/icons/layers.svg"),
            "icons/close.svg" => include_bytes!("../assets/icons/close.svg"),
            "icons/search.svg" => include_bytes!("../assets/icons/search.svg"),
            "icons/arrow-up-right.svg" => include_bytes!("../assets/icons/arrow-up-right.svg"),
            "icons/logo-p.svg" => include_bytes!("../assets/icons/logo-p.svg"),
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(bytes)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        if path != "icons" {
            return Ok(Vec::new());
        }
        Ok(Icon::ALL
            .iter()
            .map(|icon| SharedString::from(icon.path()))
            .collect())
    }
}
