//! Font loading, text measurement and glyph metrics.

use fontdue::{Font, FontSettings};

use super::css::ComputedStyle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Sans,
    Serif,
    Mono,
}

impl Family {
    fn for_style(style: &ComputedStyle) -> Self {
        for name in style.font_family.split(',') {
            let name = name
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_ascii_lowercase();
            if name.contains("mono") || name.contains("courier") || name.contains("consolas") {
                return Family::Mono;
            }
            if name == "sans-serif"
                || name.contains("arial")
                || name.contains("helvetica")
                || name.contains("roboto")
                || name.contains("dejavu sans")
            {
                return Family::Sans;
            }
            if name == "serif"
                || name.contains("times")
                || name.contains("georgia")
                || name.contains("garamond")
                || name.contains("dejavu serif")
            {
                return Family::Serif;
            }
        }
        Family::Sans
    }
}

pub struct FontSet {
    sans: FontPair,
    serif: FontPair,
    mono: FontPair,
}

struct FontPair {
    regular: Font,
    bold: Font,
}

impl FontPair {
    fn load(regular: &[u8], bold: &[u8]) -> Self {
        Self {
            regular: Font::from_bytes(regular, FontSettings::default())
                .expect("embedded font must be valid"),
            bold: Font::from_bytes(bold, FontSettings::default())
                .expect("embedded font must be valid"),
        }
    }

    fn font(&self, weight: u16) -> &Font {
        if weight >= 600 {
            &self.bold
        } else {
            &self.regular
        }
    }
}

impl Default for FontSet {
    fn default() -> Self {
        Self::embedded()
    }
}

impl FontSet {
    pub fn embedded() -> Self {
        Self {
            sans: FontPair::load(
                &include_bytes!("../../assets/fonts/DejaVuSans.ttf")[..],
                &include_bytes!("../../assets/fonts/DejaVuSans-Bold.ttf")[..],
            ),
            serif: FontPair::load(
                &include_bytes!("../../assets/fonts/DejaVuSerif.ttf")[..],
                &include_bytes!("../../assets/fonts/DejaVuSerif-Bold.ttf")[..],
            ),
            mono: FontPair::load(
                &include_bytes!("../../assets/fonts/DejaVuSansMono.ttf")[..],
                &include_bytes!("../../assets/fonts/DejaVuSansMono-Bold.ttf")[..],
            ),
        }
    }

    pub fn font_for(&self, style: &ComputedStyle) -> &Font {
        let pair = match Family::for_style(style) {
            Family::Sans => &self.sans,
            Family::Serif => &self.serif,
            Family::Mono => &self.mono,
        };
        pair.font(style.font_weight)
    }

    pub fn advance(&self, ch: char, style: &ComputedStyle) -> f32 {
        self.font_for(style)
            .metrics(ch, style.font_size)
            .advance_width
    }

    pub fn text_width(&self, text: &str, style: &ComputedStyle) -> f32 {
        let font = self.font_for(style);
        text.chars()
            .map(|ch| {
                if ch == '\t' {
                    font.metrics(' ', style.font_size).advance_width * 4.0
                } else {
                    font.metrics(ch, style.font_size).advance_width
                }
            })
            .sum()
    }

    pub fn ascent(&self, style: &ComputedStyle) -> f32 {
        self.font_for(style)
            .horizontal_line_metrics(style.font_size)
            .map(|metrics| metrics.ascent)
            .unwrap_or(style.font_size * 0.8)
    }

    pub fn descent(&self, style: &ComputedStyle) -> f32 {
        self.font_for(style)
            .horizontal_line_metrics(style.font_size)
            .map(|metrics| metrics.descent.abs())
            .unwrap_or(style.font_size * 0.2)
    }
}
