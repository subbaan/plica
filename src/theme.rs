//! Palettes and semantic colour roles.
//!
//! Layers are kept separate: `parse_kitty` (text -> Palette), `resolve`
//! (Palette -> semantic Theme with contrast fixes), and `ui.rs` (Theme ->
//! terminal colours). A future Base16 parser only needs to produce a Palette.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

impl Rgb {
    fn channel_lin(c: u8) -> f64 {
        let c = c as f64 / 255.0;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }

    /// WCAG relative luminance.
    pub fn luminance(self) -> f64 {
        0.2126 * Self::channel_lin(self.0)
            + 0.7152 * Self::channel_lin(self.1)
            + 0.0722 * Self::channel_lin(self.2)
    }

    /// Linear blend: t=0 -> self, t=1 -> other.
    pub fn mix(self, other: Rgb, t: f64) -> Rgb {
        let m = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round().clamp(0.0, 255.0) as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }
}

/// WCAG contrast ratio between two colours (1.0 ..= 21.0).
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (a.luminance(), b.luminance());
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Nudge `fg` toward white or black until it reaches `min` contrast on `bg`.
pub fn ensure_contrast(fg: Rgb, bg: Rgb, min: f64) -> Rgb {
    if contrast(fg, bg) >= min {
        return fg;
    }
    let white = Rgb(255, 255, 255);
    let black = Rgb(0, 0, 0);
    let target = if contrast(white, bg) >= contrast(black, bg) { white } else { black };
    for step in 1..=10 {
        let c = fg.mix(target, step as f64 / 10.0);
        if contrast(c, bg) >= min {
            return c;
        }
    }
    target
}

/// Raw colours as supplied by a palette source. Everything is optional except
/// foreground/background, which a palette must define to be accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Option<Rgb>,
    pub cursor_text: Option<Rgb>,
    pub selection_fg: Option<Rgb>,
    pub selection_bg: Option<Rgb>,
    pub url: Option<Rgb>,
    pub active_tab_fg: Option<Rgb>,
    pub active_tab_bg: Option<Rgb>,
    pub inactive_tab_fg: Option<Rgb>,
    pub inactive_tab_bg: Option<Rgb>,
    pub inactive_border: Option<Rgb>,
    pub ansi: [Option<Rgb>; 16],
}

#[derive(Debug)]
pub struct ParseReport {
    pub palette: Palette,
    pub warnings: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ThemeError {
    Missing(&'static str),
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ThemeError::Missing(k) => write!(f, "palette has no valid `{k}`"),
        }
    }
}

/// Parse a colour value as accepted by Kitty: `#rgb`, `#rrggbb`, `rgb:r/g/b`.
/// Returns Ok(None) for `none`.
pub fn parse_color(v: &str) -> Result<Option<Rgb>, String> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let hex = |s: &str| u8::from_str_radix(s, 16).map_err(|_| format!("invalid colour `{v}`"));
    if let Some(h) = v.strip_prefix('#') {
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("invalid colour `{v}`"));
        }
        return match h.len() {
            3 => {
                let d = |i: usize| hex(&h[i..i + 1].repeat(2));
                Ok(Some(Rgb(d(0)?, d(1)?, d(2)?)))
            }
            6 => Ok(Some(Rgb(hex(&h[0..2])?, hex(&h[2..4])?, hex(&h[4..6])?))),
            _ => Err(format!("invalid colour `{v}`")),
        };
    }
    if let Some(rest) = v.strip_prefix("rgb:") {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() != 3 {
            return Err(format!("invalid colour `{v}`"));
        }
        let mut out = [0u8; 3];
        for (i, p) in parts.iter().enumerate() {
            if p.is_empty() || p.len() > 4 || !p.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("invalid colour `{v}`"));
            }
            // Scale 1-4 hex digits to 8 bits.
            let n = u32::from_str_radix(p, 16).unwrap();
            let max = (1u32 << (4 * p.len())) - 1;
            out[i] = ((n * 255 + max / 2) / max) as u8;
        }
        return Ok(Some(Rgb(out[0], out[1], out[2])));
    }
    Err(format!("unsupported colour `{v}` (use #rrggbb)"))
}

/// Parse Kitty-style `key value` colour config. Unknown keys are ignored (the
/// same file may hold other Kitty settings); bad values for keys we use are
/// reported as warnings. Fails only if foreground/background are unusable.
pub fn parse_kitty(text: &str) -> Result<ParseReport, ThemeError> {
    let mut warnings = Vec::new();
    let mut fg = None;
    let mut bg = None;
    let mut p = Palette {
        foreground: Rgb(0, 0, 0),
        background: Rgb(0, 0, 0),
        cursor: None,
        cursor_text: None,
        selection_fg: None,
        selection_bg: None,
        url: None,
        active_tab_fg: None,
        active_tab_bg: None,
        inactive_tab_fg: None,
        inactive_tab_bg: None,
        inactive_border: None,
        ansi: [None; 16],
    };
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.splitn(2, char::is_whitespace);
        let key = it.next().unwrap_or("");
        let val = it.next().unwrap_or("").trim();
        let slot: Option<&mut Option<Rgb>> = match key {
            "foreground" => Some(&mut fg),
            "background" => Some(&mut bg),
            "cursor" => Some(&mut p.cursor),
            "cursor_text_color" => Some(&mut p.cursor_text),
            "selection_foreground" => Some(&mut p.selection_fg),
            "selection_background" => Some(&mut p.selection_bg),
            "url_color" => Some(&mut p.url),
            "active_tab_foreground" => Some(&mut p.active_tab_fg),
            "active_tab_background" => Some(&mut p.active_tab_bg),
            "inactive_tab_foreground" => Some(&mut p.inactive_tab_fg),
            "inactive_tab_background" => Some(&mut p.inactive_tab_bg),
            "inactive_border_color" => Some(&mut p.inactive_border),
            k if k.starts_with("color") => match k[5..].parse::<usize>() {
                Ok(i) if i < 16 => Some(&mut p.ansi[i]),
                _ => None, // color16..255 are valid Kitty keys we don't use
            },
            "include" | "globinclude" | "envinclude" => {
                warnings.push(format!("line {}: `{key}` is not followed", n + 1));
                None
            }
            _ => None,
        };
        if let Some(slot) = slot {
            match parse_color(val) {
                Ok(c) => *slot = c,
                Err(e) => warnings.push(format!("line {}: {e}", n + 1)),
            }
        }
    }
    p.foreground = fg.ok_or(ThemeError::Missing("foreground"))?;
    p.background = bg.ok_or(ThemeError::Missing("background"))?;
    Ok(ParseReport { palette: p, warnings })
}

/// Built-in Dracula palette (https://draculatheme.com, MIT).
pub fn dracula() -> Palette {
    let c = |s: &str| parse_color(s).unwrap();
    let ansi = [
        "#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2",
        "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df", "#a4ffff", "#ffffff",
    ];
    Palette {
        foreground: c("#f8f8f2").unwrap(),
        background: c("#282a36").unwrap(),
        cursor: c("#f8f8f2"),
        cursor_text: c("#282a36"),
        selection_fg: None,
        selection_bg: c("#44475a"),
        url: c("#8be9fd"),
        active_tab_fg: c("#282a36"),
        active_tab_bg: c("#bd93f9"),
        inactive_tab_fg: c("#f8f8f2"),
        inactive_tab_bg: c("#21222c"),
        inactive_border: c("#6272a4"),
        ansi: ansi.map(c),
    }
}

/// Semantic roles used by the UI. Everything that draws asks for a role, never
/// a raw palette slot, so a palette change re-colours every region.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub editor_fg: Rgb,
    pub editor_bg: Rgb,
    pub muted: Rgb,
    pub accent: Rgb,
    pub accent_fg: Rgb,
    pub selection_fg: Rgb,
    pub selection_bg: Rgb,
    pub search_fg: Rgb,
    pub search_bg: Rgb,
    pub surface_fg: Rgb,
    pub surface_bg: Rgb,
    pub gutter_fg: Rgb,
    pub current_line_bg: Rgb,
    pub warning: Rgb,
    pub substitute: Rgb,
}

pub fn resolve(p: &Palette) -> Theme {
    let fg = p.foreground;
    let bg = p.background;
    let a = |i: usize| p.ansi[i];

    let muted = a(8)
        .filter(|&c| contrast(c, bg) >= 2.5)
        .unwrap_or_else(|| fg.mix(bg, 0.45));
    let muted = ensure_contrast(muted, bg, 2.5);

    let accent = p.active_tab_bg.or(p.selection_bg).or(a(4)).unwrap_or(fg);
    let accent_fg = ensure_contrast(p.active_tab_fg.unwrap_or(bg), accent, 4.5);

    let selection_bg = p.selection_bg.unwrap_or_else(|| fg.mix(bg, 0.7));
    let selection_fg = ensure_contrast(p.selection_fg.unwrap_or(fg), selection_bg, 4.5);

    let search_bg = a(3).or(a(11)).unwrap_or(accent);
    let search_fg = ensure_contrast(bg, search_bg, 4.5);

    // Status/menu bars: a slightly lifted surface, distinct from the editor.
    let surface_bg = match a(0) {
        Some(c) if contrast(c, bg) >= 1.08 => c,
        _ => bg.mix(fg, 0.10),
    };
    let surface_fg = ensure_contrast(p.inactive_tab_fg.unwrap_or(fg), surface_bg, 4.5);

    Theme {
        editor_fg: ensure_contrast(fg, bg, 4.5),
        editor_bg: bg,
        muted,
        accent,
        accent_fg,
        selection_fg,
        selection_bg,
        search_fg,
        search_bg,
        surface_fg,
        surface_bg,
        gutter_fg: muted,
        current_line_bg: bg.mix(fg, 0.05),
        warning: ensure_contrast(a(1).or(a(9)).unwrap_or(fg), surface_bg, 3.0),
        substitute: ensure_contrast(a(5).or(a(13)).unwrap_or(muted), bg, 3.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEMEWALL_SAMPLE: &str = "\
# ThemeWall terminal palette: sample
# Generated by ThemeWall; this file can be loaded by Kitty.

foreground                #e0e1e1
background                #131617
cursor                    #e0e1e1
cursor_text_color         #131617
selection_foreground      #0d0f0f
selection_background      #189abf
url_color                 #4db4db

active_tab_foreground     #0d0f0f
active_tab_background     #189abf
inactive_border_color     #505253
bell_border_color         #de8d4b

color0                    #232627
color1                    #de8d4b
color8                    #505253
color15                   #e0e1e1
";

    #[test]
    fn parses_themewall_sample() {
        let r = parse_kitty(THEMEWALL_SAMPLE).unwrap();
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(r.palette.foreground, Rgb(0xe0, 0xe1, 0xe1));
        assert_eq!(r.palette.selection_bg, Some(Rgb(0x18, 0x9a, 0xbf)));
        assert_eq!(r.palette.ansi[1], Some(Rgb(0xde, 0x8d, 0x4b)));
        assert_eq!(r.palette.ansi[2], None);
    }

    #[test]
    fn colour_formats() {
        assert_eq!(parse_color("#fff"), Ok(Some(Rgb(255, 255, 255))));
        assert_eq!(parse_color("#0A0b0C"), Ok(Some(Rgb(10, 11, 12))));
        assert_eq!(parse_color("rgb:ff/80/0"), Ok(Some(Rgb(255, 128, 0))));
        assert_eq!(parse_color("rgb:ffff/0000/8080"), Ok(Some(Rgb(255, 0, 128))));
        assert_eq!(parse_color("none"), Ok(None));
        assert!(parse_color("#12345").is_err());
        assert!(parse_color("#gggggg").is_err());
        assert!(parse_color("red").is_err());
    }

    #[test]
    fn invalid_or_truncated_files_are_rejected_not_half_applied() {
        assert_eq!(parse_kitty("").unwrap_err(), ThemeError::Missing("foreground"));
        assert!(parse_kitty("foreground #ffffff\n").is_err());
        assert!(parse_kitty("foreground #ffffff\nbackground #zzz\n").is_err());
        // A bad optional value is a warning, not a failure.
        let r = parse_kitty("foreground #fff\nbackground #000\ncolor3 bogus\ncolor200 #123456\n").unwrap();
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn resolved_roles_meet_contrast() {
        for p in [dracula(), parse_kitty(THEMEWALL_SAMPLE).unwrap().palette] {
            let t = resolve(&p);
            assert!(contrast(t.editor_fg, t.editor_bg) >= 4.5);
            assert!(contrast(t.selection_fg, t.selection_bg) >= 4.5);
            assert!(contrast(t.search_fg, t.search_bg) >= 4.5);
            assert!(contrast(t.surface_fg, t.surface_bg) >= 4.5);
            assert!(contrast(t.accent_fg, t.accent) >= 4.5);
            assert!(contrast(t.muted, t.editor_bg) >= 2.5);
        }
    }

    #[test]
    fn low_contrast_palette_is_repaired() {
        let p = parse_kitty("foreground #333333\nbackground #2a2a2a\nselection_background #2b2b2b\n").unwrap();
        let t = resolve(&p.palette);
        assert!(contrast(t.editor_fg, t.editor_bg) >= 4.5);
        assert!(contrast(t.selection_fg, t.selection_bg) >= 4.5);
    }
}
