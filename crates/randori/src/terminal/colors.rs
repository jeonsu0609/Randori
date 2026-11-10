//! Terminal colours: the 16 ANSI colours (one set for dark themes, one for
//! light), the xterm 256-colour cube and grey ramp, and the theme's foreground
//! and background for the default colours. A program may replace any of them
//! (OSC 4/10/11); the terminal's own table wins when it has an entry.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui_kit::{Hsla, Rgba};

/// The ANSI colours on a dark background (normal, then bright).
const DARK: [u32; 16] = [
    0x1e1e1e, 0xf14c4c, 0x23d18b, 0xe5e510, 0x3b8eea, 0xd670d6, 0x29b8db, 0xcccccc, //
    0x666666, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xe5e5e5,
];

/// The ANSI colours on a light background, dark enough to read on white.
const LIGHT: [u32; 16] = [
    0x000000, 0xcd3131, 0x00bc00, 0x949800, 0x0451a5, 0xbc05bc, 0x0598bc, 0x555555, //
    0x666666, 0xcd3131, 0x14ce14, 0xb5ba00, 0x0451a5, 0xbc05bc, 0x0598bc, 0xa5a5a5,
];

/// Resolves cell colours against the terminal's table and the theme.
pub struct Palette<'a> {
    pub table: &'a Colors,
    pub dark: bool,
    pub foreground: Hsla,
    pub background: Hsla,
}

impl Palette<'_> {
    pub fn resolve(&self, color: Color) -> Hsla {
        match color {
            Color::Spec(rgb) => hsla(rgb),
            Color::Indexed(ix) => self.indexed(ix as usize),
            Color::Named(named) => self.named(named),
        }
    }

    /// The colour of a named slot, dimmed or brightened variants included.
    pub fn named(&self, named: NamedColor) -> Hsla {
        if let Some(rgb) = self.table[named] {
            return hsla(rgb);
        }
        match named {
            NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::Cursor => {
                self.foreground
            }
            NamedColor::Background => self.background,
            NamedColor::DimForeground => self.foreground.opacity(0.7),
            named if (named as usize) < 16 => self.indexed(named as usize),
            dim => {
                // DimBlack..=DimWhite follow Cursor in the enum
                let base = (dim as usize).saturating_sub(NamedColor::DimBlack as usize);
                self.indexed(base.min(7)).opacity(0.7)
            }
        }
    }

    pub fn indexed(&self, ix: usize) -> Hsla {
        if let Some(rgb) = self.table[ix] {
            return hsla(rgb);
        }
        hsla(default_indexed(ix, self.dark))
    }
}

/// xterm's colour for index `ix` (0–255).
pub fn default_indexed(ix: usize, dark: bool) -> Rgb {
    match ix {
        0..16 => rgb(if dark { DARK[ix] } else { LIGHT[ix] }),
        16..232 => {
            let ix = ix - 16;
            let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
            Rgb {
                r: level(ix / 36),
                g: level(ix / 6 % 6),
                b: level(ix % 6),
            }
        }
        _ => {
            let v = (8 + (ix.min(255) - 232) * 10) as u8;
            Rgb { r: v, g: v, b: v }
        }
    }
}

fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

pub fn hsla(rgb: Rgb) -> Hsla {
    Rgba {
        r: rgb.r as f32 / 255.,
        g: rgb.g as f32 / 255.,
        b: rgb.b as f32 / 255.,
        a: 1.,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cube_and_ramp_match_xterm() {
        let c = default_indexed(16, true);
        assert_eq!((c.r, c.g, c.b), (0, 0, 0));
        let c = default_indexed(196, true);
        assert_eq!((c.r, c.g, c.b), (255, 0, 0), "pure red");
        let c = default_indexed(231, true);
        assert_eq!((c.r, c.g, c.b), (255, 255, 255));
        let c = default_indexed(232, true);
        assert_eq!((c.r, c.g, c.b), (8, 8, 8));
        let c = default_indexed(255, true);
        assert_eq!((c.r, c.g, c.b), (238, 238, 238));
    }

    #[test]
    fn programs_override_the_table() {
        let mut table = Colors::default();
        table[1] = Some(Rgb { r: 1, g: 2, b: 3 });
        let palette = Palette {
            table: &table,
            dark: true,
            foreground: gpui_kit::white(),
            background: gpui_kit::black(),
        };
        assert_eq!(
            palette.resolve(Color::Named(NamedColor::Red)),
            hsla(Rgb { r: 1, g: 2, b: 3 })
        );
        assert_eq!(
            palette.resolve(Color::Named(NamedColor::Foreground)),
            gpui_kit::white()
        );
    }
}
