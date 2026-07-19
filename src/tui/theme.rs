//! Palette tokens (DESIGN 1.1, 1.2, 1.4). Render code reads the active
//! `Palette`'s fields; never inline hex in component code (DESIGN 9.7).
//! All four themes are dark: a theme is a re-hue of the same dark system,
//! never a light/dark toggle (DESIGN 0, 1.4).

use ratatui::style::Color;

/// One field per DESIGN 1.2 semantic alias, plus `elevated` (toast layer).
#[derive(Debug, PartialEq, Eq)]
pub struct Palette {
    pub name: &'static str,
    pub bg: Color,
    pub surface: Color,
    pub elevated: Color,
    pub chrome: Color,
    pub fg: Color,
    pub fg2: Color,
    pub fg3: Color,
    pub focus: Color,
    pub hot: Color,
    pub warn: Color,
}

/// DESIGN 1.1 verbatim: the reference theme every mock is authored against.
pub static TERMINAL_GHOST: Palette = Palette {
    name: "terminal_ghost",
    bg: Color::Rgb(0x02, 0x0d, 0x06),
    surface: Color::Rgb(0x06, 0x14, 0x10),
    elevated: Color::Rgb(0x0b, 0x1f, 0x18),
    chrome: Color::Rgb(0x1a, 0x40, 0x30),
    fg: Color::Rgb(0x39, 0xff, 0x6a),
    fg2: Color::Rgb(0x2a, 0x60, 0x40),
    fg3: Color::Rgb(0x16, 0x35, 0x25),
    // Overdriven above fg's luminance so the focused row clears its
    // neighbours (DESIGN 1.1).
    focus: Color::Rgb(0x20, 0xff, 0xdd),
    hot: Color::Rgb(0xff, 0x2d, 0x78),
    warn: Color::Rgb(0xe5, 0xb8, 0x00),
};

/// Pure monochrome phosphor: `focus` == `fg` by design, bold alone carries
/// focus distinction; `hot` is the one complementary accent (DESIGN 1.4).
pub static PHOSPHOR: Palette = Palette {
    name: "phosphor",
    bg: Color::Rgb(0x02, 0x0a, 0x05),
    surface: Color::Rgb(0x04, 0x14, 0x0b),
    elevated: Color::Rgb(0x08, 0x20, 0x17),
    chrome: Color::Rgb(0x14, 0x40, 0x2a),
    fg: Color::Rgb(0x33, 0xff, 0x66),
    fg2: Color::Rgb(0x1e, 0x9e, 0x46),
    fg3: Color::Rgb(0x0f, 0x5a, 0x2a),
    focus: Color::Rgb(0x33, 0xff, 0x66),
    hot: Color::Rgb(0xff, 0x4d, 0x2d),
    warn: Color::Rgb(0xd9, 0x8e, 0x04),
};

/// Nord mapping (DESIGN 1.4): focus is a hue shift, deliberately NOT a
/// luminance lift over fg; a ratified trade, do not "fix" it (DESIGN 10).
pub static NORD: Palette = Palette {
    name: "nord",
    bg: Color::Rgb(0x2e, 0x34, 0x40),
    surface: Color::Rgb(0x3b, 0x42, 0x52),
    elevated: Color::Rgb(0x43, 0x4c, 0x5e),
    chrome: Color::Rgb(0x4c, 0x56, 0x6a),
    fg: Color::Rgb(0xd8, 0xde, 0xe9),
    fg2: Color::Rgb(0xa0, 0xa8, 0xb7),
    fg3: Color::Rgb(0x61, 0x6e, 0x88),
    focus: Color::Rgb(0x88, 0xc0, 0xd0),
    hot: Color::Rgb(0xd0, 0x87, 0x70),
    warn: Color::Rgb(0xeb, 0xcb, 0x8b),
};

/// TokyoNight night base, storm surface tier; focus is the deliberate
/// luminance lift off canonical TN cyan (DESIGN 1.4 rationale).
pub static TOKYONIGHT: Palette = Palette {
    name: "tokyonight",
    bg: Color::Rgb(0x1a, 0x1b, 0x26),
    surface: Color::Rgb(0x24, 0x28, 0x3b),
    elevated: Color::Rgb(0x29, 0x2e, 0x42),
    chrome: Color::Rgb(0x3b, 0x42, 0x61),
    fg: Color::Rgb(0xc0, 0xca, 0xf5),
    fg2: Color::Rgb(0x9a, 0xa5, 0xce),
    fg3: Color::Rgb(0x56, 0x5f, 0x89),
    focus: Color::Rgb(0xb0, 0xe8, 0xff),
    hot: Color::Rgb(0xf7, 0x76, 0x8e),
    warn: Color::Rgb(0xe0, 0xaf, 0x68),
};

/// Config `palette` key to theme; Terminal Ghost is the default and the
/// fallback for any unrecognized value (DESIGN 1.4).
pub fn by_name(name: &str) -> &'static Palette {
    match name {
        "phosphor" => &PHOSPHOR,
        "nord" => &NORD,
        "tokyonight" => &TOKYONIGHT,
        _ => &TERMINAL_GHOST,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_name_resolves_all_four_and_falls_back() {
        for name in ["terminal_ghost", "phosphor", "nord", "tokyonight"] {
            assert_eq!(by_name(name).name, name);
        }
        assert_eq!(by_name("solarized").name, "terminal_ghost");
        assert_eq!(by_name("").name, "terminal_ghost");
    }

    #[test]
    fn phosphor_focus_matches_fg_by_design() {
        assert_eq!(PHOSPHOR.focus, PHOSPHOR.fg);
    }
}
