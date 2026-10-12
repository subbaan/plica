//! User-defined word and phrase highlighting.
//!
//! Groups of terms are matched against the visible lines only (never the
//! whole document) and drawn as a style overlay; file text is never touched.
//! Colours are fixed RGB, a palette slot (`color1`) or a semantic role
//! (`accent`), resolved against the current `Theme` at draw time, so linked
//! colours follow live palette changes. Group order is priority: when two
//! groups overlap, the earlier one wins.

use crate::find::{SearchOpts, find_all_bytes};
use crate::theme::{self, Rgb, Theme, ensure_contrast};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Foreground,
    Background,
    Accent,
    Selection,
    Search,
    Muted,
    Warning,
}

const ROLES: [(&str, Role); 7] = [
    ("foreground", Role::Foreground),
    ("background", Role::Background),
    ("accent", Role::Accent),
    ("selection", Role::Selection),
    ("search", Role::Search),
    ("muted", Role::Muted),
    ("warning", Role::Warning),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    Fixed(Rgb),
    /// ANSI palette slot 0..=15.
    Slot(u8),
    Role(Role),
}

impl Colour {
    pub fn parse(s: &str) -> Result<Colour, String> {
        let s = s.trim();
        let lower = s.to_ascii_lowercase();
        if let Some(n) = lower.strip_prefix("color") {
            return match n.parse::<u8>() {
                Ok(i) if i < 16 => Ok(Colour::Slot(i)),
                _ => Err(format!("`{s}`: palette slots are color0 to color15")),
            };
        }
        if let Some(&(_, r)) = ROLES.iter().find(|(n, _)| *n == lower) {
            return Ok(Colour::Role(r));
        }
        match theme::parse_color(s) {
            Ok(Some(c)) => Ok(Colour::Fixed(c)),
            _ => Err(format!(
                "`{s}` is not a colour (use #rrggbb, color0..color15, or one of: {})",
                ROLES.iter().map(|r| r.0).collect::<Vec<_>>().join(", ")
            )),
        }
    }

    /// The config spelling.
    pub fn name(self) -> String {
        match self {
            Colour::Fixed(c) => c.to_string(),
            Colour::Slot(i) => format!("color{i}"),
            Colour::Role(r) => ROLES.iter().find(|x| x.1 == r).map_or("accent", |x| x.0).into(),
        }
    }

    pub fn resolve(self, t: &Theme) -> Rgb {
        match self {
            Colour::Fixed(c) => c,
            Colour::Slot(i) => t.ansi[i as usize],
            Colour::Role(r) => match r {
                Role::Foreground => t.editor_fg,
                Role::Background => t.editor_bg,
                Role::Accent => t.accent,
                Role::Selection => t.selection_bg,
                Role::Search => t.search_bg,
                Role::Muted => t.muted,
                Role::Warning => t.warning,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub name: String,
    pub terms: Vec<String>,
    pub fg: Option<Colour>,
    pub bg: Option<Colour>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// File extensions (lowercase, no dot); empty = every file.
    pub file_types: Vec<String>,
    pub enabled: bool,
}

impl Group {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            terms: Vec::new(),
            fg: None,
            bg: None,
            bold: false,
            italic: false,
            underline: false,
            case_sensitive: false,
            whole_word: true,
            file_types: Vec::new(),
            enabled: true,
        }
    }

    fn applies_to(&self, path: Option<&Path>) -> bool {
        if self.file_types.is_empty() {
            return true;
        }
        let ext = path.and_then(Path::extension).map(|e| e.to_string_lossy().to_ascii_lowercase());
        ext.is_some_and(|e| self.file_types.contains(&e))
    }
}

/// The built-in demonstration groups (`highlights.samples = false` hides
/// them; a user group with the same name replaces one).
pub fn samples() -> Vec<Group> {
    let g = |name: &str, term: &str, fg: Colour, bg: Option<Colour>| Group {
        terms: vec![term.into()],
        fg: Some(fg),
        bg,
        bold: true,
        case_sensitive: true,
        ..Group::new(name)
    };
    vec![
        g("Important", "IMPORTANT", Colour::Role(Role::Background), Some(Colour::Slot(1))),
        g("Fixme", "FIXME", Colour::Slot(1), None),
        g("Todo", "TODO", Colour::Slot(3), None),
        g("Idea", "IDEA", Colour::Slot(6), None),
        g("Done", "DONE", Colour::Slot(2), None),
    ]
}

#[derive(Debug, Clone, PartialEq)]
pub struct Highlights {
    pub enabled: bool,
    pub samples: bool,
    /// User groups from the config, in priority order.
    pub groups: Vec<Group>,
}

impl Default for Highlights {
    fn default() -> Self {
        Self { enabled: true, samples: true, groups: Vec::new() }
    }
}

impl Highlights {
    /// Groups that apply to a file, in priority order: user groups, then the
    /// samples they don't replace.
    pub fn active(&self, path: Option<&Path>) -> Vec<Group> {
        if !self.enabled {
            return Vec::new();
        }
        let mut out: Vec<Group> = self.groups.clone();
        if self.samples {
            for s in samples() {
                if !self.groups.iter().any(|g| g.name.eq_ignore_ascii_case(&s.name)) {
                    out.push(s);
                }
            }
        }
        out.retain(|g| g.enabled && !g.terms.is_empty() && g.applies_to(path));
        out
    }
}

/// A group's colours for the current theme, with contrast repaired so a
/// faint accent stays readable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolved {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

pub fn resolve(g: &Group, t: &Theme) -> Resolved {
    let bg = g.bg.map(|c| c.resolve(t));
    let fg = match bg {
        Some(b) => Some(ensure_contrast(g.fg.map_or(t.editor_fg, |c| c.resolve(t)), b, 4.5)),
        None => g.fg.map(|c| ensure_contrast(c.resolve(t), t.editor_bg, 3.0)),
    };
    Resolved { fg, bg, bold: g.bold, italic: g.italic, underline: g.underline }
}

/// Matches in `text` as (byte start, byte end, group index). Terms are
/// literal; whole-word and case options come from each group.
pub fn match_text(groups: &[Group], text: &str) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        let opts = SearchOpts { match_case: g.case_sensitive, whole_word: g.whole_word };
        for term in &g.terms {
            for (s, e) in find_all_bytes(text, term, opts) {
                out.push((s, e, gi));
            }
        }
    }
    out
}

/// Group index per char of a line window (`u16::MAX` = none). Lower group
/// indices win where matches overlap.
pub fn paint(groups: &[Group], text: &str) -> Vec<u16> {
    let mut byte_to_char = vec![0u32; text.len() + 1];
    let mut n = 0u32;
    for (b, _) in text.char_indices() {
        byte_to_char[b] = n;
        n += 1;
    }
    byte_to_char[text.len()] = n;
    let mut marks = vec![u16::MAX; n as usize];
    let mut m = match_text(groups, text);
    // Paint low priority first so higher priority overwrites it.
    m.sort_by_key(|&(_, _, gi)| std::cmp::Reverse(gi));
    for (s, e, gi) in m {
        for mark in &mut marks[byte_to_char[s] as usize..byte_to_char[e] as usize] {
            *mark = gi as u16;
        }
    }
    marks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(name: &str, terms: &[&str]) -> Group {
        Group { terms: terms.iter().map(|s| s.to_string()).collect(), ..Group::new(name) }
    }

    #[test]
    fn colour_specs() {
        assert_eq!(Colour::parse("#ff8800"), Ok(Colour::Fixed(Rgb(255, 136, 0))));
        assert_eq!(Colour::parse("Color12"), Ok(Colour::Slot(12)));
        assert_eq!(Colour::parse("accent"), Ok(Colour::Role(Role::Accent)));
        assert!(Colour::parse("color16").is_err());
        assert!(Colour::parse("tomato").is_err());
        for c in [Colour::Fixed(Rgb(1, 2, 3)), Colour::Slot(4), Colour::Role(Role::Warning)] {
            assert_eq!(Colour::parse(&c.name()), Ok(c));
        }
    }

    #[test]
    fn whole_word_case_and_phrases() {
        let mut imp = group("imp", &["important"]);
        let phrase = Group { whole_word: false, ..group("p", &["must not"]) };
        let text = "IMPORTANT: unimportant; you Must Not";
        let m = match_text(&[imp.clone(), phrase.clone()], text);
        assert_eq!(m.iter().filter(|x| x.2 == 0).count(), 1, "whole word skips 'unimportant'");
        assert!(m.iter().any(|&(s, e, g)| g == 1 && &text[s..e] == "Must Not"));
        imp.case_sensitive = true;
        assert!(match_text(&[imp], text).is_empty(), "case-sensitive 'important' vs IMPORTANT");
    }

    #[test]
    fn earlier_group_wins_overlaps() {
        let a = group("a", &["red fox"]);
        let b = Group { whole_word: false, ..group("b", &["fox jumps"]) };
        let marks = paint(&[a, b], "red fox jumps");
        // r e d _ f o x _ j u m p s
        assert_eq!(&marks[0..7], &[0; 7]);
        assert_eq!(marks[7], 1);
        assert_eq!(&marks[8..], &[1; 5]);
    }

    #[test]
    fn marks_are_per_char_not_byte() {
        let marks = paint(&[group("x", &["ü"])], "aü b ü");
        assert_eq!(marks.len(), 6);
        assert_eq!(marks[1], u16::MAX, "aü is one word");
        assert_eq!(marks[5], 0);
    }

    #[test]
    fn scopes_and_samples() {
        let mut h = Highlights::default();
        let md = Group { file_types: vec!["md".into()], ..group("Todo", &["todo"]) };
        h.groups.push(md);
        let txt = h.active(Some(Path::new("/a/b.txt")));
        assert!(!txt.iter().any(|g| g.name == "Todo"), "user Todo replaces the sample, and is md-only");
        assert!(txt.iter().any(|g| g.name == "Fixme"));
        assert!(h.active(Some(Path::new("/a/B.MD"))).iter().any(|g| g.name == "Todo" && g.terms == ["todo"]));
        h.samples = false;
        assert!(h.active(None).is_empty());
        h.enabled = false;
        assert!(h.active(Some(Path::new("x.md"))).is_empty());
    }

    #[test]
    fn linked_colours_follow_the_theme_and_stay_readable() {
        let g = Group { fg: Some(Colour::Slot(1)), ..group("g", &["x"]) };
        let d = theme::resolve(&theme::dracula());
        let tw = theme::resolve(&theme::parse_kitty("foreground #e0e1e1\nbackground #131617\ncolor1 #de8d4b\n").unwrap().palette);
        assert_eq!(resolve(&g, &d).fg, Some(Rgb(0xff, 0x55, 0x55)));
        assert_eq!(resolve(&g, &tw).fg, Some(Rgb(0xde, 0x8d, 0x4b)));
        // A near-invisible fixed colour is lifted.
        let faint = Group { fg: Some(Colour::Fixed(Rgb(0x2a, 0x2c, 0x38))), ..group("f", &["x"]) };
        let r = resolve(&faint, &d).fg.unwrap();
        assert!(theme::contrast(r, d.editor_bg) >= 3.0);
        // With a background, the text colour is checked against it.
        let boxed = Group { fg: Some(Colour::Role(Role::Background)), bg: Some(Colour::Slot(1)), ..group("b", &["x"]) };
        let r = resolve(&boxed, &tw);
        assert!(theme::contrast(r.fg.unwrap(), r.bg.unwrap()) >= 4.5);
    }
}
