//! Frame drawing. Each frame is built into one buffer and written inside a
//! synchronized update, so there is no flicker even though we redraw fully.

use std::fmt::Write as _;
use std::io::{self, Write};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Geometry, Hit, HitTarget, Mode};
use crate::kitty;
use crate::layout::{Kind, Seg, truncate};
use crate::style::Style;

fn goto(buf: &mut String, y: u16, x: u16) {
    let _ = write!(buf, "\x1b[{};{}H", y + 1, x + 1);
}

fn pad(buf: &mut String, n: usize) {
    buf.extend(std::iter::repeat_n(' ', n));
}

const HELP: &[(&str, &str)] = &[
    ("j k ↓ ↑", "scroll by line"),
    ("d u", "half page down / up"),
    ("space b", "page down / up"),
    ("g G", "top / bottom"),
    ("] [", "next / previous heading"),
    ("1-9", "jump to top-level section"),
    ("Tab S-Tab", "focus next / previous link"),
    ("Enter", "follow focused link"),
    ("click", "follow link, jump via contents"),
    ("click code", "copy the code block"),
    ("drag", "select text and copy it"),
    ("Backspace h", "go back"),
    ("/ n N", "search, next / previous match"),
    ("t", "toggle table of contents"),
    ("r", "reload"),
    ("Esc", "clear focus / search"),
    ("q", "quit"),
];

fn is_external(target: &str) -> bool {
    ["http://", "https://", "mailto:"].iter().any(|p| target.starts_with(p))
}

impl App {
    pub fn draw(&mut self, out: &mut impl Write) -> io::Result<()> {
        let g = self.geometry();
        let (w, h) = self.size;
        let mut pre = String::new();
        let mut buf = String::with_capacity(64 * 1024);
        self.hits.clear();

        for y in 0..h {
            goto(&mut buf, y, 0);
            buf.push_str("\x1b[0m\x1b[2K");
        }

        let current = self.current_heading();
        let toc_rows = (g.rows as usize).saturating_sub(2);
        if let Some(c) = current {
            if c < self.toc_scroll {
                self.toc_scroll = c;
            } else if c >= self.toc_scroll + toc_rows {
                self.toc_scroll = c + 1 - toc_rows;
            }
        }

        for y in 0..g.rows {
            if g.toc_w > 0 {
                self.draw_toc_row(&mut buf, y, g, current);
            }
            let li = self.scroll + y as usize;
            let Some(line) = self.layout.lines.get(li) else { continue };
            goto(&mut buf, y, g.content_x);
            match line.kind {
                Kind::Text => self.draw_segs(&mut buf, &mut pre, li, &line.segs.clone(), y, g.content_x),
                Kind::Heading { scale, prefix } if y as usize + scale.rows() <= g.rows as usize => {
                    let segs = line.segs.clone();
                    self.draw_segs(&mut buf, &mut pre, usize::MAX, &segs[..prefix], y, g.content_x);
                    let text: String = segs[prefix..].iter().map(|s| s.text.as_str()).collect();
                    let style = segs.get(prefix).map(|s| s.style).unwrap_or_default();
                    style.write_sgr(&mut buf);
                    scale.render(&text, &mut buf);
                    buf.push_str("\x1b[0m");
                }
                // A scaled heading that doesn't fit entirely is drawn at normal size.
                Kind::Heading { .. } => self.draw_segs(&mut buf, &mut pre, li, &line.segs.clone(), y, g.content_x),
                Kind::HeadingCont { top } if y == 0 => {
                    let segs = self.layout.lines[top].segs.clone();
                    self.draw_segs(&mut buf, &mut pre, top, &segs, y, g.content_x);
                }
                Kind::HeadingCont { .. } => {
                    self.draw_segs(&mut buf, &mut pre, li, &line.segs.clone(), y, g.content_x);
                }
            }
        }

        self.draw_status(&mut buf, g, w, h);
        self.draw_hover(&mut buf, g, w);
        if self.mode == Mode::Help {
            self.draw_help(&mut buf, w, h);
        }
        if let Mode::Search(q) = &self.mode {
            let x = 1 + q.width() as u16 + 1;
            goto(&mut buf, h - 1, x.min(w.saturating_sub(1)));
            buf.push_str("\x1b[?25h");
        } else {
            buf.push_str("\x1b[?25l");
        }

        out.write_all(b"\x1b[?2026h")?;
        out.write_all(std::mem::take(&mut self.pending).as_bytes())?;
        out.write_all(pre.as_bytes())?;
        out.write_all(buf.as_bytes())?;
        out.write_all(b"\x1b[?2026l")?;
        out.flush()
    }

    /// Draws segments at the cursor. `li` is the layout line (for search
    /// highlights).
    fn draw_segs(&mut self, buf: &mut String, pre: &mut String, li: usize, segs: &[Seg], y: u16, x0: u16) {
        let t = &self.theme;
        let first = self.search.matches.partition_point(|m| m.line < li);
        let matches: Vec<(usize, usize, bool)> = self.search.matches[first..]
            .iter()
            .enumerate()
            .take_while(|(_, m)| m.line == li)
            .map(|(i, m)| (m.start, m.end, first + i == self.search.current))
            .collect();
        let hl_style = |cur: bool| Style::fg(t.search_fg).on(if cur { t.search_cur_bg } else { t.search_bg });
        let selected = self.selection.and_then(|s| s.cols(li));

        let mut col = 0usize;
        for seg in segs {
            let w = seg.width();
            if let Some(cell) = seg.image {
                let key = &self.image_keys[cell.img];
                match self.images.ensure(key, cell.cols, cell.rows, pre) {
                    Some(id) => {
                        Style::fg(kitty::id_color(id)).write_sgr(buf);
                        kitty::placeholder_row(cell.row, cell.cols, buf);
                    }
                    None => {
                        buf.push_str("\x1b[0m");
                        pad(buf, w);
                    }
                }
            } else {
                let mut st = seg.style;
                if seg.link.is_some() && seg.link == self.focus {
                    st.reverse = true;
                }
                let url = seg.link.map(|l| self.doc.links[l].as_str()).filter(|u| is_external(u));
                if let Some(u) = url {
                    let _ = write!(buf, "\x1b]8;;{u}\x1b\\");
                }
                let overlaps = matches.iter().any(|(s, e, _)| *s < col + w && *e > col)
                    || selected.is_some_and(|(s, e)| s < col + w && e > col);
                if !overlaps {
                    st.write_sgr(buf);
                    buf.push_str(&seg.text);
                } else {
                    let mut c = col;
                    let mut last: Option<Style> = None;
                    for ch in seg.text.chars() {
                        let want = if selected.is_some_and(|(s, e)| c >= s && c < e) {
                            Style { bg: Some(t.select_bg), reverse: false, ..st }
                        } else {
                            let hit = matches.iter().find(|(s, e, _)| c >= *s && c < *e);
                            hit.map(|(_, _, cur)| hl_style(*cur)).unwrap_or(st)
                        };
                        if last != Some(want) {
                            want.write_sgr(buf);
                            last = Some(want);
                        }
                        buf.push(ch);
                        c += ch.width().unwrap_or(0);
                    }
                }
                if url.is_some() {
                    buf.push_str("\x1b]8;;\x1b\\");
                }
            }
            if let Some(l) = seg.link {
                let x = x0 + col as u16;
                self.hits.push(Hit { y, x0: x, x1: x + w as u16, target: HitTarget::Link(l) });
            }
            col += w;
        }
        buf.push_str("\x1b[0m");
    }

    fn draw_toc_row(&mut self, buf: &mut String, y: u16, g: Geometry, current: Option<usize>) {
        let t = &self.theme;
        let width = g.toc_w as usize;
        goto(buf, y, 0);
        if y == 0 {
            Style::fg(t.dim).bold().write_sgr(buf);
            buf.push_str(&truncate(" CONTENTS", width));
        } else if y >= 2 {
            let i = self.toc_scroll + y as usize - 2;
            if let Some(h) = self.layout.headings.get(i) {
                let top = self.layout.headings.iter().map(|h| h.level).min().unwrap_or(1);
                let depth = (h.level - top) as usize;
                let indent = 2 * depth;
                let text = truncate(&h.title, width.saturating_sub(indent + 3));
                let active = Some(i) == current;
                let color = t.headings[h.level as usize - 1];
                let mut style = if active {
                    Style::fg(color).bold().on(t.sel_bg)
                } else if depth == 0 {
                    Style::fg(t.fg).bold()
                } else {
                    Style::fg(if depth == 1 { t.fg } else { t.dim })
                };
                if active {
                    Style::fg(t.accent).on(t.sel_bg).write_sgr(buf);
                    buf.push('▌');
                } else {
                    buf.push(' ');
                }
                style.write_sgr(buf);
                pad(buf, indent + 1);
                buf.push_str(&text);
                let used = 1 + indent + 1 + text.width();
                if active {
                    pad(buf, width.saturating_sub(used));
                }
                style = Style::default();
                style.write_sgr(buf);
                self.hits.push(Hit { y, x0: 0, x1: g.toc_w, target: HitTarget::Toc(i) });
            }
        }
        goto(buf, y, g.toc_w);
        Style::fg(t.border).write_sgr(buf);
        buf.push('│');
        buf.push_str("\x1b[0m");
    }

    fn draw_status(&self, buf: &mut String, g: Geometry, w: u16, h: u16) {
        let t = &self.theme;
        let width = w as usize;
        goto(buf, h - 1, 0);
        let bar = Style::fg(t.bar_fg).on(t.bar_bg);

        let total = self.layout.lines.len();
        let rows = g.rows as usize;
        let pct = if total <= rows { 100 } else { ((self.scroll + rows).min(total) * 100) / total };
        let mut right = String::new();
        if !self.search.query.is_empty() {
            let n = self.search.matches.len();
            let cur = if n == 0 { 0 } else { self.search.current + 1 };
            let _ = write!(right, " /{} {cur}/{n} ", self.search.query);
        }
        let loading = self.images.loading();
        if loading > 0 {
            let _ = write!(right, " ⟳ {loading} image{} ", if loading == 1 { "" } else { "s" });
        }
        let _ = write!(right, " {pct:>3}%  ? help ");

        if let Mode::Search(q) = &self.mode {
            bar.write_sgr(buf);
            let s = truncate(&format!(" /{q}"), width);
            buf.push_str(&s);
            pad(buf, width.saturating_sub(s.width()));
            buf.push_str("\x1b[0m");
            return;
        }

        let chip = format!(" {} ", self.source.name);
        let chip = truncate(&chip, width / 3);
        Style::fg(t.on_accent).on(t.accent).bold().write_sgr(buf);
        buf.push_str(&chip);

        let middle = match &self.message {
            Some((m, _)) => m.clone(),
            None => self.current_heading().map(|i| self.layout.headings[i].title.clone()).unwrap_or_default(),
        };
        let room = width.saturating_sub(chip.width() + right.width());
        let middle = truncate(&format!(" {middle}"), room);
        let mut ms = bar;
        if self.message.is_none() {
            ms.italic = true;
        }
        ms.write_sgr(buf);
        buf.push_str(&middle);
        bar.write_sgr(buf);
        pad(buf, room.saturating_sub(middle.width()));
        Style::fg(t.dim).on(t.bar_bg).write_sgr(buf);
        buf.push_str(&truncate(&right, width.saturating_sub(chip.width())));
        buf.push_str("\x1b[0m");
    }

    /// One-line popup with the hovered link's target, just below the link
    /// (or above it on the last content row).
    fn draw_hover(&self, buf: &mut String, g: Geometry, w: u16) {
        let t = &self.theme;
        let Some(hover) = self.hover else { return };
        let Some(info) = self.link_info.get(hover.link).filter(|i| !i.label.is_empty()) else { return };
        let text = truncate(&format!(" {} ", info.label), (w as usize).saturating_sub(2));
        let width = text.width() as u16;
        let y = if hover.y + 1 < g.rows { hover.y + 1 } else { hover.y.saturating_sub(1) };
        let x = hover.x.min(w.saturating_sub(width + 1));
        goto(buf, y, x);
        let style =
            if info.broken { Style::fg(t.on_accent).on(t.caution).bold() } else { Style::fg(t.fg).on(t.sel_bg) };
        style.write_sgr(buf);
        buf.push_str(&text);
        buf.push_str("\x1b[0m");
    }

    fn draw_help(&self, buf: &mut String, w: u16, h: u16) {
        let t = &self.theme;
        let key_w = HELP.iter().map(|(k, _)| k.width()).max().unwrap_or(0);
        let desc_w = HELP.iter().map(|(_, d)| d.width()).max().unwrap_or(0);
        let box_w = (key_w + desc_w + 7).min(w as usize);
        let box_h = HELP.len() + 4;
        let x = (w as usize).saturating_sub(box_w) / 2;
        let y = (h as usize).saturating_sub(box_h) / 2;
        let border = Style::fg(t.accent).on(t.bar_bg);
        let body = Style::fg(t.fg).on(t.bar_bg);
        let inner = box_w.saturating_sub(2);

        goto(buf, y as u16, x as u16);
        border.write_sgr(buf);
        let title = " Keys ";
        let _ = write!(buf, "╭─{title}{}╮", "─".repeat(inner.saturating_sub(title.width() + 1)));
        for row in 0..box_h - 2 {
            goto(buf, (y + 1 + row) as u16, x as u16);
            border.write_sgr(buf);
            buf.push('│');
            let entry = row.checked_sub(1).and_then(|i| HELP.get(i));
            let used = match entry {
                Some((k, d)) => {
                    Style::fg(t.accent).on(t.bar_bg).bold().write_sgr(buf);
                    let _ = write!(buf, "  {k:<key_w$}");
                    body.write_sgr(buf);
                    let _ = write!(buf, "  {d}");
                    2 + key_w + 2 + d.width()
                }
                None => {
                    body.write_sgr(buf);
                    0
                }
            };
            pad(buf, inner.saturating_sub(used));
            border.write_sgr(buf);
            buf.push('│');
        }
        goto(buf, (y + box_h - 1) as u16, x as u16);
        border.write_sgr(buf);
        let _ = write!(buf, "╰{}╯", "─".repeat(inner));
        buf.push_str("\x1b[0m");
    }
}
