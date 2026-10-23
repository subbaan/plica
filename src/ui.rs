//! Rendering. Everything here asks the `Theme` for semantic roles; only
//! `Painter` knows how roles become terminal colours.

use crate::app::{App, Button, Level, TabHit, ThemeSource};
use crate::find::FindHit;
use crate::layout::GlyphKind;
use crate::lineedit::LineEdit;
use crate::theme::Rgb;
use ratatui::Frame;
use ratatui::buffer::Buffer as TermBuf;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use std::time::Duration;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Converts theme colours to terminal colours for the active colour mode.
pub struct Painter {
    pub truecolor: bool,
    pub plain: bool,
}

impl Painter {
    fn c(&self, rgb: Rgb) -> Color {
        if self.plain {
            Color::Reset
        } else if self.truecolor {
            Color::Rgb(rgb.0, rgb.1, rgb.2)
        } else {
            Color::Indexed(nearest_256(rgb))
        }
    }

    fn style(&self, fg: Rgb, bg: Rgb) -> Style {
        Style::default().fg(self.c(fg)).bg(self.c(bg))
    }

    /// In plain mode, emphasis must come from attributes instead of colour.
    fn plain_or(&self, s: Style, m: Modifier) -> Style {
        if self.plain { Style::default().add_modifier(m) } else { s }
    }
}

fn nearest_256(c: Rgb) -> u8 {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let idx = |v: u8| STEPS.iter().enumerate().min_by_key(|(_, s)| (**s as i32 - v as i32).abs()).unwrap().0;
    let (r, g, b) = (idx(c.0), idx(c.1), idx(c.2));
    let cube = 16 + 36 * r + 6 * g + b;
    let cube_rgb = (STEPS[r], STEPS[g], STEPS[b]);
    let avg = (c.0 as u32 + c.1 as u32 + c.2 as u32) / 3;
    let gi = (((avg as i32 - 8).max(0) / 10).min(23)) as u8;
    let gv = 8 + 10 * gi as i32;
    let d = |a: (i32, i32, i32)| (a.0 - c.0 as i32).pow(2) + (a.1 - c.1 as i32).pow(2) + (a.2 - c.2 as i32).pow(2);
    if d((gv, gv, gv)) < d((cube_rgb.0 as i32, cube_rgb.1 as i32, cube_rgb.2 as i32)) {
        232 + gi
    } else {
        cube as u8
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.width < 10 || area.height < 4 {
        return;
    }
    let p = Painter {
        truecolor: app.caps.truecolor,
        plain: app.theme_source == ThemeSource::TerminalDefault,
    };
    let t = app.theme.clone();
    let buf = f.buffer_mut();

    let title = Rect { x: area.x, y: area.y, width: area.width, height: 1 };
    let status = Rect { x: area.x, y: area.bottom() - 1, width: area.width, height: 1 };
    let prompt_h = u16::from(app.prompt.is_some());
    let prompt = Rect { x: area.x, y: status.y - prompt_h, width: area.width, height: prompt_h };
    let find_h = match &app.find {
        Some(b) if b.replace_mode => 2,
        Some(_) => 1,
        None => 0,
    }
    .min(area.height.saturating_sub(4 + prompt_h));
    let find_rect = Rect { x: area.x, y: prompt.y - find_h, width: area.width, height: find_h };
    let editor = Rect { x: area.x, y: area.y + 1, width: area.width, height: area.height - 2 - prompt_h - find_h };

    let gutter_w = if app.line_numbers { digits(app.tab.buf.len_lines()).max(3) as u16 + 2 } else { 1 };
    let gutter = Rect { width: gutter_w.min(editor.width / 2), ..editor };
    let scrollbar = Rect { x: editor.right() - 1, width: 1, ..editor };
    let text = Rect {
        x: gutter.right(),
        width: editor.width.saturating_sub(gutter.width + scrollbar.width),
        ..editor
    };
    app.regions.text = text;
    app.regions.gutter = gutter;
    app.regions.scrollbar = scrollbar;
    app.ensure_caret_visible();
    let rows = app.visible_rows(text.height as usize);

    // ---- editor ----
    let base = p.style(t.editor_fg, t.editor_bg);
    buf.set_style(editor, base);
    let sel = app.tab.buf.sel;
    let caret_line = app.tab.buf.line_of(sel.head);
    let sel_style = p.plain_or(p.style(t.selection_fg, t.selection_bg), Modifier::REVERSED);
    let search_style = p.plain_or(p.style(t.search_fg, t.search_bg), Modifier::UNDERLINED);
    let current_line = if p.plain { base } else { p.style(t.editor_fg, t.current_line_bg) };
    let gutter_style = p.plain_or(p.style(t.gutter_fg, t.editor_bg), Modifier::DIM);
    let sub_style = p.plain_or(Style::default().fg(p.c(t.substitute)), Modifier::DIM);

    // Visible search matches while the find bar is open (from its cache).
    let mut matches: Vec<(usize, usize)> = Vec::new();
    if let (Some(bar), Some(first), Some(last)) = (app.find.as_mut(), rows.first(), rows.last()) {
        let all = bar.matches(&app.tab.buf);
        let lo = all.partition_point(|&(_, e)| e <= first.start);
        let hi = all.partition_point(|&(s, _)| s <= last.end);
        matches = all[lo..hi.max(lo)].to_vec();
    }
    // Word/phrase highlights for the visible lines only.
    let groups = app.active_highlights();
    let hl_styles: Vec<Style> = groups.iter().map(|g| highlight_style(&p, &crate::highlight::resolve(g, &t))).collect();
    let mut hl_lines: std::collections::HashMap<usize, (usize, Vec<u16>)> = std::collections::HashMap::new();
    if !groups.is_empty() {
        for row in &rows {
            if hl_lines.contains_key(&row.line) {
                continue;
            }
            let ls = app.tab.buf.line_start(row.line);
            let le = app.tab.buf.line_end(row.line);
            // Very long lines: match only around what is on screen.
            let (ws, we) = if le - ls <= HL_LINE_LIMIT {
                (ls, le)
            } else {
                let last = rows.iter().rev().find(|r| r.line == row.line).map_or(row.end, |r| r.end);
                (row.start.saturating_sub(HL_MARGIN).max(ls), (last + HL_MARGIN).min(le))
            };
            let text: String = app.tab.buf.rope().slice(ws..we).into();
            hl_lines.insert(row.line, (ws, crate::highlight::paint(&groups, &text)));
        }
    }
    // Syntax colours for the visible lines (parsed incrementally; lines
    // that don't fit in the budget are guessed and finished while idle).
    let mut syn_lines: std::collections::HashMap<usize, (usize, crate::syntax::Runs)> = std::collections::HashMap::new();
    if app.syntax {
        app.tab.update_syntax();
        if let (Some(first), Some(last)) = (rows.first(), rows.last())
            && let Some(v) = app.tab.syntax_view(first.line, last.line, SYNTAX_BUDGET)
        {
            for (i, runs) in v.into_iter().enumerate() {
                if let Some(runs) = runs.filter(|r| !r.is_empty()) {
                    let line = first.line + i;
                    syn_lines.insert(line, (app.tab.buf.line_start(line), runs));
                }
            }
        }
    }
    let syn_styles = syntax_styles(&p, &t, &app.syntax_colours, app.adjust.high_contrast);
    let syn_at = |line: usize, pos: usize| -> Option<Style> {
        let (ls, runs) = syn_lines.get(&line)?;
        let r = crate::syntax::run_at(runs, pos.checked_sub(*ls)?)?;
        let mut st = syn_styles[r.kind as usize];
        if r.font & crate::syntax::FONT_BOLD != 0 {
            st = st.add_modifier(Modifier::BOLD);
        }
        if r.font & crate::syntax::FONT_ITALIC != 0 {
            st = st.add_modifier(Modifier::ITALIC);
        }
        if r.font & crate::syntax::FONT_UNDERLINE != 0 {
            st = st.add_modifier(Modifier::UNDERLINED);
        }
        Some(st)
    };
    let hl_at = |line: usize, pos: usize| -> Option<Style> {
        let (ws, marks) = hl_lines.get(&line)?;
        let gi = *marks.get(pos.checked_sub(*ws)?)?;
        hl_styles.get(gi as usize).copied()
    };
    // Markdown tables on screen: pipes and the delimiter row in the border
    // colour, the header row bold.
    let border_style = p.plain_or(Style::default().fg(p.c(t.muted)), Modifier::DIM);
    let mut table_lines: std::collections::HashMap<usize, (crate::table::RowKind, Vec<usize>)> = std::collections::HashMap::new();
    for row in &rows {
        if let std::collections::hash_map::Entry::Vacant(e) = table_lines.entry(row.line)
            && let Some(tb) = app.tab.table_at(row.line)
        {
            match tb.row_of_line(row.line) {
                // A header row is bold only if the table has a header.
                Some(0) if !tb.has_header() => e.insert((crate::table::RowKind::Body, tb.pipes(0))),
                Some(r) if tb.rows[r].kind != crate::table::RowKind::Delimiter => e.insert((tb.rows[r].kind, tb.pipes(r))),
                // Rules (and the Markdown --- row) in the border colour.
                _ => e.insert((crate::table::RowKind::Delimiter, Vec::new())),
            };
        }
    }
    let table_style = |line: usize, pos: usize| -> Option<Style> {
        let (kind, pipes) = table_lines.get(&line)?;
        match kind {
            crate::table::RowKind::Delimiter => Some(border_style),
            _ if pipes.binary_search(&pos).is_ok() => Some(border_style),
            crate::table::RowKind::Header => Some(Style::default().add_modifier(Modifier::BOLD)),
            crate::table::RowKind::Body => None,
        }
    };
    // Selected table cells: their spans are drawn as selected.
    let mut cell_spans: Vec<(usize, usize)> = Vec::new();
    if let Some((tb, cs)) = app.cell_sel_live() {
        for row in &rows {
            if let Some(r) = tb.row_of_line(row.line)
                && row.row_in_line == 0
            {
                for (c, cell) in tb.rows[r].cells.iter().enumerate() {
                    if cs.contains(r, c) {
                        cell_spans.push((cell.span.start, cell.span.end));
                    }
                }
            }
        }
    }
    let in_cell_sel = |s: usize, e: usize| cell_spans.iter().any(|&(a, b)| s >= a && e <= b);
    let in_match = |s: usize, e: usize| {
        let i = matches.partition_point(|&(_, me)| me <= s);
        matches.get(i).is_some_and(|&(ms, me)| s >= ms && e <= me)
    };

    // Each row's sideways scroll: the whole view without soft wrap; with
    // it, only the caret's (unwrapped) table.
    let sxs: Vec<usize> = rows.iter().map(|r| app.row_scroll_x(r.line)).collect();
    for (i, row) in rows.iter().enumerate() {
        let scroll_x = sxs[i];
        let y = text.y + i as u16;
        let is_caret_line = row.line == caret_line && sel.is_empty();
        if is_caret_line {
            buf.set_style(Rect { x: text.x, y, width: text.width, height: 1 }, current_line);
        }
        // Gutter: line number on a line's first row, a wrap marker on others.
        buf.set_style(Rect { x: gutter.x, y, width: gutter.width, height: 1 }, gutter_style);
        if app.line_numbers {
            let label = if row.row_in_line == 0 { format!("{:>w$} ", row.line + 1, w = gutter.width as usize - 2) } else { format!("{:>w$} ", "↪", w = gutter.width as usize - 2) };
            let st = if row.line == caret_line { gutter_style.fg(p.c(t.editor_fg)) } else { gutter_style };
            buf.set_stringn(gutter.x, y, label, gutter.width as usize, st);
        }
        for g in &row.glyphs {
            if g.col + g.width <= scroll_x {
                continue;
            }
            let rel = g.col as isize - scroll_x as isize;
            if rel as usize + g.width > text.width as usize {
                // Partially visible wide glyph at the right edge: blank it.
                if rel >= 0 && (rel as usize) < text.width as usize {
                    put(buf, text.x + rel as u16, y, " ", 1, base);
                }
                break;
            }
            let mut st = if is_caret_line { current_line } else { base };
            if let Some(ss) = syn_at(row.line, g.start) {
                st = st.patch(ss);
            }
            if let Some(ts) = table_style(row.line, g.start) {
                st = st.patch(ts);
            }
            if let Some(h) = hl_at(row.line, g.start) {
                st = st.patch(h);
            }
            if g.kind == GlyphKind::Substitute {
                st = st.patch(sub_style);
            }
            if in_match(g.start, g.end) {
                st = search_style;
            }
            if (g.start >= sel.start() && g.end <= sel.end() && !sel.is_empty()) || in_cell_sel(g.start, g.end) {
                st = sel_style;
            }
            if rel < 0 {
                // Wide glyph cut by the left edge.
                let visible = (g.width as isize + rel) as usize;
                for k in 0..visible {
                    put(buf, text.x + k as u16, y, " ", 1, st);
                }
                continue;
            }
            let x = text.x + rel as u16;
            let ws_style = if p.plain { st.add_modifier(Modifier::DIM) } else { st.fg(p.c(t.muted)) };
            if g.kind == GlyphKind::Tab {
                for k in 0..g.width {
                    let (sym, s2) = if app.show_whitespace && k == 0 { ("→", ws_style) } else { (" ", st) };
                    put(buf, x + k as u16, y, sym, 1, s2);
                }
            } else if app.show_whitespace && g.symbol == " " {
                put(buf, x, y, "·", 1, ws_style);
            } else {
                put(buf, x, y, &g.symbol, g.width, st);
            }
        }
        // Show a selected line break as one highlighted cell.
        if row.is_last && !sel.is_empty() && row.end >= sel.start() && row.end < sel.end() {
            let rel = row.width() as isize - scroll_x as isize;
            if rel >= 0 && (rel as u16) < text.width {
                put(buf, text.x + rel as u16, y, " ", 1, sel_style);
            }
        }
        // A table row cut by the window edge (soft wrap on): ‹ / › markers.
        if app.wrap && table_lines.contains_key(&row.line) && text.width > 2 {
            let mark = base.patch(border_style).add_modifier(Modifier::BOLD);
            if scroll_x > 0 {
                put(buf, text.x, y, "‹", 1, mark);
            }
            if row.width() > scroll_x + text.width as usize {
                put(buf, text.right() - 1, y, "›", 1, mark);
            }
        }
    }
    // Rows past end of document.
    for i in rows.len()..text.height as usize {
        let y = text.y + i as u16;
        buf.set_stringn(gutter.x, y, format!("{:>w$}", "~", w = gutter.width.saturating_sub(2) as usize), gutter.width as usize, gutter_style);
    }

    // Caret (hardware cursor so Kitty draws it with its own cursor colours).
    let mut caret_pos = None;
    if app.prompt.is_none()
        && let Some((i, row)) = rows.iter().enumerate().find(|(_, r)| r.contains_caret(sel.head))
    {
        let col = row.col_of(sel.head) as isize - sxs[i] as isize;
        if col >= 0 && col <= text.width as isize {
            caret_pos = Some(Position { x: (text.x + col as u16).min(text.right().saturating_sub(1)), y: text.y + i as u16 });
        }
    }
    let doc_caret = caret_pos;

    draw_scrollbar(buf, app, scrollbar, rows.len(), &p);
    app.rendered_rows = rows;
    app.rendered_scroll = sxs;

    // ---- tab bar ----
    let surface = p.plain_or(p.style(t.surface_fg, t.surface_bg), Modifier::REVERSED);
    let accent = p.plain_or(p.style(t.accent_fg, t.accent), Modifier::REVERSED | Modifier::BOLD);
    draw_tab_bar(buf, app, title, surface, accent, &p);

    // ---- find bar ----
    app.regions.find_hits.clear();
    if app.find.is_some() && find_h > 0 {
        let field = p.plain_or(p.style(t.editor_fg, t.editor_bg), Modifier::UNDERLINED);
        if let Some(pos) = draw_find_bar(buf, app, find_rect, surface, accent, field) {
            caret_pos = Some(pos);
        }
    }

    // ---- prompt ----
    if let Some(pr) = &app.prompt {
        buf.set_style(prompt, surface);
        let hint = "  Enter confirm · Esc cancel";
        let label = format!(" {}: ", pr.kind.label());
        let lw = label.chars().count() as u16;
        buf.set_stringn(prompt.x, prompt.y, &label, prompt.width as usize, accent);
        let input_w = prompt.width.saturating_sub(lw + 1) as usize;
        let fx = prompt.x + lw;
        let v = pr.input.view(input_w);
        let (end, _) = buf.set_stringn(fx, prompt.y, v.shown, input_w, surface);
        buf.set_stringn(end + 1, prompt.y, hint, prompt.right().saturating_sub(end + 1) as usize, surface.add_modifier(Modifier::DIM));
        caret_pos = Some(Position { x: (fx + v.caret).min(prompt.right() - 1), y: prompt.y });
        app.regions.prompt_field = Rect { x: fx, y: prompt.y, width: prompt.right().saturating_sub(fx), height: 1 };
    } else {
        app.regions.prompt_field = Rect::default();
    }

    // ---- status bar ----
    buf.set_style(status, surface);
    let msg = app.status.as_ref().filter(|(_, lvl, at)| {
        at.elapsed() < if *lvl == Level::Warn { Duration::from_secs(10) } else { Duration::from_secs(5) }
    });
    let mut right = status_right(app, false);
    // A message takes priority: drop the extra fields if it would not fit.
    if let Some((m, _, _)) = msg
        && m.chars().count() + 4 + right.chars().count() > status.width as usize
    {
        right = status_right(app, true);
    }
    let rw = right.chars().count() as u16;
    let left_w = status.width.saturating_sub(rw + 1) as usize;
    match msg {
        Some((m, Level::Warn, _)) => {
            let st = p.plain_or(surface.fg(p.c(t.warning)).add_modifier(Modifier::BOLD), Modifier::BOLD);
            buf.set_stringn(status.x, status.y, format!(" ⚠ {m}"), left_w, st);
        }
        Some((m, Level::Info, _)) => {
            buf.set_stringn(status.x, status.y, format!(" {m}"), left_w, surface);
        }
        None => {
            let path = app.tab.doc.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "Untitled (Ctrl+S to choose a name)".into());
            buf.set_stringn(status.x, status.y, format!(" {path}"), left_w, surface.add_modifier(Modifier::DIM));
        }
    }
    let rx = status.right().saturating_sub(rw);
    buf.set_stringn(rx, status.y, &right, rw as usize, surface);
    let enc = format!(" {} ", encoding_label(&app.tab.doc));
    app.regions.status_encoding = match right.find(&enc) {
        Some(i) => Rect { x: rx + right[..i].chars().count() as u16, y: status.y, width: enc.chars().count() as u16, height: 1 },
        None => Rect::default(),
    };
    let lang = format!(" {} ", app.tab.syntax_label());
    app.regions.status_syntax = match right.find(&lang) {
        Some(i) if app.syntax && (app.tab.syntax.is_some() || app.tab.syntax_manual) => Rect { x: rx + right[..i].chars().count() as u16, y: status.y, width: lang.chars().count() as u16, height: 1 },
        _ => Rect::default(),
    };
    let ovr = format!(" {} ", if app.overwrite { "OVR" } else { "INS" });
    app.regions.status_overwrite = match right.find(&ovr) {
        Some(i) => Rect { x: rx + right[..i].chars().count() as u16, y: status.y, width: 5, height: 1 },
        None => Rect::default(),
    };

    if app.show_help {
        draw_help(buf, app, area, &p);
        caret_pos = None;
    }
    app.regions.prefs_hits.clear();
    if app.prefs.is_some() {
        draw_prefs(buf, app, area, &p);
        if app.prompt.is_none() {
            caret_pos = None;
        }
    }
    if app.picker.is_some() {
        draw_picker(buf, app, area, &p);
        if app.dialog.is_none() {
            caret_pos = Some(Position { x: app.regions.picker_field.x, y: app.regions.picker_field.y });
            if let Some(pk) = &app.picker {
                let v = pk.filter.view(app.regions.picker_field.width as usize);
                caret_pos = Some(Position { x: app.regions.picker_field.x + v.caret, y: app.regions.picker_field.y });
            }
        }
    } else {
        app.regions.picker_box = Rect::default();
        app.regions.picker_field = Rect::default();
        app.regions.picker_list = Rect::default();
    }
    app.regions.dialog_buttons.clear();
    if app.dialog.is_some() {
        draw_dialog(buf, app, area, &p);
        caret_pos = None;
    }

    if let Some(pos) = caret_pos {
        f.set_cursor_position(pos);
    }
    app.caret_block = app.overwrite && caret_pos.is_some() && caret_pos == doc_caret;
}

fn draw_tab_bar(buf: &mut TermBuf, app: &mut App, title: Rect, surface: Style, accent: Style, p: &Painter) {
    buf.set_style(title, surface);
    let buttons: [(&str, Button, bool); 6] = [
        (" Prefs ", Button::Prefs, app.prefs.is_some()),
        (" Save ", Button::Save, app.tab.buf.is_dirty()),
        (" Table ", Button::Table, app.caret_table().is_some()),
        (" Wrap ", Button::Wrap, app.wrap),
        (" Mouse ", Button::Mouse, app.mouse_enabled),
        (" Help F1 ", Button::Help, false),
    ];
    // " name ● " / " name × " per tab; ● = unsaved (click to close, like VS Code).
    let n = app.tab_count();
    let labels: Vec<(String, bool)> = (0..n)
        .map(|i| {
            let tab = app.tab_at(i);
            (fit_middle(&tab.doc.display_name(), 28), tab.buf.is_dirty())
        })
        .collect();
    let width = |l: &(String, bool)| l.0.chars().count() as u16 + 4;
    let app_name = format!(" Plica {VERSION} │");
    let all: u16 = labels.iter().map(width).sum();

    // Tabs have priority: the buttons (all reachable by keys too) show only
    // when every tab still fits beside them.
    let total: u16 = buttons.iter().map(|b| b.0.len() as u16 + 1).sum();
    let bx = title.right().saturating_sub(total);
    let show_buttons = bx > title.x + 20 && title.x + all < bx;
    let bar_end = if show_buttons { bx - 1 } else { title.right() };
    let bar = Rect { width: bar_end - title.x, ..title };
    app.regions.tab_bar = bar;
    app.regions.tab_hits.clear();
    app.regions.buttons.clear();
    let mut x = bar.x;
    if all + (app_name.chars().count() as u16) < bar.width {
        buf.set_stringn(x, bar.y, &app_name, bar.width as usize, surface.add_modifier(Modifier::DIM));
        x += app_name.chars().count() as u16;
    }
    // Scroll so the active tab is visible, with ‹ › markers for hidden tabs.
    let active = app.active;
    let avail = bar.right().saturating_sub(x);
    let mut start = 0;
    while start < active {
        let need: u16 = labels[start..=active].iter().map(width).sum::<u16>()
            + u16::from(start > 0)
            + u16::from(active + 1 < n);
        if need <= avail {
            break;
        }
        start += 1;
    }
    if start > 0 {
        buf.set_stringn(x, bar.y, "‹", 1, surface.add_modifier(Modifier::BOLD));
        app.regions.tab_hits.push((Rect { x, y: bar.y, width: 1, height: 1 }, TabHit::Prev));
        x += 1;
    }
    let warn = if p.plain { surface } else { surface.fg(p.c(app.theme.warning)) };
    for (i, label) in labels.iter().enumerate().skip(start) {
        let w = width(label);
        let more_after = i + 1 < n;
        if x + w + u16::from(more_after) > bar.right() && i != active {
            buf.set_stringn(x, bar.y, "›", 1, surface.add_modifier(Modifier::BOLD));
            app.regions.tab_hits.push((Rect { x, y: bar.y, width: 1, height: 1 }, TabHit::Next));
            break;
        }
        let st = if i == active { accent } else { surface };
        let (marker, mst) = if label.1 { ("●", if i == active { st } else { warn }) } else { ("×", st) };
        buf.set_stringn(x, bar.y, format!(" {} ", label.0), bar.right().saturating_sub(x) as usize, st);
        let mx = x + w - 2;
        if mx < bar.right() {
            buf.set_stringn(mx, bar.y, marker, 1, mst);
            buf.set_stringn(mx + 1, bar.y, " ", 1, st);
            app.regions.tab_hits.push((Rect { x: mx, y: bar.y, width: 1, height: 1 }, TabHit::Close(i)));
        }
        app.regions.tab_hits.push((Rect { x, y: bar.y, width: w.min(bar.right() - x), height: 1 }, TabHit::Select(i)));
        x += w;
        if x >= bar.right() {
            break;
        }
    }

    if show_buttons {
        let mut x = bx;
        for (label, b, on) in buttons {
            let w = label.len() as u16;
            let st = if on { accent } else { surface.add_modifier(Modifier::BOLD) };
            buf.set_stringn(x, title.y, label, w as usize, st);
            app.regions.buttons.push((Rect { x, y: title.y, width: w, height: 1 }, b));
            x += w + 1;
        }
    }
}

/// Syntax parsing per frame; the rest of the view is guessed, then
/// finished between events.
const SYNTAX_BUDGET: Duration = Duration::from_millis(12);

/// The style of each syntax token kind (indexed by `Kind as usize`).
/// Colours are contrast-repaired against the editor background; without
/// colours, comments are dim and keywords and headings bold.
fn syntax_styles(p: &Painter, t: &crate::theme::Theme, overrides: &[(crate::syntax::Kind, crate::highlight::Colour)], high_contrast: bool) -> Vec<Style> {
    use crate::syntax::Kind;
    let mut v = vec![Style::default(); Kind::ALL.len() + 1];
    for k in Kind::ALL {
        let mut st = Style::default();
        if p.plain {
            st = match k {
                Kind::Comment | Kind::Quote => st.add_modifier(Modifier::DIM),
                Kind::Keyword | Kind::Heading => st.add_modifier(Modifier::BOLD),
                _ => st,
            };
        } else {
            let colour = overrides.iter().rev().find(|(o, _)| *o == k).map(|(_, c)| *c).or(k.default_colour());
            if let Some(c) = colour {
                st = st.fg(p.c(crate::theme::ensure_contrast(c.resolve(t), t.editor_bg, k.min_contrast(high_contrast))));
            }
            if k.italic() {
                st = st.add_modifier(Modifier::ITALIC);
            }
            if k.bold() {
                st = st.add_modifier(Modifier::BOLD);
            }
        }
        v[k as usize] = st;
    }
    v
}

/// Lines longer than this are highlighted only near the visible part.
const HL_LINE_LIMIT: usize = 20_000;
const HL_MARGIN: usize = 512;

fn highlight_style(p: &Painter, r: &crate::highlight::Resolved) -> Style {
    let mut m = Modifier::empty();
    if r.bold {
        m |= Modifier::BOLD;
    }
    if r.italic {
        m |= Modifier::ITALIC;
    }
    if r.underline {
        m |= Modifier::UNDERLINED;
    }
    if p.plain {
        // No colours: a highlight must still show.
        return Style::default().add_modifier(if m.is_empty() { Modifier::BOLD } else { m });
    }
    let mut s = Style::default().add_modifier(m);
    if let Some(fg) = r.fg {
        s = s.fg(p.c(fg));
    }
    if let Some(bg) = r.bg {
        s = s.bg(p.c(bg));
    }
    s
}

fn put(buf: &mut TermBuf, x: u16, y: u16, sym: &str, width: usize, st: Style) {
    if let Some(c) = buf.cell_mut((x, y)) {
        c.set_symbol(sym).set_style(st);
    }
    // Cells covered by a wide glyph must be reset so the diff skips them.
    for k in 1..width {
        if let Some(c) = buf.cell_mut((x + k as u16, y)) {
            c.reset();
            c.set_style(st);
        }
    }
}

/// Find bar (one row; two in replace mode). Returns the caret position for
/// the focused field.
fn draw_find_bar(buf: &mut TermBuf, app: &mut App, r: Rect, surface: Style, accent: Style, field: Style) -> Option<Position> {
    let status = app.find_status();
    let bar = app.find.as_ref()?;
    buf.set_style(r, surface);
    let mut hits: Vec<(Rect, FindHit)> = Vec::new();
    let label_w = 10u16; // " Replace: "
    let chip = |on: bool| if on { accent } else { surface.add_modifier(Modifier::DIM) };

    // Right side of row 1: toggles, prev/next, count, close.
    let chips: [(&str, FindHit, bool); 3] = [
        (" Aa ", FindHit::MatchCase, bar.opts.match_case),
        (" W ", FindHit::WholeWord, bar.opts.whole_word),
        (" Sel ", FindHit::InSelection, bar.scope.is_some()),
    ];
    let count = format!(" {status} ");
    let right_w: u16 = chips.iter().map(|c| c.0.len() as u16 + 1).sum::<u16>() + 6 + count.chars().count() as u16 + 3;
    let field_w = r.width.saturating_sub(label_w + right_w + 1).max(8);
    let y = r.y;
    let mut caret = None;

    let mut draw_field = |buf: &mut TermBuf, y: u16, label: &str, text: &LineEdit, focused: bool, hit: FindHit| {
        buf.set_stringn(r.x, y, label, label_w as usize, if focused { accent } else { surface });
        let fx = r.x + label_w;
        buf.set_style(Rect { x: fx, y, width: field_w, height: 1 }, field);
        let v = text.view(field_w as usize);
        buf.set_stringn(fx, y, v.shown, field_w as usize, field);
        hits.push((Rect { x: fx, y, width: field_w, height: 1 }, hit));
        if focused {
            caret = Some(Position { x: (fx + v.caret).min(fx + field_w - 1), y });
        }
    };
    let focus_find = bar.focus == crate::find::Field::Find;
    draw_field(buf, y, " Find:    ", &bar.query, focus_find, FindHit::FindField);
    if bar.replace_mode && r.height > 1 {
        draw_field(buf, y + 1, " Replace: ", &bar.replacement, !focus_find, FindHit::ReplaceField);
    }

    let mut x = r.x + label_w + field_w + 1;
    for (label, hit, on) in chips {
        let w = label.len() as u16;
        buf.set_stringn(x, y, label, w as usize, chip(on));
        hits.push((Rect { x, y, width: w, height: 1 }, hit));
        x += w + 1;
    }
    for (label, hit) in [(" ↑ ", FindHit::Prev), (" ↓ ", FindHit::Next)] {
        buf.set_stringn(x, y, label, 3, surface.add_modifier(Modifier::BOLD));
        hits.push((Rect { x, y, width: 3, height: 1 }, hit));
        x += 3;
    }
    let (cx, _) = buf.set_stringn(x, y, &count, r.right().saturating_sub(x) as usize, surface);
    let close_x = r.right().saturating_sub(3).max(cx);
    buf.set_stringn(close_x, y, " × ", 3, surface.add_modifier(Modifier::BOLD));
    hits.push((Rect { x: close_x, y, width: 3, height: 1 }, FindHit::Close));

    if bar.replace_mode && r.height > 1 {
        let mut x = r.x + label_w + field_w + 1;
        for (label, hit) in [(" Replace ", FindHit::ReplaceOne), (" All ", FindHit::ReplaceAll)] {
            let w = label.len() as u16;
            buf.set_stringn(x, y + 1, label, w as usize, surface.add_modifier(Modifier::BOLD | Modifier::REVERSED));
            hits.push((Rect { x, y: y + 1, width: w, height: 1 }, hit));
            x += w + 1;
        }
        buf.set_stringn(x + 1, y + 1, "Enter: replace · Alt+A: all", r.right().saturating_sub(x + 1) as usize, surface.add_modifier(Modifier::DIM));
    }
    app.regions.find_hits = hits;
    caret
}

/// The status bar's encoding field: "?" marks a guess, "+raw" kept bytes,
/// "⚠" an inexact decode.
pub fn encoding_label(d: &crate::document::Document) -> String {
    let mut s = d.encoding.label();
    if d.how == crate::encoding::How::Guess {
        s.push('?');
    }
    if d.encoding.raw {
        s.push_str("+raw");
    }
    if d.inexact {
        s.push_str(" ⚠");
    }
    s
}

fn status_right(app: &App, compact: bool) -> String {
    let head = app.tab.buf.sel.head;
    let line = app.tab.buf.line_of(head);
    let line_start = app.tab.buf.line_start(line);
    // Column = graphemes from line start, 1-based.
    let mut col = 1;
    let mut p = line_start;
    while p < head {
        p = app.tab.buf.next_grapheme(p);
        col += 1;
    }
    let mut parts = vec![format!("Ln {}, Col {}", line + 1, col)];
    if compact {
        // Overwrite mode stays visible: it changes what typing does.
        return if app.overwrite { format!(" {} │ OVR ", parts[0]) } else { format!(" {} ", parts[0]) };
    }
    parts.push(if app.overwrite { "OVR" } else { "INS" }.into());
    if let Some((_, cs)) = app.cell_sel_live() {
        let (r0, c0, r1, c1) = cs.rect();
        let rows = (r0..=r1).filter(|&r| r != 1).count();
        parts.push(format!("{rows}×{} cells", c1 - c0 + 1));
    } else if !app.tab.buf.sel.is_empty() {
        parts.push(format!("Sel {}", app.tab.buf.sel.end() - app.tab.buf.sel.start()));
    }
    if let Some(tb) = app.caret_table() {
        let kind = match tb.style {
            crate::table::Style::Markdown => "Table",
            crate::table::Style::Ascii => "ASCII table",
            crate::table::Style::Unicode => "Box table",
        };
        parts.push(if app.table_keys_live().is_some() { kind.into() } else { format!("{kind} (keys off)") });
    }
    // The language, when there is one (plain text needs no label; the
    // language menu is on a key too).
    if app.syntax && (app.tab.syntax.is_some() || app.tab.syntax_manual) {
        parts.push(app.tab.syntax_label());
    }
    parts.push(encoding_label(&app.tab.doc));
    let le = app.tab.doc.line_ending.label();
    parts.push(if app.tab.doc.mixed_line_endings { format!("{le}*") } else { le.into() });
    parts.push(match &app.theme_source {
        ThemeSource::Builtin(b) => b.title().into(),
        ThemeSource::TerminalDefault => "no colour".into(),
        ThemeSource::File { path, last_error } => {
            let n = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if last_error.is_some() { format!("⚠ {n}") } else { n }
        }
    });
    format!(" {} ", parts.join(" │ "))
}

fn draw_scrollbar(buf: &mut TermBuf, app: &App, r: Rect, _rows: usize, p: &Painter) {
    let t = &app.theme;
    let track = p.plain_or(p.style(t.muted, t.editor_bg), Modifier::DIM);
    let thumb = p.plain_or(p.style(t.muted, t.muted), Modifier::REVERSED);
    let lines = app.tab.buf.len_lines().max(1);
    let h = r.height as usize;
    for i in 0..h {
        put(buf, r.x, r.y + i as u16, "│", 1, track);
    }
    if lines <= 1 && h > 0 {
        return;
    }
    let thumb_h = (h * h / (lines + h)).clamp(1, h);
    let pos = (app.tab.top_line * (h - thumb_h)) / lines.saturating_sub(1).max(1);
    for i in pos..(pos + thumb_h).min(h) {
        put(buf, r.x, r.y + i as u16, " ", 1, thumb);
    }
}

fn draw_help(buf: &mut TermBuf, app: &App, area: Rect, p: &Painter) {
    let t = &app.theme;
    let caps = &app.caps;
    // Command keys come from the live keymap, so remapping shows here.
    use crate::keymap::Command as C;
    let km = &app.keymap;
    let pair = |a: C, b: C| {
        format!("{:>17}  {:<20}{:>17}  {}", km.label(a), a.describe(), km.label(b), b.describe())
    };
    let mut lines: Vec<String> = vec![format!("Plica {VERSION} — keys (rebind in config.toml, see plica --print-config)"), String::new()];
    for (a, b) in [
        (C::Save, C::SaveAs),
        (C::Open, C::NewTab),
        (C::CloseTab, C::Quit),
        (C::Undo, C::Redo),
        (C::Copy, C::Cut),
        (C::Paste, C::SelectAll),
        (C::Find, C::Replace),
        (C::FindNext, C::FindPrev),
        (C::GotoLine, C::Help),
        (C::NextTab, C::PrevTab),
        (C::ToggleWrap, C::ToggleLineNumbers),
        (C::ToggleWhitespace, C::ToggleMouse),
        (C::ScrollUp, C::ScrollDown),
        (C::Preferences, C::HighlightWord),
        (C::Encoding, C::TableMenu),
        (C::ToggleOverwrite, C::FormatTable),
    ] {
        lines.push(pair(a, b));
    }
    lines.extend([
        String::new(),
        "Fixed: Alt+1..9 go to tab · Tab/Shift+Tab indent · Ctrl/Alt+←/→ word · Esc clears selection".into(),
        "Find bar: Alt+C case · Alt+W whole word · Alt+L in selection · Alt+A replace all".into(),
        "Tables: Tab/Shift+Tab next/previous cell · Enter new row · Esc plain keys in this table".into(),
        "Mouse: click · drag · double/triple-click · Shift/Alt+click extend · middle-click pastes selection".into(),
        "       wheel (Shift: sideways)".into(),
        "       line numbers select lines · tabs: click, ×, middle-click, wheel · drop files to open".into(),
        String::new(),
        if std::env::var_os("TMUX").is_some() {
            "tmux: needs `set -g mouse on` and `set -g set-clipboard on` for mouse and clipboard".into()
        } else {
            String::new()
        },
        format!(
            "Terminal: keyboard protocol {} · {}",
            if caps.keyboard_enhanced { "on" } else { "off (some Ctrl+Shift keys unavailable)" },
            if caps.truecolor { "truecolour" } else { "256 colours" }
        ),
        "Press any key or click to close".into(),
    ]);
    let w = (lines.iter().map(|l| l.chars().count()).max().unwrap_or(10) + 4).min(area.width as usize) as u16;
    let h = (lines.len() + 2).min(area.height as usize) as u16;
    let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    let st = p.plain_or(p.style(t.surface_fg, t.surface_bg), Modifier::REVERSED);
    let border = p.plain_or(p.style(t.accent, t.surface_bg), Modifier::REVERSED);
    for y in r.y..r.bottom() {
        for x in r.x..r.right() {
            put(buf, x, y, " ", 1, st);
        }
    }
    for x in r.x..r.right() {
        put(buf, x, r.y, "─", 1, border);
        put(buf, x, r.bottom() - 1, "─", 1, border);
    }
    for y in r.y..r.bottom() {
        put(buf, r.x, y, "│", 1, border);
        put(buf, r.right() - 1, y, "│", 1, border);
    }
    put(buf, r.x, r.y, "╭", 1, border);
    put(buf, r.right() - 1, r.y, "╮", 1, border);
    put(buf, r.x, r.bottom() - 1, "╰", 1, border);
    put(buf, r.right() - 1, r.bottom() - 1, "╯", 1, border);
    for (i, l) in lines.iter().enumerate().take(h.saturating_sub(2) as usize) {
        let s = if i == 0 { st.add_modifier(Modifier::BOLD) } else { st };
        buf.set_stringn(r.x + 2, r.y + 1 + i as u16, l, w.saturating_sub(4) as usize, s);
    }
}

/// Shorten `s` to at most `w` chars with a middle ellipsis.
fn fit_middle(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        return s.to_string();
    }
    if w <= 1 {
        return "…".chars().take(w).collect();
    }
    let tail = (w - 1) / 2;
    let head = w - 1 - tail;
    let start: String = s.chars().take(head).collect();
    let end: String = s.chars().skip(n - tail).collect();
    format!("{start}…{end}")
}

/// Rounded box with a title in the top border; returns the inner rect.
fn draw_box(buf: &mut TermBuf, r: Rect, title: &str, st: Style, border: Style) -> Rect {
    for y in r.y..r.bottom() {
        for x in r.x..r.right() {
            put(buf, x, y, " ", 1, st);
        }
    }
    for x in r.x..r.right() {
        put(buf, x, r.y, "─", 1, border);
        put(buf, x, r.bottom() - 1, "─", 1, border);
    }
    for y in r.y..r.bottom() {
        put(buf, r.x, y, "│", 1, border);
        put(buf, r.right() - 1, y, "│", 1, border);
    }
    put(buf, r.x, r.y, "╭", 1, border);
    put(buf, r.right() - 1, r.y, "╮", 1, border);
    put(buf, r.x, r.bottom() - 1, "╰", 1, border);
    put(buf, r.right() - 1, r.bottom() - 1, "╯", 1, border);
    if !title.is_empty() {
        buf.set_stringn(r.x + 2, r.y, format!(" {title} "), r.width.saturating_sub(4) as usize, border.add_modifier(Modifier::BOLD));
    }
    Rect { x: r.x + 2, y: r.y + 1, width: r.width.saturating_sub(4), height: r.height.saturating_sub(2) }
}

fn draw_dialog(buf: &mut TermBuf, app: &mut App, area: Rect, p: &Painter) {
    let t = &app.theme;
    let Some(d) = &app.dialog else { return };
    let st = p.plain_or(p.style(t.surface_fg, t.surface_bg), Modifier::REVERSED);
    let border = p.plain_or(p.style(t.accent, t.surface_bg), Modifier::REVERSED);
    let focus = p.plain_or(p.style(t.accent_fg, t.accent), Modifier::BOLD | Modifier::UNDERLINED);
    let labels: Vec<String> = d.buttons.iter().map(|b| format!(" {} ", b.label)).collect();
    let buttons_w: usize = labels.iter().map(|l| l.chars().count() + 2).sum();
    let text_w = d.lines.iter().map(|l| l.chars().count()).max().unwrap_or(0).max(d.title.chars().count() + 4);
    let w = (text_w.max(buttons_w) + 6).min(area.width as usize) as u16;
    let h = (d.lines.len() + 4).min(area.height as usize) as u16;
    let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    let inner = draw_box(buf, r, &d.title, st, border);
    for (i, line) in d.lines.iter().enumerate() {
        buf.set_stringn(inner.x, inner.y + i as u16, line, inner.width as usize, st);
    }
    // Buttons, right-aligned on the last inner row.
    let by = inner.bottom() - 1;
    let mut x = inner.right().saturating_sub(buttons_w as u16).max(inner.x);
    let mut hits = Vec::new();
    for (i, (b, label)) in d.buttons.iter().zip(&labels).enumerate() {
        let bw = label.chars().count() as u16;
        let bst = if i == d.focus { focus } else { st.add_modifier(Modifier::BOLD) };
        buf.set_stringn(x, by, label, bw as usize, bst);
        if let Some(m) = b.mnemonic {
            // +1 for the leading space in the label.
            let mx = x + 1 + b.label[..m].chars().count() as u16;
            if let Some(c) = buf.cell_mut((mx, by)) {
                c.modifier.insert(Modifier::UNDERLINED);
            }
        }
        hits.push((Rect { x, y: by, width: bw, height: 1 }, i));
        x += bw + 2;
    }
    app.regions.dialog_buttons = hits;
}

fn draw_picker(buf: &mut TermBuf, app: &mut App, area: Rect, p: &Painter) {
    let t = &app.theme;
    let Some(pk) = &app.picker else { return };
    let st = p.plain_or(p.style(t.surface_fg, t.surface_bg), Modifier::REVERSED);
    let border = p.plain_or(p.style(t.accent, t.surface_bg), Modifier::REVERSED);
    let sel = p.plain_or(p.style(t.accent_fg, t.accent), Modifier::BOLD | Modifier::UNDERLINED);
    let field = p.plain_or(p.style(t.editor_fg, t.editor_bg), Modifier::UNDERLINED);
    let dim = st.add_modifier(Modifier::DIM);
    let visible = pk.visible();
    let label_w = pk.items.iter().map(|i| i.label.chars().count()).max().unwrap_or(0);
    let detail_w = pk.items.iter().map(|i| i.detail.chars().count()).max().unwrap_or(0);
    let w = ((label_w + detail_w + 9).max(44) as u16).min(area.width.saturating_sub(2)).max(10);
    let h = ((pk.items.len() + 5) as u16).min(area.height.saturating_sub(2)).max(6);
    let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    let inner = draw_box(buf, r, &pk.title, st, border);
    if inner.height < 3 {
        return;
    }
    // Filter field on the first row, hint on the last, list in between.
    let fl = " Filter: ";
    buf.set_stringn(inner.x, inner.y, fl, inner.width as usize, st);
    let fx = inner.x + fl.len() as u16;
    let fw = inner.right().saturating_sub(fx + 1);
    buf.set_style(Rect { x: fx, y: inner.y, width: fw, height: 1 }, field);
    let v = pk.filter.view(fw as usize);
    buf.set_stringn(fx, inner.y, v.shown, fw as usize, field);
    let list = Rect { x: inner.x, y: inner.y + 1, width: inner.width, height: inner.height - 2 };
    let top = pk.scroll_for(list.height as usize);
    for (row, &i) in visible.iter().enumerate().skip(top).take(list.height as usize) {
        let y = list.y + (row - top) as u16;
        let it = &pk.items[i];
        let style = if row == pk.sel { sel } else { st };
        buf.set_style(Rect { x: list.x, y, width: list.width, height: 1 }, style);
        let mark = if it.current { "●" } else { " " };
        let (x, _) = buf.set_stringn(list.x, y, format!(" {mark} {:<label_w$}  ", it.label), list.width as usize, style);
        let dstyle = if row == pk.sel { style } else { dim };
        buf.set_stringn(x, y, &it.detail, list.right().saturating_sub(x) as usize, dstyle);
    }
    if visible.is_empty() {
        buf.set_stringn(list.x + 1, list.y, "(nothing matches)", list.width.saturating_sub(1) as usize, dim);
    }
    let hint = format!(" ↑↓ · Enter or click: choose · Esc · {}/{}", visible.len(), pk.items.len());
    buf.set_stringn(inner.x, inner.bottom() - 1, hint, inner.width as usize, dim);
    app.regions.picker_box = r;
    app.regions.picker_field = Rect { x: fx, y: inner.y, width: fw, height: 1 };
    app.regions.picker_list = list;
}

fn draw_prefs(buf: &mut TermBuf, app: &mut App, area: Rect, p: &Painter) {
    use crate::prefs::{PrefsHit, Section};
    let t = app.theme.clone();
    let settings = app.current_settings();
    let Some(state) = &app.prefs else { return };
    let st = p.plain_or(p.style(t.surface_fg, t.surface_bg), Modifier::REVERSED);
    let border = p.plain_or(p.style(t.accent, t.surface_bg), Modifier::REVERSED);
    let sel = p.plain_or(p.style(t.accent_fg, t.accent), Modifier::BOLD | Modifier::UNDERLINED);
    let dim = st.add_modifier(Modifier::DIM);
    let w = area.width.saturating_sub(4).min(84);
    let h = area.height.saturating_sub(2).min(20);
    if w < 40 || h < 10 {
        return;
    }
    let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    let inner = draw_box(buf, r, "Preferences", st, border);
    let mut hits = Vec::new();

    // Sections.
    let left_w = 20u16;
    for (i, sec) in Section::ALL.iter().enumerate() {
        let y = inner.y + i as u16;
        if y >= inner.bottom() - 2 {
            break;
        }
        let style = if i == state.section { sel } else { st };
        buf.set_stringn(inner.x, y, format!(" {:<18}", sec.title()), left_w as usize, style);
        hits.push((Rect { x: inner.x, y, width: left_w, height: 1 }, PrefsHit::Section(i)));
    }
    for y in inner.y..inner.bottom() - 2 {
        put(buf, inner.x + left_w, y, "│", 1, dim);
    }

    // Rows of the current section.
    let rx = inner.x + left_w + 2;
    let rw = inner.right().saturating_sub(rx);
    let section = Section::ALL[state.section];
    match section {
        Section::Keys => {
            let label = " Edit config.toml ";
            let bw = label.len() as u16;
            buf.set_stringn(rx, inner.y, label, rw as usize, st.add_modifier(Modifier::BOLD | Modifier::REVERSED));
            hits.push((Rect { x: rx, y: inner.y, width: bw.min(rw), height: 1 }, PrefsHit::EditConfig));
            buf.set_stringn(rx + bw + 1, inner.y, "(Enter) to rebind keys under [keys]", rw.saturating_sub(bw + 1) as usize, dim);
            let visible = (inner.height.saturating_sub(4)) as usize;
            for (i, cmd) in crate::keymap::Command::ALL.iter().skip(state.keys_scroll).take(visible).enumerate() {
                let line = format!("{:<22}{}", cmd.describe(), app.keymap.label(*cmd));
                buf.set_stringn(rx, inner.y + 2 + i as u16, line, rw as usize, st);
            }
        }
        Section::Highlights => {
            let area = Rect { x: rx, y: inner.y, width: rw, height: inner.height.saturating_sub(3) };
            let styles = PrefsStyles { st, sel, dim, base: p.style(t.editor_fg, t.editor_bg) };
            let scroll = draw_prefs_hl(buf, app, state, area, &styles, p, &t, &mut hits);
            if let Some(s) = app.prefs.as_mut() {
                s.hl_scroll = scroll;
            }
            app.regions.prefs_hits = hits;
            draw_prefs_footer(buf, app, inner, rx, rw, st, dim);
            return;
        }
        _ => {
            let rows = section.rows();
            for (i, row) in rows.iter().enumerate() {
                let y = inner.y + i as u16;
                let focused = i == state.row;
                let lst = if focused { st.add_modifier(Modifier::BOLD) } else { st };
                buf.set_stringn(rx, y, format!("{:<20}", row.label()), 20, lst);
                let vx = rx + 21;
                let val = row.value(&settings);
                let vst = if focused { sel } else { st };
                buf.set_stringn(vx, y, &val, rw.saturating_sub(21) as usize, vst);
                hits.push((Rect { x: rx, y, width: rw, height: 1 }, PrefsHit::Row(i)));
                if val.starts_with('‹') {
                    let n = val.chars().count() as u16;
                    // ‹ and › are listed first so they win over the row hit.
                    hits.insert(0, (Rect { x: vx, y, width: 2, height: 1 }, PrefsHit::Dec(i)));
                    hits.insert(0, (Rect { x: vx + n - 2, y, width: 2, height: 1 }, PrefsHit::Inc(i)));
                }
            }
            if let Some(row) = rows.get(state.row) {
                buf.set_stringn(rx, inner.y + rows.len() as u16 + 1, row.hint(), rw as usize, dim);
            }
            if section == Section::Appearance {
                // A strip of the current roles, so a theme can be judged
                // before saving.
                let py = inner.y + rows.len() as u16 + 3;
                if py + 2 < inner.bottom().saturating_sub(2) {
                    let base = p.style(t.editor_fg, t.editor_bg);
                    let sel_style = p.plain_or(p.style(t.selection_fg, t.selection_bg), Modifier::REVERSED);
                    let search_style = p.plain_or(p.style(t.search_fg, t.search_bg), Modifier::UNDERLINED);
                    let accent = p.plain_or(p.style(t.accent_fg, t.accent), Modifier::REVERSED | Modifier::BOLD);
                    let surface = p.plain_or(p.style(t.surface_fg, t.surface_bg), Modifier::REVERSED);
                    let todo = crate::highlight::samples().into_iter().find(|g| g.name == "Todo");
                    let todo_style = todo.map_or(base, |g| base.patch(highlight_style(p, &crate::highlight::resolve(&g, &t))));
                    let chips: [(&str, Style); 7] = [
                        (" Text ", base),
                        (" Selected ", sel_style),
                        (" Match ", search_style),
                        (" Accent ", accent),
                        (" Bar ", surface),
                        (" Muted ", p.plain_or(p.style(t.muted, t.editor_bg), Modifier::DIM)),
                        (" TODO ", todo_style),
                    ];
                    buf.set_stringn(rx, py, "Preview", rw as usize, dim);
                    let mut x = rx;
                    for (label, style) in chips {
                        if x >= inner.right() {
                            break;
                        }
                        let (nx, _) = buf.set_stringn(x, py + 1, label, inner.right().saturating_sub(x) as usize, style);
                        x = nx + 1;
                    }
                }
            }
        }
    }

    app.regions.prefs_hits = hits;
    draw_prefs_footer(buf, app, inner, rx, rw, st, dim);
}

fn draw_prefs_footer(buf: &mut TermBuf, app: &mut App, inner: Rect, rx: u16, rw: u16, st: Style, dim: Style) {
    use crate::prefs::PrefsHit;
    let hits = &mut app.regions.prefs_hits;
    // Footer: buttons and hints.
    let fy = inner.bottom() - 1;
    let mut x = inner.x;
    for (label, hit) in [(" Save ", PrefsHit::Save), (" Apply ", PrefsHit::Apply), (" Cancel ", PrefsHit::Cancel)] {
        let bw = label.len() as u16;
        buf.set_stringn(x, fy, label, bw as usize, st.add_modifier(Modifier::BOLD | Modifier::REVERSED));
        hits.push((Rect { x, y: fy, width: bw, height: 1 }, hit));
        x += bw + 1;
    }
    buf.set_stringn(x, fy, " ←/→ change · Tab section · Ctrl+S save · Esc cancel", inner.right().saturating_sub(x) as usize, dim);
    if let Some(path) = &app.config_path {
        let shown = std::env::var_os("HOME")
            .and_then(|h| path.strip_prefix(h).ok().map(|r| format!("~/{}", r.display())))
            .unwrap_or_else(|| path.display().to_string());
        buf.set_stringn(rx, fy - 1, format!("Changes preview live · Save writes {shown}"), rw as usize, dim);
    }
}

struct PrefsStyles {
    st: Style,
    sel: Style,
    dim: Style,
    /// Document text colours, for previews.
    base: Style,
}

/// The Highlights page. Returns the list's scroll offset.
#[allow(clippy::too_many_arguments)]
fn draw_prefs_hl(
    buf: &mut TermBuf,
    app: &App,
    state: &crate::prefs::PrefsState,
    r: Rect,
    s: &PrefsStyles,
    p: &Painter,
    t: &crate::theme::Theme,
    hits: &mut Vec<(Rect, crate::prefs::PrefsHit)>,
) -> usize {
    use crate::prefs::PrefsHit;
    use crate::highlight::Origin;
    use crate::prefs_hl::{EditRow, HlHit, ListRow, list_rows};
    let h = &app.highlights;
    let w = r.width as usize;
    let style_of = |g: &crate::highlight::Group| s.base.patch(highlight_style(p, &crate::highlight::resolve(g, t)));
    if r.height < 3 {
        return state.hl_scroll;
    }

    if let Some(gi) = state.hl_edit {
        let Some(g) = h.groups.get(gi) else { return state.hl_scroll };
        let avail = r.height as usize - 2; // preview + hint
        let skip = state.hl_edit_row.saturating_sub(avail.saturating_sub(1));
        for (i, row) in EditRow::ALL.iter().enumerate().skip(skip).take(avail) {
            let y = r.y + (i - skip) as u16;
            let focused = i == state.hl_edit_row;
            let lst = if focused { s.st.add_modifier(Modifier::BOLD) } else { s.st };
            if *row == EditRow::Back {
                buf.set_stringn(r.x, y, row.label(), w, if focused { s.sel } else { s.st });
                hits.push((Rect { x: r.x, y, width: r.width, height: 1 }, PrefsHit::Hl(HlHit::EditRow(i))));
                continue;
            }
            buf.set_stringn(r.x, y, format!("{:<18}", row.label()), 18, lst);
            let vx = r.x + 19;
            let val = row.value(g);
            let (end, _) = buf.set_stringn(vx, y, &val, w.saturating_sub(19), if focused { s.sel } else { s.st });
            hits.push((Rect { x: r.x, y, width: r.width, height: 1 }, PrefsHit::Hl(HlHit::EditRow(i))));
            if val.starts_with('‹') {
                let n = val.chars().count() as u16;
                hits.insert(0, (Rect { x: vx, y, width: 2, height: 1 }, PrefsHit::Hl(HlHit::Dec(i))));
                hits.insert(0, (Rect { x: vx + n - 2, y, width: 2, height: 1 }, PrefsHit::Hl(HlHit::Inc(i))));
                // Swatch of the resolved colour.
                let c = if *row == EditRow::Fg { g.fg } else { g.bg };
                if let Some(c) = c
                    && !p.plain
                {
                    buf.set_stringn(end + 1, y, "██", 2, Style::default().fg(p.c(c.resolve(t))));
                }
            }
        }
        // Preview: the group's own terms, drawn as they appear in a document.
        let py = r.bottom() - 2;
        let (mut x, _) = buf.set_stringn(r.x, py, "Preview  ", w, s.dim);
        for term in &g.terms {
            if x >= r.right() {
                break;
            }
            let (nx, _) = buf.set_stringn(x, py, term, r.right().saturating_sub(x) as usize, style_of(g));
            let (nx, _) = buf.set_stringn(nx, py, "  ", r.right().saturating_sub(nx) as usize, s.base);
            x = nx;
        }
        if let Some(row) = EditRow::ALL.get(state.hl_edit_row) {
            buf.set_stringn(r.x, r.bottom() - 1, row.hint(), w, s.dim);
        }
        return state.hl_scroll;
    }

    let rows = list_rows(h);
    let groups = h.ordered();
    let avail = r.height as usize - 2; // blank + hint
    let cur = state.hl_row.min(rows.len() - 1);
    let mut scroll = state.hl_scroll.min(rows.len().saturating_sub(1));
    if cur < scroll {
        scroll = cur;
    } else if cur >= scroll + avail {
        scroll = cur + 1 - avail;
    }
    let check = |on: bool| if on { "[x]" } else { "[ ]" };
    for (i, row) in rows.iter().enumerate().skip(scroll).take(avail) {
        let y = r.y + (i - scroll) as u16;
        let focused = i == cur;
        let fst = if focused { s.sel } else { s.st };
        let row_hit = (Rect { x: r.x, y, width: r.width, height: 1 }, PrefsHit::Hl(HlHit::Row(i)));
        match row {
            ListRow::Enabled | ListRow::Samples => {
                let (label, on) = if *row == ListRow::Enabled { ("Highlighting", h.enabled) } else { ("Sample groups", h.samples) };
                buf.set_stringn(r.x, y, format!("{label:<18}"), 18, if focused { s.st.add_modifier(Modifier::BOLD) } else { s.st });
                buf.set_stringn(r.x + 19, y, format!("{} {}", check(on), if on { "on" } else { "off" }), w.saturating_sub(19), fst);
                hits.push(row_hit);
            }
            ListRow::Group(gi) => {
                let (g, origin) = &groups[*gi];
                buf.set_stringn(r.x, y, check(g.enabled), 3, fst);
                hits.insert(0, (Rect { x: r.x, y, width: 3, height: 1 }, PrefsHit::Hl(HlHit::Toggle(i))));
                let live = g.enabled && h.enabled;
                let name: String = g.name.chars().take(16).collect();
                let (nx, _) = buf.set_stringn(r.x + 4, y, &name, 16, if live { style_of(g) } else { s.dim });
                let mut rest = String::new();
                for _ in nx..r.x + 21 {
                    rest.push(' ');
                }
                rest.push_str(&g.terms.join(", "));
                match origin {
                    Origin::Sample => rest.push_str("  (sample)"),
                    Origin::Override(_) => rest.push_str("  (sample, edited)"),
                    Origin::User(_) => {}
                }
                buf.set_stringn(nx, y, &rest, r.right().saturating_sub(nx) as usize, s.dim);
                hits.push(row_hit);
            }
            ListRow::New => {
                buf.set_stringn(r.x, y, "+ New group", w, fst);
                hits.push(row_hit);
            }
        }
    }
    buf.set_stringn(
        r.x,
        r.bottom() - 1,
        "Space toggle · Enter edit · N new · Del delete · Alt+↑↓ move",
        w,
        s.dim,
    );
    scroll
}

fn digits(n: usize) -> usize {
    n.max(1).ilog10() as usize + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_middle_keeps_ends() {
        assert_eq!(fit_middle("dropped note.txt", 20), "dropped note.txt");
        assert_eq!(fit_middle("dropped note.txt", 9), "drop….txt");
        assert_eq!(fit_middle("abc", 1), "…");
    }

    #[test]
    fn nearest_256_basics() {
        assert_eq!(nearest_256(Rgb(0, 0, 0)), 16);
        assert_eq!(nearest_256(Rgb(255, 255, 255)), 231);
        assert_eq!(nearest_256(Rgb(128, 128, 128)), 244);
        assert_eq!(nearest_256(Rgb(255, 0, 0)), 196);
    }
}
