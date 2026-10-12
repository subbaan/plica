//! The Highlights page of Preferences: a list of groups (each drawn in its
//! own style as a preview) and an editor for one group. Changes apply live
//! to `App::highlights`; Cancel restores the snapshot taken when
//! Preferences opened, Save writes `[[highlights.group]]` to config.toml.
//!
//! Built-in samples are shown after the user's groups. Changing a sample
//! (even switching it off) copies it into the user's groups under the same
//! name; the copy takes the sample's place, so the list doesn't reorder.

use crate::app::{App, Prompt, PromptKind};
use crate::highlight::{Colour, Group, Highlights, Origin, Role};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Rows of the group list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListRow {
    Enabled,
    Samples,
    Group(usize),
    New,
}

/// Rows of the group editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditRow {
    Name,
    Terms,
    Fg,
    Bg,
    Bold,
    Italic,
    Underline,
    WholeWord,
    MatchCase,
    FileTypes,
    Enabled,
    Back,
}

impl EditRow {
    pub const ALL: [EditRow; 12] = [
        EditRow::Name,
        EditRow::Terms,
        EditRow::Fg,
        EditRow::Bg,
        EditRow::Bold,
        EditRow::Italic,
        EditRow::Underline,
        EditRow::WholeWord,
        EditRow::MatchCase,
        EditRow::FileTypes,
        EditRow::Enabled,
        EditRow::Back,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EditRow::Name => "Name",
            EditRow::Terms => "Words / phrases",
            EditRow::Fg => "Text colour",
            EditRow::Bg => "Background",
            EditRow::Bold => "Bold",
            EditRow::Italic => "Italic",
            EditRow::Underline => "Underline",
            EditRow::WholeWord => "Whole words",
            EditRow::MatchCase => "Match case",
            EditRow::FileTypes => "File types",
            EditRow::Enabled => "Enabled",
            EditRow::Back => "‹ Back to groups",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            EditRow::Name | EditRow::Terms | EditRow::FileTypes => "Enter to edit (lists are comma-separated)",
            EditRow::Fg | EditRow::Bg => "←/→ palette colours & roles, Enter to type #rrggbb",
            EditRow::WholeWord => "Off: also inside longer words",
            EditRow::Back => "Esc also goes back",
            _ => "Space or ←/→ to toggle",
        }
    }

    pub fn value(self, g: &Group) -> String {
        let b = |v: bool| if v { "[x] on".to_string() } else { "[ ] off".to_string() };
        let colour = |c: Option<Colour>| format!("‹ {} ›", c.map_or("none".into(), Colour::name));
        match self {
            EditRow::Name => g.name.clone(),
            EditRow::Terms => g.terms.join(", "),
            EditRow::Fg => colour(g.fg),
            EditRow::Bg => colour(g.bg),
            EditRow::Bold => b(g.bold),
            EditRow::Italic => b(g.italic),
            EditRow::Underline => b(g.underline),
            EditRow::WholeWord => b(g.whole_word),
            EditRow::MatchCase => b(g.case_sensitive),
            EditRow::FileTypes => {
                if g.file_types.is_empty() {
                    "(all files)".into()
                } else {
                    g.file_types.join(", ")
                }
            }
            EditRow::Enabled => b(g.enabled),
            EditRow::Back => String::new(),
        }
    }
}

/// The colours ←/→ step through: none, roles, then palette slots.
fn colour_cycle() -> Vec<Option<Colour>> {
    let mut v = vec![None];
    for r in [Role::Accent, Role::Warning, Role::Search, Role::Selection, Role::Muted, Role::Foreground, Role::Background] {
        v.push(Some(Colour::Role(r)));
    }
    v.extend((0..16).map(|i| Some(Colour::Slot(i))));
    v
}

pub fn step_colour(c: Option<Colour>, dir: i32) -> Option<Colour> {
    let all = colour_cycle();
    match all.iter().position(|x| *x == c) {
        Some(i) => all[(i as i32 + dir).rem_euclid(all.len() as i32) as usize],
        // A fixed colour isn't in the cycle: step to the ends.
        None if dir > 0 => all[0],
        None => all[all.len() - 1],
    }
}

pub fn list_rows(h: &Highlights) -> Vec<ListRow> {
    let mut v = vec![ListRow::Enabled, ListRow::Samples];
    v.extend((0..h.ordered().len()).map(ListRow::Group));
    v.push(ListRow::New);
    v
}

/// Make display group `i` editable (copying a sample) and return its index
/// in `h.groups`.
fn materialise(h: &mut Highlights, i: usize) -> Option<usize> {
    let (g, origin) = h.ordered().into_iter().nth(i)?;
    match origin {
        Origin::User(j) | Origin::Override(j) => Some(j),
        Origin::Sample => {
            h.groups.push(g);
            Some(h.groups.len() - 1)
        }
    }
}

/// List row of `h.groups[gi]`.
fn row_of(h: &Highlights, gi: usize) -> usize {
    let pos = h.ordered().iter().position(|(_, o)| matches!(o, Origin::User(j) | Origin::Override(j) if *j == gi));
    2 + pos.unwrap_or(0)
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|t| !t.is_empty()).map(String::from).collect()
}

/// Clickable parts of the Highlights page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlHit {
    /// A list row (select; on a group, open the editor).
    Row(usize),
    /// A list row's checkbox.
    Toggle(usize),
    EditRow(usize),
    Dec(usize),
    Inc(usize),
}

impl App {
    fn hl_state(&mut self) -> Option<&mut crate::prefs::PrefsState> {
        self.prefs.as_mut()
    }

    /// Keys on the Highlights page. Returns false for keys the page leaves to
    /// the dialog (Tab, Ctrl+S, Esc in the list).
    pub fn on_prefs_hl_key(&mut self, k: KeyEvent) -> bool {
        let alt_or_ctrl = k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL);
        let Some(p) = self.prefs.as_ref() else { return false };
        if let Some(gi) = p.hl_edit {
            let row = EditRow::ALL[p.hl_edit_row.min(EditRow::ALL.len() - 1)];
            match k.code {
                KeyCode::Esc => self.hl_back(),
                KeyCode::Up | KeyCode::Down => {
                    if let Some(s) = self.hl_state() {
                        s.hl_edit_row = if k.code == KeyCode::Up {
                            s.hl_edit_row.saturating_sub(1)
                        } else {
                            (s.hl_edit_row + 1).min(EditRow::ALL.len() - 1)
                        };
                    }
                }
                KeyCode::Left | KeyCode::Char('-') => self.hl_edit_change(gi, row, -1),
                KeyCode::Right | KeyCode::Char('+') => self.hl_edit_change(gi, row, 1),
                KeyCode::Enter | KeyCode::Char(' ') => self.hl_edit_change(gi, row, 0),
                KeyCode::Delete | KeyCode::Backspace if matches!(row, EditRow::Fg | EditRow::Bg | EditRow::FileTypes) => {
                    self.hl_set(gi, |g| match row {
                        EditRow::Fg => g.fg = None,
                        EditRow::Bg => g.bg = None,
                        _ => g.file_types.clear(),
                    })
                }
                KeyCode::Tab | KeyCode::BackTab | KeyCode::PageDown | KeyCode::PageUp => return false,
                KeyCode::Char('s') if alt_or_ctrl => return false,
                _ => {}
            }
            return true;
        }
        let rows = list_rows(&self.highlights);
        let cur = p.hl_row.min(rows.len() - 1);
        match (k.code, rows[cur]) {
            (KeyCode::Up, ListRow::Group(i)) if alt_or_ctrl => self.hl_move(i, -1),
            (KeyCode::Down, ListRow::Group(i)) if alt_or_ctrl => self.hl_move(i, 1),
            (KeyCode::Up | KeyCode::Down, _) => {
                let to = if k.code == KeyCode::Up { cur.saturating_sub(1) } else { (cur + 1).min(rows.len() - 1) };
                if let Some(s) = self.hl_state() {
                    s.hl_row = to;
                }
            }
            (KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right, r) => self.hl_toggle(r),
            (KeyCode::Enter, ListRow::Group(i)) => self.hl_open(i),
            (KeyCode::Enter, r @ (ListRow::Enabled | ListRow::Samples)) => self.hl_toggle(r),
            (KeyCode::Enter, ListRow::New) | (KeyCode::Char('n'), _) => self.hl_new(),
            (KeyCode::Delete, ListRow::Group(i)) => self.hl_delete(i),
            _ => return false,
        }
        true
    }

    pub fn on_prefs_hl_click(&mut self, hit: HlHit) {
        match hit {
            HlHit::Row(i) => {
                if let Some(s) = self.hl_state() {
                    s.hl_row = i;
                }
                match list_rows(&self.highlights).get(i) {
                    Some(&ListRow::Group(g)) => self.hl_open(g),
                    Some(&ListRow::New) => self.hl_new(),
                    Some(&r) => self.hl_toggle(r),
                    None => {}
                }
            }
            HlHit::Toggle(i) => {
                if let Some(s) = self.hl_state() {
                    s.hl_row = i;
                }
                if let Some(&r) = list_rows(&self.highlights).get(i) {
                    self.hl_toggle(r);
                }
            }
            HlHit::EditRow(i) | HlHit::Dec(i) | HlHit::Inc(i) => {
                let Some(gi) = self.prefs.as_ref().and_then(|p| p.hl_edit) else { return };
                if let Some(s) = self.hl_state() {
                    s.hl_edit_row = i;
                }
                let dir = match hit {
                    HlHit::Dec(_) => -1,
                    HlHit::Inc(_) => 1,
                    _ => 0,
                };
                self.hl_edit_change(gi, EditRow::ALL[i], dir);
            }
        }
    }

    fn hl_toggle(&mut self, r: ListRow) {
        match r {
            ListRow::Enabled => self.highlights.enabled = !self.highlights.enabled,
            ListRow::Samples => self.highlights.samples = !self.highlights.samples,
            ListRow::Group(i) => {
                if let Some(gi) = materialise(&mut self.highlights, i) {
                    let g = &mut self.highlights.groups[gi];
                    g.enabled = !g.enabled;
                }
            }
            ListRow::New => self.hl_new(),
        }
    }

    fn hl_open(&mut self, display_index: usize) {
        let Some(gi) = materialise(&mut self.highlights, display_index) else { return };
        if let Some(s) = self.hl_state() {
            s.hl_edit = Some(gi);
            s.hl_edit_row = 0;
            s.hl_row = 2 + display_index;
        }
    }

    fn hl_back(&mut self) {
        let Some(gi) = self.prefs.as_mut().and_then(|s| s.hl_edit.take()) else { return };
        if self.highlights.groups.get(gi).is_some_and(|g| g.terms.is_empty()) {
            self.highlights.groups.remove(gi);
            self.info("Group without words removed");
            let n = list_rows(&self.highlights).len();
            if let Some(s) = self.hl_state() {
                s.hl_row = s.hl_row.min(n - 1);
            }
        }
    }

    fn hl_new(&mut self) {
        let n = self.highlights.groups.len() + 1;
        let g = Group { fg: Some(Colour::Role(Role::Accent)), ..Group::new(&format!("Group {n}")) };
        self.highlights.groups.push(g);
        let gi = self.highlights.groups.len() - 1;
        let row = row_of(&self.highlights, gi);
        if let Some(s) = self.hl_state() {
            s.hl_edit = Some(gi);
            s.hl_edit_row = 1;
            s.hl_row = row;
        }
        // Start with the words: a group without terms does nothing.
        self.prompt = Some(Prompt::new(PromptKind::HlTerms, ""));
    }

    fn hl_delete(&mut self, display_index: usize) {
        match self.highlights.ordered().get(display_index).map(|x| x.1) {
            Some(Origin::User(j)) => {
                let name = self.highlights.groups.remove(j).name;
                self.info(format!("Removed group {name}"));
            }
            Some(Origin::Override(j)) => {
                let name = self.highlights.groups.remove(j).name;
                self.info(format!("{name} is back to the built-in sample"));
            }
            Some(Origin::Sample) => {
                // A sample can't be deleted, only switched off.
                if let Some(gi) = materialise(&mut self.highlights, display_index) {
                    self.highlights.groups[gi].enabled = false;
                }
                self.info("Samples can be switched off (or all hidden with Sample groups off)");
            }
            None => {}
        }
        let n = list_rows(&self.highlights).len();
        if let Some(s) = self.hl_state() {
            s.hl_row = s.hl_row.min(n - 1);
        }
    }

    /// Move a user group up/down in priority.
    fn hl_move(&mut self, display_index: usize, dir: i32) {
        let order = self.highlights.ordered();
        let Some(&(_, Origin::User(a))) = order.get(display_index) else {
            self.info("Sample groups keep their place after your own groups");
            return;
        };
        let to = display_index as i32 + dir;
        let Some(&(_, Origin::User(b))) = (to >= 0).then(|| order.get(to as usize)).flatten() else { return };
        self.highlights.groups.swap(a, b);
        if let Some(s) = self.hl_state() {
            s.hl_row = 2 + to as usize;
        }
    }

    fn hl_set(&mut self, gi: usize, f: impl FnOnce(&mut Group)) {
        if let Some(g) = self.highlights.groups.get_mut(gi) {
            f(g);
        }
    }

    fn hl_edit_change(&mut self, gi: usize, row: EditRow, dir: i32) {
        let Some(g) = self.highlights.groups.get(gi) else { return };
        let prompt = |kind: PromptKind, text: String| Some(Prompt::new(kind, &text));
        match row {
            EditRow::Back => self.hl_back(),
            EditRow::Name if dir == 0 => self.prompt = prompt(PromptKind::HlName, g.name.clone()),
            EditRow::Terms if dir == 0 => self.prompt = prompt(PromptKind::HlTerms, g.terms.join(", ")),
            EditRow::FileTypes if dir == 0 => self.prompt = prompt(PromptKind::HlFileTypes, g.file_types.join(", ")),
            EditRow::Fg if dir == 0 => self.prompt = prompt(PromptKind::HlFg, g.fg.map(Colour::name).unwrap_or_default()),
            EditRow::Bg if dir == 0 => self.prompt = prompt(PromptKind::HlBg, g.bg.map(Colour::name).unwrap_or_default()),
            EditRow::Fg => self.hl_set(gi, |g| g.fg = step_colour(g.fg, dir)),
            EditRow::Bg => self.hl_set(gi, |g| g.bg = step_colour(g.bg, dir)),
            EditRow::Bold => self.hl_set(gi, |g| g.bold = !g.bold),
            EditRow::Italic => self.hl_set(gi, |g| g.italic = !g.italic),
            EditRow::Underline => self.hl_set(gi, |g| g.underline = !g.underline),
            EditRow::WholeWord => self.hl_set(gi, |g| g.whole_word = !g.whole_word),
            EditRow::MatchCase => self.hl_set(gi, |g| g.case_sensitive = !g.case_sensitive),
            EditRow::Enabled => self.hl_set(gi, |g| g.enabled = !g.enabled),
            EditRow::Name | EditRow::Terms | EditRow::FileTypes => {}
        }
    }

    /// Result of one of the Highlights prompts.
    pub fn hl_prompt_done(&mut self, kind: PromptKind, input: &str) {
        let Some(gi) = self.prefs.as_ref().and_then(|p| p.hl_edit) else { return };
        let input = input.trim();
        let colour = |s: &str| -> Result<Option<Colour>, String> {
            if s.is_empty() { Ok(None) } else { Colour::parse(s).map(Some) }
        };
        match kind {
            PromptKind::HlName if !input.is_empty() => self.hl_set(gi, |g| g.name = input.into()),
            PromptKind::HlTerms => {
                let terms = split_list(input);
                if terms.is_empty() {
                    self.warn("A group needs at least one word or phrase");
                }
                self.hl_set(gi, |g| g.terms = terms);
            }
            PromptKind::HlFileTypes => self.hl_set(gi, |g| {
                g.file_types = split_list(input).into_iter().map(|t| t.trim_start_matches('.').to_ascii_lowercase()).collect()
            }),
            PromptKind::HlFg | PromptKind::HlBg => match colour(input) {
                Ok(c) => self.hl_set(gi, |g| if kind == PromptKind::HlFg { g.fg = c } else { g.bg = c }),
                Err(e) => self.warn(e),
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_cycle_wraps_and_handles_fixed() {
        assert_eq!(step_colour(None, 1), Some(Colour::Role(Role::Accent)));
        assert_eq!(step_colour(None, -1), Some(Colour::Slot(15)));
        assert_eq!(step_colour(Some(Colour::Slot(15)), 1), None);
        assert_eq!(step_colour(Some(Colour::Fixed(crate::theme::Rgb(1, 2, 3))), 1), None);
    }

    #[test]
    fn editing_a_sample_makes_it_a_user_group() {
        use crate::highlight::samples;
        let mut h = Highlights::default();
        h.groups.push(Group { terms: vec!["x".into()], ..Group::new("Mine") });
        let n = h.ordered().len();
        assert_eq!(n, samples().len() + 1);
        let gi = materialise(&mut h, 3).unwrap();
        assert_eq!(h.groups[gi].name, samples()[2].name);
        let order = h.ordered();
        assert_eq!(order.len(), n, "the copy replaces the sample, no duplicate");
        assert_eq!(order[3].1, Origin::Override(gi), "and keeps the sample's place");
        assert_eq!(order[0].1, Origin::User(0));
    }

    #[test]
    fn list_split() {
        assert_eq!(split_list(" Alice,  release day ,,"), ["Alice", "release day"]);
    }
}
