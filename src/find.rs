//! Find and replace: the search engine, the find bar's state, and its
//! commands (as an `impl App` block, to keep app.rs manageable).

use crate::app::App;
use crate::buffer::{Buffer, EditKind, Selection, is_word_char};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Cap on cached matches so a one-letter query in a huge file stays cheap.
const MAX_MATCHES: usize = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchOpts {
    pub match_case: bool,
    pub whole_word: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Find,
    Replace,
}

/// Clickable parts of the find bar (recorded at render time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindHit {
    FindField,
    ReplaceField,
    MatchCase,
    WholeWord,
    InSelection,
    Prev,
    Next,
    ReplaceOne,
    ReplaceAll,
    Close,
}

pub struct FindBar {
    pub query: String,
    pub replacement: String,
    pub focus: Field,
    pub replace_mode: bool,
    pub opts: SearchOpts,
    /// Restrict to this char range (In selection). Adjusted as replacements
    /// change its length.
    pub scope: Option<(usize, usize)>,
    /// Caret when the bar opened: incremental search starts here.
    origin: usize,
    cache: Option<Cache>,
}

/// (buffer id+version, query, options, scope): matches are valid while equal.
type CacheKey = ((u64, u64), String, SearchOpts, Option<(usize, usize)>);

struct Cache {
    key: CacheKey,
    matches: Vec<(usize, usize)>,
    truncated: bool,
}

impl FindBar {
    pub fn new(query: String, origin: usize) -> Self {
        Self {
            query,
            replacement: String::new(),
            focus: Field::Find,
            replace_mode: false,
            opts: SearchOpts::default(),
            scope: None,
            origin,
            cache: None,
        }
    }

    /// All matches (char ranges, ascending), cached per buffer version.
    pub fn matches(&mut self, buf: &Buffer) -> &[(usize, usize)] {
        let key = (buf.cache_key(), self.query.clone(), self.opts, self.scope);
        if self.cache.as_ref().is_none_or(|c| c.key != key) {
            let (matches, truncated) = find_all_chars(buf, &self.query, self.opts, self.scope);
            self.cache = Some(Cache { key, matches, truncated });
        }
        &self.cache.as_ref().unwrap().matches
    }

    pub fn truncated(&self) -> bool {
        self.cache.as_ref().is_some_and(|c| c.truncated)
    }

    fn field_mut(&mut self) -> &mut String {
        match self.focus {
            Field::Find => &mut self.query,
            Field::Replace => &mut self.replacement,
        }
    }
}

/// Lowercase only where it keeps the UTF-8 length identical, so byte offsets
/// in the lowered string still map onto the original text.
pub fn lower_same_len(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut l = c.to_lowercase();
            match (l.next(), l.next()) {
                (Some(lc), None) if lc.len_utf8() == c.len_utf8() => lc,
                _ => c,
            }
        })
        .collect()
}

/// Non-overlapping literal matches in `text` as byte ranges.
pub fn find_all_bytes(text: &str, query: &str, opts: SearchOpts) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let (hay, needle) = if opts.match_case {
        (std::borrow::Cow::Borrowed(text), query.to_string())
    } else {
        (std::borrow::Cow::Owned(lower_same_len(text)), lower_same_len(query))
    };
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = hay[from..].find(&needle) {
        let s = from + i;
        let e = s + needle.len();
        let ok = !opts.whole_word
            || (text[..s].chars().next_back().is_none_or(|c| !is_word_char(c))
                && text[e..].chars().next().is_none_or(|c| !is_word_char(c)));
        if ok {
            out.push((s, e));
            from = e;
        } else {
            from = s + hay[s..].chars().next().map_or(1, char::len_utf8);
        }
        if from >= hay.len() {
            break;
        }
    }
    out
}

/// Matches in the buffer as char ranges, optionally limited to a scope.
pub fn find_all_chars(
    buf: &Buffer,
    query: &str,
    opts: SearchOpts,
    scope: Option<(usize, usize)>,
) -> (Vec<(usize, usize)>, bool) {
    let rope = buf.rope();
    let (start, end) = scope.unwrap_or((0, buf.len_chars()));
    let end = end.min(buf.len_chars());
    if query.is_empty() || start >= end {
        return (Vec::new(), false);
    }
    let text = rope.slice(start..end).to_string();
    let base = rope.char_to_byte(start);
    let mut v = find_all_bytes(&text, query, opts);
    let truncated = v.len() > MAX_MATCHES;
    v.truncate(MAX_MATCHES);
    let out = v
        .into_iter()
        .map(|(s, e)| (rope.byte_to_char(base + s), rope.byte_to_char(base + e)))
        .collect();
    (out, truncated)
}

impl App {
    /// Open (or refocus) the find bar; `replace` opens it in replace mode.
    pub fn open_find(&mut self, replace: bool) {
        let sel = self.tab.buf.sel;
        let selected = self.tab.buf.selected_text();
        let multi_line = selected.contains('\n');
        let bar = self.find.get_or_insert_with(|| {
            let mut b = FindBar::new(self.last_find.clone(), sel.start());
            b.opts = self.last_find_opts;
            b
        });
        if !selected.is_empty() && !multi_line {
            bar.query = selected;
        }
        if multi_line {
            bar.scope = Some((sel.start(), sel.end()));
        }
        if replace {
            bar.replace_mode = true;
            bar.focus = if bar.query.is_empty() { Field::Find } else { Field::Replace };
        } else {
            bar.focus = Field::Find;
        }
    }

    pub fn close_find(&mut self) {
        if let Some(b) = self.find.take() {
            self.last_find = b.query;
            self.last_find_opts = b.opts;
        }
    }

    /// Returns true if the key was handled by the find bar.
    pub fn on_find_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let Some(bar) = &mut self.find else { return false };
        let lower = match k.code {
            KeyCode::Char(c) => Some(c.to_ascii_lowercase()),
            _ => None,
        };
        match (k.code, lower) {
            (KeyCode::Esc, _) => self.close_find(),
            (_, Some('f')) if ctrl => bar.focus = Field::Find,
            (_, Some('h' | 'r')) if ctrl => {
                bar.replace_mode = true;
                bar.focus = Field::Replace;
            }
            (_, Some('c')) if alt => self.toggle_find_opt(FindHit::MatchCase),
            (_, Some('w')) if alt => self.toggle_find_opt(FindHit::WholeWord),
            (_, Some('l')) if alt => self.toggle_find_opt(FindHit::InSelection),
            (_, Some('a')) if alt => self.replace_all(),
            (KeyCode::Enter, _) if alt || (ctrl && alt) => self.replace_all(),
            (KeyCode::Enter, _) if bar.focus == Field::Replace => self.replace_one(),
            (KeyCode::Enter, _) => self.find_step(!shift),
            (KeyCode::F(3), _) => self.find_step(!shift),
            (KeyCode::Tab | KeyCode::BackTab, _) if bar.replace_mode => {
                bar.focus = if bar.focus == Field::Find { Field::Replace } else { Field::Find };
            }
            (KeyCode::Backspace, _) => {
                bar.field_mut().pop();
                if bar.focus == Field::Find {
                    self.find_incremental();
                }
            }
            (_, Some('v')) if ctrl => {
                let t = self.clipboard.paste();
                self.find_insert_text(&t);
            }
            (KeyCode::Char(c), _) if !ctrl && !alt => {
                let mut b = [0u8; 4];
                self.find_insert_text(c.encode_utf8(&mut b));
            }
            // Anything else (arrows, Ctrl+S, ...) goes to the editor.
            _ => return false,
        }
        true
    }

    /// Text typed or pasted into the focused field (newlines become spaces).
    pub fn find_insert_text(&mut self, t: &str) {
        let Some(bar) = &mut self.find else { return };
        bar.field_mut().push_str(&t.replace(['\r', '\n'], " "));
        if bar.focus == Field::Find {
            self.find_incremental();
        }
    }

    pub fn on_find_click(&mut self, hit: FindHit) {
        let Some(bar) = &mut self.find else { return };
        match hit {
            FindHit::FindField => bar.focus = Field::Find,
            FindHit::ReplaceField => bar.focus = Field::Replace,
            FindHit::MatchCase | FindHit::WholeWord | FindHit::InSelection => self.toggle_find_opt(hit),
            FindHit::Prev => self.find_step(false),
            FindHit::Next => self.find_step(true),
            FindHit::ReplaceOne => self.replace_one(),
            FindHit::ReplaceAll => self.replace_all(),
            FindHit::Close => self.close_find(),
        }
    }

    fn toggle_find_opt(&mut self, which: FindHit) {
        let sel = self.tab.buf.sel;
        let Some(bar) = &mut self.find else { return };
        match which {
            FindHit::MatchCase => bar.opts.match_case = !bar.opts.match_case,
            FindHit::WholeWord => bar.opts.whole_word = !bar.opts.whole_word,
            FindHit::InSelection => {
                bar.scope = match bar.scope {
                    Some(_) => None,
                    None if !sel.is_empty() => Some((sel.start(), sel.end())),
                    None => {
                        self.warn("Select some text first to search within it");
                        return;
                    }
                }
            }
            _ => {}
        }
        self.find_incremental();
    }

    /// Select the first match at or after where the search started.
    fn find_incremental(&mut self) {
        let Some(bar) = &mut self.find else { return };
        let origin = bar.origin;
        let m = bar.matches(&self.tab.buf);
        let hit = m.iter().find(|&&(s, _)| s >= origin).or(m.first()).copied();
        if let Some((s, e)) = hit {
            self.tab.buf.sel = Selection { anchor: s, head: e };
            self.tab.follow_caret = true;
        }
    }

    /// Move to the next/previous match, wrapping around.
    pub fn find_step(&mut self, forward: bool) {
        let sel = self.tab.buf.sel;
        let Some(bar) = &mut self.find else {
            // F3 with the bar closed: reopen with the last search.
            self.open_find(false);
            if !self.find.as_ref().is_some_and(|b| b.query.is_empty()) {
                self.find_step(forward);
            }
            return;
        };
        let query = bar.query.clone();
        let m = bar.matches(&self.tab.buf);
        if m.is_empty() {
            if !query.is_empty() {
                self.warn(format!("Not found: {query}"));
            }
            return;
        }
        // Matches never have zero length, so "starts at or after the
        // selection end" skips the current match and nothing else.
        let (target, wrapped) = if forward {
            match m.iter().find(|&&(s, _)| s >= sel.end()) {
                Some(&r) => (r, false),
                None => (m[0], true),
            }
        } else {
            match m.iter().rev().find(|&&(s, _)| s < sel.start()) {
                Some(&r) => (r, false),
                None => (*m.last().unwrap(), true),
            }
        };
        if let Some(bar) = &mut self.find {
            bar.origin = target.0;
        }
        self.tab.buf.sel = Selection { anchor: target.0, head: target.1 };
        self.tab.buf.break_undo_group();
        self.tab.follow_caret = true;
        if wrapped {
            self.info("Search wrapped");
        }
    }

    /// "3 of 17" style position of the current selection among matches.
    pub fn find_status(&mut self) -> String {
        let sel = self.tab.buf.sel;
        let Some(bar) = &mut self.find else { return String::new() };
        if bar.query.is_empty() {
            return String::new();
        }
        let (n, pos) = {
            let m = bar.matches(&self.tab.buf);
            (m.len(), m.binary_search(&(sel.start(), sel.end())))
        };
        let more = if bar.truncated() { "+" } else { "" };
        match pos {
            Ok(i) => format!("{} of {n}{more}", i + 1),
            Err(_) if n == 0 => "No results".into(),
            Err(_) => format!("{n}{more} found"),
        }
    }

    /// Replace the current match (if the selection is one), then go to the next.
    fn replace_one(&mut self) {
        let sel = self.tab.buf.sel;
        let Some(bar) = &mut self.find else { return };
        let replacement = bar.replacement.clone();
        let on_match = bar.matches(&self.tab.buf).binary_search(&(sel.start(), sel.end())).is_ok();
        if on_match {
            let delta = replacement.chars().count() as isize - (sel.end() - sel.start()) as isize;
            if let Some((s, e)) = bar.scope {
                bar.scope = Some((s, (e as isize + delta).max(s as isize) as usize));
            }
            self.tab.buf.insert(&replacement, EditKind::Other);
            self.tab.buf.break_undo_group();
        }
        self.find_step(true);
    }

    /// Replace every match (within the scope) as one undo step.
    pub fn replace_all(&mut self) {
        let Some(bar) = &mut self.find else { return };
        let replacement = bar.replacement.clone();
        let matches = bar.matches(&self.tab.buf).to_vec();
        if matches.is_empty() {
            let q = bar.query.clone();
            self.warn(format!("Not found: {q}"));
            return;
        }
        let rlen = replacement.chars().count() as isize;
        let delta: isize = matches.iter().map(|&(s, e)| rlen - (e - s) as isize).sum();
        if let Some((s, e)) = bar.scope {
            bar.scope = Some((s, (e as isize + delta).max(s as isize) as usize));
        }
        let n = matches.len();
        let ops = matches.into_iter().map(|(s, e)| (s, e - s, replacement.clone())).collect();
        self.tab.buf.transact(ops);
        self.tab.follow_caret = true;
        self.info(format!("Replaced {n} occurrence{} (Ctrl+Z undoes)", if n == 1 { "" } else { "s" }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_case_and_whole_word() {
        let t = "Cat cat concat CAT_x cat.";
        let o = SearchOpts::default();
        assert_eq!(find_all_bytes(t, "cat", o).len(), 5);
        let cs = SearchOpts { match_case: true, ..o };
        assert_eq!(find_all_bytes(t, "cat", cs), vec![(4, 7), (11, 14), (21, 24)]);
        let ww = SearchOpts { whole_word: true, ..o };
        assert_eq!(find_all_bytes(t, "cat", ww), vec![(0, 3), (4, 7), (21, 24)]);
    }

    #[test]
    fn multibyte_offsets_stay_aligned() {
        let b = Buffer::from_str("café Ünïcode ünïcode");
        let (m, _) = find_all_chars(&b, "ÜNÏ", SearchOpts::default(), None);
        assert_eq!(m, vec![(5, 8), (13, 16)]);
        let (m, _) = find_all_chars(&b, "ünï", SearchOpts { match_case: true, ..Default::default() }, None);
        assert_eq!(m, vec![(13, 16)]);
    }

    #[test]
    fn scope_limits_matches() {
        let b = Buffer::from_str("a a a a");
        let (m, _) = find_all_chars(&b, "a", SearchOpts::default(), Some((2, 5)));
        assert_eq!(m, vec![(2, 3), (4, 5)]);
    }

    #[test]
    fn overlapping_candidates_skip_correctly_for_whole_word() {
        let ww = SearchOpts { whole_word: true, ..Default::default() };
        assert_eq!(find_all_bytes("aaa aa", "aa", ww), vec![(4, 6)]);
    }
}
