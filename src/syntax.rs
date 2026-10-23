//! Syntax colouring: Sublime Text grammars run by syntect (two-face's
//! extended set, the one `bat` ships). Tokens are sorted into a few kinds
//! (comment, string, keyword…) whose colours are palette slots and roles,
//! so they follow the live palette like the rest of the interface.
//!
//! Parsing is incremental and lazy. The parser state at the start of every
//! `CHECKPOINT`-th line is kept; an edit invalidates only from its first
//! line, and re-parsing stops as soon as it reaches a kept state that came
//! out the same. Only lines down to the bottom of the view are parsed, within
//! a time budget per frame; what doesn't fit continues while idle, and the
//! view meanwhile shows its best guess.

use crate::buffer::{Buffer, Since};
use crate::highlight::{Colour, Role};
use ropey::Rope;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Instant;
use syntect::highlighting::{
    Color, FontStyle, HighlightState, Highlighter, RangedHighlightIterator, ScopeSelectors, StyleModifier, Theme as SynTheme,
    ThemeItem, ThemeSettings,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// Keep the parser state at the start of every this many lines.
const CHECKPOINT: usize = 32;
/// Lines longer than this (bytes) are not parsed: drawn plain, and the
/// parser carries on after them as if they weren't there.
const LONG_LINE: usize = 8_192;
/// Documents larger than this (bytes) are not coloured.
pub const MAX_DOC: usize = 10 * 1024 * 1024;
/// A guess for a line past the exact parse starts from a kept state at
/// most this many lines above it, else from scratch this far above it.
const GUESS_REACH: usize = 64;
/// The exact parse follows the view only this far (bytes of text past
/// where it got to): seconds of work. A view further down, after a jump in
/// a big file, keeps its guesses, like Vim's syntax sync.
const EXACT_REACH: usize = 1 << 20;
/// At most this many lines' runs are kept around.
const MAX_RUNS: usize = 4_000;

/// What a token is, as far as colour goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Kind {
    Plain = 0,
    Comment,
    String,
    Constant,
    Keyword,
    Type,
    Function,
    Variable,
    Tag,
    Attribute,
    Heading,
    Link,
    Code,
    Quote,
    Inserted,
    Deleted,
    Changed,
}

impl Kind {
    /// Every kind with a colour (all but `Plain`), in config order.
    pub const ALL: [Kind; 16] = [
        Kind::Comment,
        Kind::String,
        Kind::Constant,
        Kind::Keyword,
        Kind::Type,
        Kind::Function,
        Kind::Variable,
        Kind::Tag,
        Kind::Attribute,
        Kind::Heading,
        Kind::Link,
        Kind::Code,
        Kind::Quote,
        Kind::Inserted,
        Kind::Deleted,
        Kind::Changed,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Plain => "plain",
            Kind::Comment => "comment",
            Kind::String => "string",
            Kind::Constant => "constant",
            Kind::Keyword => "keyword",
            Kind::Type => "type",
            Kind::Function => "function",
            Kind::Variable => "variable",
            Kind::Tag => "tag",
            Kind::Attribute => "attribute",
            Kind::Heading => "heading",
            Kind::Link => "link",
            Kind::Code => "code",
            Kind::Quote => "quote",
            Kind::Inserted => "inserted",
            Kind::Deleted => "deleted",
            Kind::Changed => "changed",
        }
    }

    pub fn from_name(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.name() == s)
    }

    fn from_index(i: u8) -> Kind {
        Kind::ALL.into_iter().find(|k| *k as u8 == i).unwrap_or(Kind::Plain)
    }

    /// The colour used unless `[syntax.colours]` says otherwise. ANSI slots
    /// are only colours: these follow the usual terminal-theme conventions.
    pub fn default_colour(self) -> Option<Colour> {
        Some(match self {
            Kind::Plain => return None,
            Kind::Comment | Kind::Quote => Colour::Role(Role::Muted),
            Kind::String | Kind::Code | Kind::Inserted => Colour::Slot(2),
            Kind::Constant | Kind::Attribute | Kind::Changed => Colour::Slot(3),
            Kind::Keyword => Colour::Slot(5),
            Kind::Type => Colour::Slot(6),
            Kind::Function | Kind::Link => Colour::Slot(4),
            Kind::Variable | Kind::Tag | Kind::Deleted => Colour::Slot(1),
            Kind::Heading => Colour::Role(Role::Accent),
        })
    }

    pub fn italic(self) -> bool {
        matches!(self, Kind::Comment | Kind::Quote)
    }

    pub fn bold(self) -> bool {
        self == Kind::Heading
    }

    /// The colour this kind is given (an override from `[syntax.colours]`
    /// if any, else the default), as specified.
    pub fn colour(self, overrides: &[(Kind, Colour)]) -> Option<Colour> {
        overrides.iter().rev().find(|(o, _)| *o == self).map(|(_, c)| *c).or(self.default_colour())
    }

    /// The colour as drawn: resolved against the theme and lifted to
    /// the kind's minimum contrast with the editor background.
    pub fn resolve(self, overrides: &[(Kind, Colour)], t: &crate::theme::Theme, high_contrast: bool) -> Option<crate::theme::Rgb> {
        let c = self.colour(overrides)?;
        Some(crate::theme::ensure_contrast(c.resolve(t), t.editor_bg, self.min_contrast(high_contrast)))
    }

    /// Comments are meant to recede: they need less contrast than code.
    pub fn min_contrast(self, high_contrast: bool) -> f64 {
        match (self.italic(), high_contrast) {
            (true, false) => 3.0,
            (true, true) | (false, false) => 4.5,
            (false, true) => 7.0,
        }
    }

    /// Scope selectors (TextMate syntax) for this kind. The most specific
    /// selector that matches a token wins, so e.g. `storage.type.function`
    /// (`fn`, `def`) beats `storage.type` (`i32`).
    fn selectors(self) -> &'static str {
        match self {
            // Fenced code in a known language gets that language's colours
            // on the plain text colour, not the inline-code colour.
            Kind::Plain => "keyword.operator, variable.parameter.option, punctuation.definition.parameter, markup.raw.code-fence source",
            Kind::Comment => "comment, punctuation.definition.comment",
            Kind::String => "string, punctuation.definition.string",
            Kind::Constant => "constant, support.constant",
            Kind::Keyword => {
                "keyword, storage, keyword.operator.word, keyword.operator.logical.python, keyword.operator.new, \
                 storage.type.function, storage.type.class, storage.type.struct, storage.type.enum, storage.type.trait, \
                 storage.type.impl, storage.type.module, storage.type.type, storage.type.union, storage.type.interface, \
                 storage.type.namespace, storage.type.function.arrow, punctuation.definition.list_item, markup.list.numbered.bullet"
            }
            Kind::Type => {
                "storage.type, support.type, support.class, entity.name.type, entity.name.class, entity.name.struct, \
                 entity.name.enum, entity.name.trait, entity.name.interface, entity.name.union, entity.name.impl, \
                 entity.other.inherited-class, entity.name.namespace"
            }
            Kind::Function => {
                "entity.name.function, support.function, variable.function, variable.annotation, meta.diff.range, \
                 meta.diff.header, meta.diff.index"
            }
            Kind::Variable => "variable.language, variable.parameter, variable.other.readwrite.shell, punctuation.definition.variable",
            Kind::Tag => "entity.name.tag",
            Kind::Attribute => "entity.other.attribute-name",
            Kind::Heading => "markup.heading, entity.name.section",
            Kind::Link => "markup.underline.link, meta.link.inline.description, meta.link.reference.description, string.other.link",
            Kind::Code => "markup.raw, punctuation.definition.raw.code-fence",
            Kind::Quote => "markup.quote",
            Kind::Inserted => "markup.inserted",
            Kind::Deleted => "markup.deleted",
            Kind::Changed => "markup.changed",
        }
    }
}

/// Bold / italic / underline asked for by the markup itself.
pub const FONT_BOLD: u8 = 1;
pub const FONT_UNDERLINE: u8 = 2;
pub const FONT_ITALIC: u8 = 4;

/// The grammars (loaded once; a few milliseconds, regexes compile lazily).
pub fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// A syntect theme whose "colours" are kind numbers, so syntect's own
/// selector scoring decides each token's kind.
fn highlighter() -> &'static Highlighter<'static> {
    static THEME: OnceLock<SynTheme> = OnceLock::new();
    static HL: OnceLock<Highlighter<'static>> = OnceLock::new();
    HL.get_or_init(|| {
        let theme = THEME.get_or_init(|| {
            let kind = |k: Kind| Color { r: k as u8, g: 0, b: 0, a: 255 };
            let mut scopes: Vec<ThemeItem> = std::iter::once(Kind::Plain)
                .chain(Kind::ALL)
                .map(|k| ThemeItem {
                    scope: ScopeSelectors::from_str(k.selectors()).expect("valid selectors"),
                    style: StyleModifier { foreground: Some(kind(k)), ..StyleModifier::default() },
                })
                .collect();
            for (sel, f) in [
                ("markup.bold", FontStyle::BOLD),
                ("markup.italic", FontStyle::ITALIC),
                ("markup.underline.link", FontStyle::UNDERLINE),
            ] {
                scopes.push(ThemeItem {
                    scope: ScopeSelectors::from_str(sel).expect("valid selector"),
                    style: StyleModifier { font_style: Some(f), ..StyleModifier::default() },
                });
            }
            SynTheme { settings: ThemeSettings { foreground: Some(kind(Kind::Plain)), ..ThemeSettings::default() }, scopes, ..SynTheme::default() }
        });
        Highlighter::new(theme)
    })
}

/// The grammar for a file: by name (`Makefile`), extension, then the first
/// line (`#!/bin/bash`, `<?xml`). None for plain text.
pub fn detect(path: Option<&Path>, first_line: &str) -> Option<usize> {
    let ss = syntaxes();
    let by_name = path.and_then(|p| {
        let name = p.file_name()?.to_str()?;
        let ext = p.extension().and_then(|e| e.to_str());
        ss.find_syntax_by_extension(name)
            .or_else(|| ext.and_then(|e| ss.find_syntax_by_extension(e)))
            .or_else(|| ext.and_then(|e| ss.find_syntax_by_extension(&e.to_ascii_lowercase())))
    });
    let found = by_name.or_else(|| ss.find_syntax_by_first_line(first_line))?;
    if found.name == "Plain Text" {
        return None;
    }
    ss.syntaxes().iter().position(|s| std::ptr::eq(s, found))
}

pub fn name(syntax: usize) -> &'static str {
    syntaxes().syntaxes().get(syntax).map_or("?", |s| s.name.as_str())
}

/// Grammars to offer in the language menu: (index, name, extensions),
/// sorted by name. Hidden helper grammars are left out.
pub fn menu() -> Vec<(usize, &'static str, String)> {
    let mut v: Vec<_> = syntaxes()
        .syntaxes()
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.hidden && s.name != "Plain Text")
        .map(|(i, s)| (i, s.name.as_str(), s.file_extensions.iter().take(6).map(String::as_str).collect::<Vec<_>>().join(" ")))
        .collect();
    v.sort_by_key(|(_, n, _)| n.to_lowercase());
    v
}

/// A stretch of a line from char offset `start` (to the next run's start).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub start: usize,
    pub kind: Kind,
    pub font: u8,
}

pub type Runs = Rc<[Run]>;

/// The run covering char offset `pos` of its line.
pub fn run_at(runs: &[Run], pos: usize) -> Option<Run> {
    let i = runs.partition_point(|r| r.start <= pos);
    runs.get(i.checked_sub(1)?).copied()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    parse: ParseState,
    hl: HighlightState,
}

impl State {
    fn start(syntax: &SyntaxReference) -> State {
        State { parse: ParseState::new(syntax), hl: HighlightState::new(highlighter(), ScopeStack::new()) }
    }

    /// Parse one line (its text without the line break), moving to the
    /// state at the start of the next.
    fn line(&mut self, text: &str) -> Runs {
        if text.len() > LONG_LINE {
            return Rc::from([]);
        }
        let mut owned = String::with_capacity(text.len() + 1);
        owned.push_str(text);
        owned.push('\n');
        let Ok(ops) = self.parse.parse_line(&owned, syntaxes()) else {
            return Rc::from([]);
        };
        let mut runs: Vec<Run> = Vec::new();
        let mut chars = 0;
        for (style, piece, _) in RangedHighlightIterator::new(&mut self.hl, &ops, &owned, highlighter()) {
            let run = Run { start: chars, kind: Kind::from_index(style.foreground.r), font: style.font_style.bits() };
            if runs.last().is_none_or(|r| (r.kind, r.font) != (run.kind, run.font)) {
                runs.push(run);
            }
            chars += piece.chars().count();
        }
        // Nothing but plain text: no runs at all.
        if runs.iter().all(|r| r.kind == Kind::Plain && r.font == 0) {
            runs.clear();
        }
        Rc::from(runs)
    }
}

/// A line's text without its line break.
fn line_text(rope: &Rope, line: usize) -> String {
    let mut s: String = rope.line(line).into();
    while s.ends_with(['\n', '\r', '\u{0b}', '\u{0c}', '\u{85}', '\u{2028}', '\u{2029}']) {
        s.pop();
    }
    s
}

/// Colouring state for one buffer and grammar.
pub struct Colouring {
    syntax: usize,
    /// The buffer version everything below describes.
    key: (u64, u64),
    lines: usize,
    /// The start states of lines `0..=exact` are known exactly.
    exact: usize,
    /// The state at `exact`, to carry on from without a replay.
    front: Option<(usize, State)>,
    /// Kept states at the start of some lines (line 0 is implied). Those
    /// after `exact` are from older text: provisional until confirmed.
    marks: BTreeMap<usize, State>,
    /// Boundaries of provisional data: lines after `exact` whose text
    /// changed, or where an earlier exact parse stopped. Past one of these,
    /// the kept runs (from the line on) and states (after it) came from
    /// other text, so a confirmed state vouches only up to the next one.
    dirty: BTreeSet<usize>,
    /// Runs per line: exact before `exact`, provisional after it.
    runs: HashMap<usize, Runs>,
    /// Guessed runs for lines past `exact`; dropped by any edit.
    guesses: HashMap<usize, Runs>,
    /// The lines last asked for (the view), where idle work heads.
    want: (usize, usize),
}

impl Colouring {
    pub fn new(syntax: usize, buf: &Buffer) -> Self {
        Self {
            syntax,
            key: buf.cache_key(),
            lines: buf.rope().len_lines(),
            exact: 0,
            front: None,
            marks: BTreeMap::new(),
            dirty: BTreeSet::new(),
            runs: HashMap::new(),
            guesses: HashMap::new(),
            want: (0, 0),
        }
    }

    pub fn syntax(&self) -> usize {
        self.syntax
    }

    fn reference(&self) -> &'static SyntaxReference {
        &syntaxes().syntaxes()[self.syntax]
    }

    /// Bring the cache up to the buffer's current text.
    fn sync(&mut self, buf: &Buffer) {
        match buf.changes_since(self.key) {
            Since::Same => {}
            Since::Unknown => *self = Colouring { want: self.want, ..Colouring::new(self.syntax, buf) },
            Since::Changed(c) => {
                let rope = buf.rope();
                let lines = rope.len_lines();
                let a = rope.char_to_line(c.at);
                let b = rope.char_to_line(c.at + c.inserted);
                // Lines after `b` are unchanged; they were `d` lines away.
                let d = lines as isize - self.lines as isize;
                let b_old = (b as isize - d).max(a as isize) as usize;
                let shift = |l: usize| (l as isize + d) as usize;
                // A state at the start of line `a` doesn't depend on line `a`.
                self.marks = std::mem::take(&mut self.marks)
                    .into_iter()
                    .filter_map(|(l, s)| if l <= a { Some((l, s)) } else if l > b_old { Some((shift(l), s)) } else { None })
                    .collect();
                self.runs = std::mem::take(&mut self.runs)
                    .into_iter()
                    .filter_map(|(l, r)| if l < a { Some((l, r)) } else if l > b_old { Some((shift(l), r)) } else { None })
                    .collect();
                self.dirty = std::mem::take(&mut self.dirty)
                    .into_iter()
                    .filter_map(|l| if l < a { Some(l) } else if l > b_old { Some(shift(l)) } else { None })
                    .collect();
                self.dirty.insert(a);
                // Data past the old exact point is from an older parse: it
                // must stay a boundary once the exact point moves back.
                if a < self.exact && self.exact < self.lines && self.exact > b_old {
                    self.dirty.insert(shift(self.exact));
                }
                self.exact = self.exact.min(a);
                self.dirty.retain(|&l| l > self.exact);
                if self.front.as_ref().is_some_and(|(l, _)| *l != self.exact) {
                    self.front = None;
                }
                self.guesses.clear();
                self.lines = lines;
            }
        }
        self.key = buf.cache_key();
    }

    /// The kept state nearest at or before `line` (exact if `line <= exact`).
    fn mark_before(&self, line: usize) -> (usize, State) {
        match self.marks.range(..=line).next_back() {
            Some((l, s)) => (*l, s.clone()),
            None => (0, State::start(self.reference())),
        }
    }

    /// Parse exactly from `exact` towards line `to` (exclusive) until
    /// `deadline`, keeping runs for the lines in `keep`. True when it got there.
    fn advance(&mut self, rope: &Rope, to: usize, deadline: Instant, keep: (usize, usize)) -> bool {
        let to = to.min(self.lines);
        let (mut line, mut st) = match self.front.take() {
            Some((l, s)) if l == self.exact => (l, s),
            _ => self.mark_before(self.exact),
        };
        let mut last_mark = self.marks.range(..=line).next_back().map_or(0, |(l, _)| *l);
        loop {
            if line > self.exact {
                // This line's start state is now known exactly.
                self.exact = line;
                self.dirty.retain(|&l| l > line);
                if let Some(old) = self.marks.get(&line) {
                    if *old == st {
                        // Converged: the old states and runs hold down to
                        // the next changed line. Carry on from there.
                        self.exact = self.dirty.first().copied().unwrap_or(self.lines).max(line);
                        self.dirty.retain(|&l| l > self.exact);
                        if self.exact >= to {
                            return true;
                        }
                        (line, st) = self.mark_before(self.exact);
                        last_mark = line;
                        continue;
                    }
                    self.marks.insert(line, st.clone());
                    last_mark = line;
                } else if line - last_mark >= CHECKPOINT {
                    self.marks.insert(line, st.clone());
                    last_mark = line;
                }
            } else if self.marks.contains_key(&line) {
                last_mark = line;
            }
            if line >= to || line >= self.lines {
                break;
            }
            // A replay up to `exact` is short (marks are close): finish it,
            // so the next call can carry on from `front`.
            if line >= self.exact && Instant::now() >= deadline {
                break;
            }
            let wanted = (keep.0..=keep.1).contains(&line);
            if line < self.exact && (!wanted || self.runs.contains_key(&line)) {
                // Replaying known ground: only the state matters.
                st.line(&line_text(rope, line));
            } else {
                let runs = st.line(&line_text(rope, line));
                if wanted {
                    self.runs.insert(line, runs);
                } else {
                    self.runs.remove(&line);
                }
            }
            line += 1;
        }
        if line == self.exact {
            self.front = Some((line, st));
        }
        self.exact >= to
    }

    /// Runs for lines `first..=last` of the view (None = draw it plain),
    /// parsing for at most `budget`: the rest are guessed, and `pending`
    /// says so until `idle` has finished them.
    pub fn view(&mut self, buf: &Buffer, first: usize, last: usize, budget: std::time::Duration) -> Vec<Option<Runs>> {
        self.sync(buf);
        let rope = buf.rope();
        let last = last.min(self.lines.saturating_sub(1));
        self.want = (first, last);
        if self.within_reach(rope, first) {
            self.advance(rope, last + 1, Instant::now() + budget, (first, last));
        }
        // Exact lines whose runs aren't kept: replay from the nearest mark.
        let missing: Vec<usize> = (first..=last.min(self.exact.saturating_sub(1))).filter(|l| !self.runs.contains_key(l)).collect();
        if let (Some(&lo), Some(&hi)) = (missing.first(), missing.last()) {
            let (mut line, mut st) = self.mark_before(lo);
            while line <= hi {
                let runs = st.line(&line_text(rope, line));
                if line >= first {
                    self.runs.insert(line, runs);
                }
                line += 1;
            }
        }
        // Lines past the exact parse: provisional runs, else a guess.
        let from = first.max(self.exact);
        if let Some(g0) = (from..=last).find(|l| !self.runs.contains_key(l) && !self.guesses.contains_key(l)) {
            let (mut line, mut st) = match self.marks.range(..=g0).next_back() {
                Some((l, s)) if g0 - l <= GUESS_REACH => (*l, s.clone()),
                _ => (g0.saturating_sub(GUESS_REACH), State::start(self.reference())),
            };
            while line <= last {
                let runs = st.line(&line_text(rope, line));
                if line >= g0 && !self.runs.contains_key(&line) {
                    self.guesses.insert(line, runs);
                }
                line += 1;
            }
        }
        let runs = (first..=last)
            .map(|l| if l < self.exact { self.runs.get(&l) } else { self.runs.get(&l).or_else(|| self.guesses.get(&l)) }.cloned())
            .collect();
        self.trim();
        runs
    }

    /// Continue the exact parse towards the last view for up to `budget`.
    /// True when it reached the view's bottom (time to redraw).
    pub fn idle(&mut self, buf: &Buffer, budget: std::time::Duration) -> bool {
        if self.pending(buf) {
            let want = self.want;
            self.advance(buf.rope(), want.1 + 1, Instant::now() + budget, want)
        } else {
            false
        }
    }

    /// The last view still has guessed lines that idle work will fix.
    pub fn pending(&mut self, buf: &Buffer) -> bool {
        self.key == buf.cache_key() && self.exact <= self.want.1.min(self.lines.saturating_sub(1)) && self.within_reach(buf.rope(), self.want.0)
    }

    /// `line` is close enough below the exact parse to parse up to.
    fn within_reach(&self, rope: &Rope, line: usize) -> bool {
        let at = |l: usize| rope.line_to_byte(l.min(rope.len_lines()));
        at(line).saturating_sub(at(self.exact)) <= EXACT_REACH
    }

    /// Forget runs far from the view.
    fn trim(&mut self) {
        let (a, b) = self.want;
        let near = |l: &usize| *l + MAX_RUNS / 4 >= a && *l <= b + MAX_RUNS / 4;
        if self.runs.len() > MAX_RUNS {
            self.runs.retain(|l, _| near(l));
        }
        if self.guesses.len() > MAX_RUNS / 4 {
            self.guesses.retain(|l, _| near(l));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{EditKind, Selection};
    use std::time::Duration;

    fn by_name(n: &str) -> usize {
        syntaxes().syntaxes().iter().position(|s| s.name == n).unwrap()
    }

    /// Kinds of each char of a line.
    fn kinds(runs: &Option<Runs>, len: usize) -> Vec<Kind> {
        (0..len).map(|i| runs.as_ref().and_then(|r| run_at(r, i)).map_or(Kind::Plain, |r| r.kind)).collect()
    }

    fn kind_of(c: &mut Colouring, buf: &Buffer, line: usize, word: &str) -> Kind {
        let text = line_text(buf.rope(), line);
        let at = text.find(word).unwrap_or_else(|| panic!("{word} not on line {line}"));
        let at = text[..at].chars().count();
        let v = c.view(buf, line, line, Duration::from_secs(5));
        kinds(&v[0], at + 1)[at]
    }

    #[test]
    fn detects_by_name_extension_and_first_line() {
        let d = |p: &str, first: &str| detect(Some(Path::new(p)), first).map(name);
        assert_eq!(d("x.py", ""), Some("Python"));
        assert_eq!(d("run", "#!/bin/bash"), Some("Bourne Again Shell (bash)"));
        assert_eq!(d("run", "#!/usr/bin/env python3"), Some("Python"));
        assert_eq!(d("Makefile", ""), Some("Makefile"));
        assert_eq!(d("Cargo.toml", ""), Some("TOML"));
        assert_eq!(d("README.md", ""), Some("Markdown"));
        assert_eq!(d("notes.txt", ""), None);
        assert_eq!(d("notes", "hello"), None);
        assert_eq!(detect(None, "#!/bin/sh").map(name), Some("Bourne Again Shell (bash)"));
        assert!(menu().iter().any(|(_, n, _)| *n == "Rust"));
    }

    #[test]
    fn kinds_for_common_tokens() {
        let buf = Buffer::from_str("# note\nname=\"x $HOME\"\nif true; then echo 3; fi\n");
        let mut c = Colouring::new(by_name("Bourne Again Shell (bash)"), &buf);
        assert_eq!(kind_of(&mut c, &buf, 0, "note"), Kind::Comment);
        assert_eq!(kind_of(&mut c, &buf, 1, "x "), Kind::String);
        assert_eq!(kind_of(&mut c, &buf, 1, "HOME"), Kind::Variable);
        assert_eq!(kind_of(&mut c, &buf, 2, "if"), Kind::Keyword);
        assert_eq!(kind_of(&mut c, &buf, 2, "echo"), Kind::Function);
        let buf = Buffer::from_str("def f(x: int) -> str:\n    return 'a' + 2\n");
        let mut c = Colouring::new(by_name("Python"), &buf);
        assert_eq!(kind_of(&mut c, &buf, 0, "def"), Kind::Keyword);
        assert_eq!(kind_of(&mut c, &buf, 0, "f("), Kind::Function);
        assert_eq!(kind_of(&mut c, &buf, 0, "int"), Kind::Type);
        assert_eq!(kind_of(&mut c, &buf, 1, "2"), Kind::Constant);
        let buf = Buffer::from_str("# Title\nSome `code` and **bold**.\n");
        let mut c = Colouring::new(by_name("Markdown"), &buf);
        assert_eq!(kind_of(&mut c, &buf, 0, "Title"), Kind::Heading);
        assert_eq!(kind_of(&mut c, &buf, 1, "code"), Kind::Code);
        let v = c.view(&buf, 1, 1, Duration::from_secs(5));
        let r = run_at(v[0].as_ref().unwrap(), 20).unwrap();
        assert_eq!(r.font & FONT_BOLD, FONT_BOLD);
        // Plain prose has no runs at all.
        assert!(c.view(&buf, 2, 2, Duration::from_secs(5))[0].as_ref().is_none_or(|r| r.is_empty()));
    }

    /// Whatever is edited, the incremental result equals a fresh parse.
    #[test]
    fn incremental_matches_fresh_parse() {
        let src: String = (0..400)
            .map(|i| match i % 7 {
                0 => format!("def f{i}(x):\n"),
                1 => "    \"\"\"doc\n".to_string(),
                2 => "    more doc\"\"\"\n".to_string(),
                3 => format!("    return x + {i}  # c\n"),
                4 => "s = '''\n".to_string(),
                5 => "'''\n".to_string(),
                _ => "\n".to_string(),
            })
            .collect();
        let py = by_name("Python");
        let mut buf = Buffer::from_str(&src);
        let mut c = Colouring::new(py, &buf);
        let mut seed = 11u64;
        let mut rnd = |m: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize % m.max(1)
        };
        for round in 0..200 {
            for _ in 0..1 + rnd(3) {
                let len = buf.len_chars();
                match rnd(7) {
                    0 => {
                        buf.undo();
                    }
                    1 => {
                        buf.redo();
                    }
                    2 => {
                        let at = rnd(len + 1);
                        buf.sel = Selection { anchor: at, head: (at + rnd(80)).min(len) };
                        buf.delete((at, at), EditKind::Other);
                    }
                    _ => {
                        buf.sel = Selection::caret(rnd(len + 1));
                        buf.insert(["\"\"\"", "'''", "#", "\n", "x = 1\n", "'"][rnd(6)], EditKind::Other);
                    }
                }
            }
            let n = buf.len_lines();
            let top = rnd(n);
            let bottom = (top + 40).min(n - 1);
            // Small budgets exercise the guess + idle path; unchecked
            // rounds leave idle work unfinished.
            let budget = [Duration::ZERO, Duration::from_micros(300), Duration::from_millis(50)][rnd(3)];
            c.view(&buf, top, bottom, budget);
            if rnd(2) == 0 {
                c.idle(&buf, Duration::from_micros(rnd(500) as u64));
                continue;
            }
            while c.pending(&buf) {
                c.idle(&buf, Duration::from_micros(200));
            }
            let got = c.view(&buf, top, bottom, Duration::ZERO);
            assert!(!c.pending(&buf));
            let mut fresh = Colouring::new(py, &buf);
            let want = fresh.view(&buf, top, bottom, Duration::from_secs(60));
            for (i, l) in (top..=bottom).enumerate() {
                let len = line_text(buf.rope(), l).chars().count() + 1;
                assert_eq!(kinds(&got[i], len), kinds(&want[i], len), "round {round}, line {l}");
            }
        }
    }
}
