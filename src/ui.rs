//! Rendering. Everything here asks the `Theme` for semantic roles; only
//! `Painter` knows how roles become terminal colours.

use crate::app::{App, Button, Level, TabHit, ThemeSource};
use crate::find::FindHit;
use crate::layout::GlyphKind;
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
    let in_match = |s: usize, e: usize| {
        let i = matches.partition_point(|&(_, me)| me <= s);
        matches.get(i).is_some_and(|&(ms, me)| s >= ms && e <= me)
    };

    let scroll_x = if app.wrap { 0 } else { app.tab.scroll_x };
    for (i, row) in rows.iter().enumerate() {
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
            if g.kind == GlyphKind::Substitute {
                st = st.patch(sub_style);
            }
            if in_match(g.start, g.end) {
                st = search_style;
            }
            if g.start >= sel.start() && g.end <= sel.end() && !sel.is_empty() {
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
        let col = row.col_of(sel.head) as isize - scroll_x as isize;
        if col >= 0 && col <= text.width as isize {
            caret_pos = Some(Position { x: (text.x + col as u16).min(text.right().saturating_sub(1)), y: text.y + i as u16 });
        }
    }

    draw_scrollbar(buf, app, scrollbar, rows.len(), &p);
    app.rendered_rows = rows;

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
        // Show the tail of long input.
        let shown: String = {
            let n = pr.input.chars().count();
            pr.input.chars().skip(n.saturating_sub(input_w.saturating_sub(1))).collect()
        };
        let (end, _) = buf.set_stringn(prompt.x + lw, prompt.y, &shown, input_w, surface);
        buf.set_stringn(end + 1, prompt.y, hint, prompt.right().saturating_sub(end + 1) as usize, surface.add_modifier(Modifier::DIM));
        caret_pos = Some(Position { x: end.min(prompt.right() - 1), y: prompt.y });
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
    buf.set_stringn(status.right().saturating_sub(rw), status.y, &right, rw as usize, surface);

    if app.show_help {
        draw_help(buf, app, area, &p);
        caret_pos = None;
    }
    app.regions.dialog_buttons.clear();
    if app.dialog.is_some() {
        draw_dialog(buf, app, area, &p);
        caret_pos = None;
    }

    if let Some(pos) = caret_pos {
        f.set_cursor_position(pos);
    }
}

fn draw_tab_bar(buf: &mut TermBuf, app: &mut App, title: Rect, surface: Style, accent: Style, p: &Painter) {
    buf.set_style(title, surface);
    let buttons: [(&str, Button, bool); 4] = [
        (" Save ", Button::Save, app.tab.buf.is_dirty()),
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

    let mut draw_field = |buf: &mut TermBuf, y: u16, label: &str, text: &str, focused: bool, hit: FindHit| {
        buf.set_stringn(r.x, y, label, label_w as usize, if focused { accent } else { surface });
        let fx = r.x + label_w;
        buf.set_style(Rect { x: fx, y, width: field_w, height: 1 }, field);
        let n = text.chars().count();
        let shown: String = text.chars().skip(n.saturating_sub(field_w.saturating_sub(1) as usize)).collect();
        let (end, _) = buf.set_stringn(fx, y, &shown, field_w as usize, field);
        hits.push((Rect { x: fx, y, width: field_w, height: 1 }, hit));
        if focused {
            caret = Some(Position { x: end.min(fx + field_w - 1), y });
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
        return format!(" {} ", parts[0]);
    }
    if !app.tab.buf.sel.is_empty() {
        parts.push(format!("Sel {}", app.tab.buf.sel.end() - app.tab.buf.sel.start()));
    }
    parts.push(if app.tab.doc.has_bom { "UTF-8 BOM".into() } else { "UTF-8".into() });
    let le = app.tab.doc.line_ending.label();
    parts.push(if app.tab.doc.mixed_line_endings { format!("{le}*") } else { le.into() });
    parts.push(match &app.theme_source {
        ThemeSource::Builtin => "Dracula".into(),
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
    ] {
        lines.push(pair(a, b));
    }
    lines.extend([
        String::new(),
        "Fixed: Alt+1..9 go to tab · Tab/Shift+Tab indent · Ctrl/Alt+←/→ word · Esc clears selection".into(),
        "Find bar: Alt+C case · Alt+W whole word · Alt+L in selection · Alt+A replace all".into(),
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
