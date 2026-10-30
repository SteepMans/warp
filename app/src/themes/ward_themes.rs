//! Built-in themes that ship with Ward (the fork's OSS channel).
//!
//! Each theme is described by a [`WardPalette`]; the same data drives the theme itself and the
//! contrast tests, so a palette edit is always re-checked.

use pathfinder_color::ColorU;
use warp_core::ui::theme::{
    AnsiColor, AnsiColors, Details, Fill, HorizontalGradient, TerminalColors, VerticalGradient,
    WarpTheme,
};

use super::theme::ThemeKind;

/// Colors are `0xRRGGBBAA`. `background` and `accent` hold one color for a solid fill or two for
/// a gradient (background: top to bottom, accent: left to right). ANSI arrays are in the order
/// black, red, green, yellow, blue, magenta, cyan, white.
pub(super) struct WardPalette {
    pub name: &'static str,
    pub light: bool,
    pub background: &'static [u32],
    pub foreground: u32,
    pub accent: &'static [u32],
    pub cursor: u32,
    pub normal: [u32; 8],
    pub bright: [u32; 8],
}

const WARD_EMBER: WardPalette = WardPalette {
    name: "Ward Ember",
    light: false,
    background: &[0x1A1412FF],
    foreground: 0xF2E3D5FF,
    accent: &[0xFF7A3DFF],
    cursor: 0xFFB547FF,
    normal: [
        0x2A201CFF, 0xF2564BFF, 0x9FCB6BFF, 0xF5B940FF, 0x6FA8D6FF, 0xD982B5FF, 0x6CC4B4FF,
        0xD9C9BAFF,
    ],
    bright: [
        0x7A6A60FF, 0xFF7F70FF, 0xBEE38CFF, 0xFFD36EFF, 0x96C3EAFF, 0xEDA5CFFF, 0x92DDCFFF,
        0xFFF4E8FF,
    ],
};

const WARD_ABYSS: WardPalette = WardPalette {
    name: "Ward Abyss",
    light: false,
    background: &[0x0B1622FF],
    foreground: 0xD6E6F2FF,
    accent: &[0x3FB8D9FF],
    cursor: 0x5FD4F0FF,
    normal: [
        0x13253AFF, 0xF0707AFF, 0x5FD3A0FF, 0xF2C866FF, 0x5AA6F2FF, 0xB98CF2FF, 0x4FD0D8FF,
        0xB9CADAFF,
    ],
    bright: [
        0x5E7A94FF, 0xFF98A0FF, 0x8AEBC0FF, 0xFFDD8FFF, 0x8CC2FFFF, 0xD3B2FFFF, 0x84E6ECFF,
        0xEEF6FCFF,
    ],
};

const WARD_AURORA: WardPalette = WardPalette {
    name: "Ward Aurora",
    light: false,
    background: &[0x10131CFF],
    foreground: 0xE4E8F0FF,
    accent: &[0x5BE3A6FF, 0xA88BFAFF],
    cursor: 0x7CF0BEFF,
    normal: [
        0x1C2130FF, 0xF26D85FF, 0x5BE3A6FF, 0xEBCB6EFF, 0x7AA2F7FF, 0xB993FAFF, 0x5ED1E6FF,
        0xC5CBD8FF,
    ],
    bright: [
        0x646C85FF, 0xFF94A8FF, 0x8FF2C6FF, 0xF7DE97FF, 0xA3BEFFFF, 0xD3B8FFFF, 0x8DE6F3FF,
        0xF4F6FAFF,
    ],
};

const WARD_SYNTHWAVE: WardPalette = WardPalette {
    name: "Ward Synthwave",
    light: false,
    background: &[0x1E1033FF, 0x2B1145FF],
    foreground: 0xF5E9FFFF,
    accent: &[0xFF4FB8FF],
    cursor: 0xFF4FB8FF,
    normal: [
        0x2E1F47FF, 0xFF5C7AFF, 0x4DF0B5FF, 0xFFD84DFF, 0x5EB6FFFF, 0xFF6BD6FF, 0x36E2F0FF,
        0xE0D0F5FF,
    ],
    bright: [
        0x8A78B0FF, 0xFF8AA0FF, 0x85FFD0FF, 0xFFE88AFF, 0x92D0FFFF, 0xFF9BE5FF, 0x7EF0F8FF,
        0xFFFFFFFF,
    ],
};

const WARD_MOSS: WardPalette = WardPalette {
    name: "Ward Moss",
    light: false,
    background: &[0x151B16FF],
    foreground: 0xDCE5D3FF,
    accent: &[0x8FBF5AFF],
    cursor: 0xB5D97EFF,
    normal: [
        0x222B23FF, 0xE5735FFF, 0x9BCB63FF, 0xE0C062FF, 0x79A9C9FF, 0xC291B8FF, 0x74C2A8FF,
        0xC7CFBEFF,
    ],
    bright: [
        0x66735FFF, 0xF59A88FF, 0xBCE38AFF, 0xF0D78EFF, 0xA1C6DEFF, 0xDAB3D1FF, 0x9DDCC6FF,
        0xEEF3E8FF,
    ],
};

const WARD_MONO: WardPalette = WardPalette {
    name: "Ward Mono",
    light: false,
    background: &[0x0A0A0AFF],
    foreground: 0xEDEDEDFF,
    accent: &[0xFFB000FF],
    cursor: 0xFFB000FF,
    normal: [
        0x1C1C1CFF, 0xFF5F56FF, 0x8FD18FFF, 0xFFB000FF, 0x8AB4F8FF, 0xD7A6E8FF, 0x8ED6D6FF,
        0xC8C8C8FF,
    ],
    bright: [
        0x6E6E6EFF, 0xFF8A84FF, 0xB4E6B4FF, 0xFFCB52FF, 0xB3CDFBFF, 0xE8C8F2FF, 0xB6EAEAFF,
        0xFFFFFFFF,
    ],
};

const WARD_PAPER: WardPalette = WardPalette {
    name: "Ward Paper",
    light: true,
    background: &[0xF7F3EAFF],
    foreground: 0x2B2A27FF,
    accent: &[0x2F5DA8FF],
    cursor: 0x2F5DA8FF,
    normal: [
        0x2B2A27FF, 0xB3261EFF, 0x3D7A2AFF, 0x8A6100FF, 0x2F5DA8FF, 0x8E3F8EFF, 0x1F7A80FF,
        0xD8D2C4FF,
    ],
    bright: [
        0x5F5A50FF, 0xD0402FFF, 0x4E9436FF, 0xA87A00FF, 0x4174C4FF, 0xA855A8FF, 0x2A929AFF,
        0xFFFDF8FF,
    ],
};

const WARD_LATTE: WardPalette = WardPalette {
    name: "Ward Latte",
    light: true,
    background: &[0xF2EADFFF],
    foreground: 0x3A2E26FF,
    accent: &[0xA8642AFF],
    cursor: 0xA8642AFF,
    normal: [
        0x3A2E26FF, 0xB8333AFF, 0x4F7D32FF, 0x8F6400FF, 0x3C68A6FF, 0x94478AFF, 0x2C7E78FF,
        0xDDD3C5FF,
    ],
    bright: [
        0x6E6055FF, 0xD04B4FFF, 0x5F9440FF, 0xA97A10FF, 0x4E7EC0FF, 0xAE5FA3FF, 0x2F8C85FF,
        0xFBF7F1FF,
    ],
};

pub(super) static WARD_PALETTES: [(ThemeKind, &WardPalette); 8] = [
    (ThemeKind::WardEmber, &WARD_EMBER),
    (ThemeKind::WardAbyss, &WARD_ABYSS),
    (ThemeKind::WardAurora, &WARD_AURORA),
    (ThemeKind::WardSynthwave, &WARD_SYNTHWAVE),
    (ThemeKind::WardMoss, &WARD_MOSS),
    (ThemeKind::WardMono, &WARD_MONO),
    (ThemeKind::WardPaper, &WARD_PAPER),
    (ThemeKind::WardLatte, &WARD_LATTE),
];

fn ansi_colors(colors: &[u32; 8]) -> AnsiColors {
    AnsiColors::new(
        AnsiColor::from_u32(colors[0]),
        AnsiColor::from_u32(colors[1]),
        AnsiColor::from_u32(colors[2]),
        AnsiColor::from_u32(colors[3]),
        AnsiColor::from_u32(colors[4]),
        AnsiColor::from_u32(colors[5]),
        AnsiColor::from_u32(colors[6]),
        AnsiColor::from_u32(colors[7]),
    )
}

impl WardPalette {
    pub(super) fn theme(&self) -> WarpTheme {
        let background = match self.background {
            [top, bottom] => Fill::VerticalGradient(VerticalGradient::new(
                ColorU::from_u32(*top),
                ColorU::from_u32(*bottom),
            )),
            colors => Fill::Solid(ColorU::from_u32(colors[0])),
        };
        let accent = match self.accent {
            [left, right] => Fill::HorizontalGradient(HorizontalGradient::new(
                ColorU::from_u32(*left),
                ColorU::from_u32(*right),
            )),
            colors => Fill::Solid(ColorU::from_u32(colors[0])),
        };
        WarpTheme::new(
            background,
            ColorU::from_u32(self.foreground),
            accent,
            Some(Fill::Solid(ColorU::from_u32(self.cursor))),
            Some(if self.light {
                Details::Lighter
            } else {
                Details::Darker
            }),
            TerminalColors::new(ansi_colors(&self.normal), ansi_colors(&self.bright)),
            None,
            Some(self.name.to_string()),
        )
    }
}

/// Ward themes for the built-in theme map.
pub(super) fn ward_themes() -> impl Iterator<Item = (ThemeKind, WarpTheme)> {
    WARD_PALETTES
        .iter()
        .map(|(kind, palette)| (kind.clone(), palette.theme()))
}

#[cfg(test)]
#[path = "ward_themes_tests.rs"]
mod tests;
