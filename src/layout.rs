//! Turns the document model into terminal lines for a given width.

use std::collections::HashMap;

use pulldown_cmark::{Alignment, BlockQuoteKind};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::convert::{Format, Variants};
use crate::doc::{Block, CodeSrc, Doc, Inline, InlineStyle, Item, Length, Src, plain_text};
use crate::highlight::Highlighter;
use crate::kitty::{MAX_PLACEHOLDER_CELLS, Scale};
use crate::style::Style;
use crate::theme::Theme;

/// One row of an image drawn with kitty placeholders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageCell {
    /// Index into `Doc::images`.
    pub img: usize,
    pub cols: u16,
    pub rows: u16,
    pub row: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    pub text: String,
    pub style: Style,
    pub link: Option<usize>,
    pub image: Option<ImageCell>,
    /// Where this text sits in the Markdown source, for copying selections.
    pub src: Option<Src>,
    /// Decoration (code block padding, quote bars) left out of copied text.
    pub decor: bool,
}

impl Seg {
    pub fn new(text: impl Into<String>, style: Style) -> Seg {
        Seg { text: text.into(), style, link: None, image: None, decor: false, src: None }
    }

    pub fn decor(text: impl Into<String>, style: Style) -> Seg {
        Seg { decor: true, ..Seg::new(text, style) }
    }

    pub fn width(&self) -> usize {
        match self.image {
            Some(cell) => cell.cols as usize,
            None => self.text.width(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Text,
    /// First row of a scaled heading. The first `prefix` segs are drawn
    /// normally (list bullets, quote bars), the rest at `scale`.
    Heading {
        scale: Scale,
        prefix: usize,
    },
    /// Rows covered by the scaled heading starting at line `top`.
    HeadingCont {
        top: usize,
    },
}

#[derive(Clone, Debug)]
pub struct Line {
    pub kind: Kind,
    pub segs: Vec<Seg>,
}

impl Line {
    pub fn plain(&self) -> String {
        self.segs
            .iter()
            .map(|s| match s.image {
                Some(c) => "▒".repeat(c.cols as usize),
                None => s.text.clone(),
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct HeadingEntry {
    pub level: u8,
    pub title: String,
    pub line: usize,
}

/// Lines `start..end` hold a code block whose source is `text`.
#[derive(Clone, Debug)]
pub struct CodeSpan {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// A clickable format tab in a data code block's header.
#[derive(Clone, Debug)]
pub struct Tab {
    pub line: usize,
    /// Columns `x0..x1` relative to the text column.
    pub x0: usize,
    pub x1: usize,
    pub block: usize,
    pub format: Format,
    pub source: Format,
    /// Why the block can't be shown in this format.
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Layout {
    pub lines: Vec<Line>,
    pub headings: Vec<HeadingEntry>,
    pub anchors: HashMap<String, usize>,
    pub code_blocks: Vec<CodeSpan>,
    pub tabs: Vec<Tab>,
}

pub struct Env<'a> {
    pub theme: &'a Theme,
    pub hl: &'a Highlighter,
    pub text_sizing: bool,
    /// Pixel size of a loaded image, by `Doc::images` index.
    pub image_dims: &'a dyn Fn(usize) -> Option<(u32, u32)>,
    pub cell: (u16, u16),
    pub max_image_rows: usize,
    /// Format a data code block (by id) is shown in, if switched.
    pub code_format: &'a dyn Fn(usize) -> Option<Format>,
    /// Why an image failed to load, by `Doc::images` index.
    pub image_error: &'a dyn Fn(usize) -> Option<String>,
    /// Per link id: true when the target is a missing file or anchor.
    pub broken_links: &'a [bool],
}

pub fn layout(doc: &Doc, width: usize, env: &Env) -> Layout {
    let mut b = Builder { doc, env, out: Layout::default() };
    b.blocks(&doc.blocks, &[], &[], width.max(12), 0);
    b.out
}

enum Piece {
    Segs(Vec<Seg>),
    Image(ImageCell, Option<usize>, Option<Src>),
}

struct Builder<'a> {
    doc: &'a Doc,
    env: &'a Env<'a>,
    out: Layout,
}

fn concat(a: &[Seg], b: Seg) -> Vec<Seg> {
    let mut v = a.to_vec();
    v.push(b);
    v
}

fn gap_between(prev: &Block, next: &Block) -> bool {
    !matches!(prev, Block::Plain(_))
        && !matches!((prev, next), (Block::DefTitle(_), Block::DefBody(_)) | (Block::DefBody(_), Block::DefBody(_)))
}

impl Builder<'_> {
    fn push(&mut self, prefix: &[Seg], segs: Vec<Seg>) {
        let mut v = prefix.to_vec();
        v.extend(segs);
        self.out.lines.push(Line { kind: Kind::Text, segs: v });
    }

    fn blank(&mut self, prefix: &[Seg]) {
        let mut v = prefix.to_vec();
        while let Some(last) = v.last_mut() {
            let t = last.text.trim_end().to_string();
            if t.is_empty() {
                v.pop();
            } else {
                last.text = t;
                break;
            }
        }
        self.push(&v, Vec::new());
    }

    fn blocks(&mut self, blocks: &[Block], first: &[Seg], rest: &[Seg], width: usize, depth: usize) {
        for (i, b) in blocks.iter().enumerate() {
            if i > 0 && gap_between(&blocks[i - 1], b) {
                self.blank(rest);
            }
            self.block(b, if i == 0 { first } else { rest }, rest, width, depth);
        }
    }

    fn block(&mut self, block: &Block, first: &[Seg], rest: &[Seg], width: usize, depth: usize) {
        let t = self.env.theme;
        match block {
            Block::Paragraph(inl) | Block::Plain(inl) => {
                let pieces = self.pieces(inl, Style::default(), width, true);
                self.flow(pieces, first, rest, width);
            }
            Block::Heading { level, inlines, anchor } => self.heading(*level, inlines, anchor, first, rest, width),
            Block::Code { lang, text, src, id, variants } => {
                let data = variants.as_ref().map(|v| (*id, v));
                self.code(lang, text, lang, Some(src), data, first, rest, width)
            }
            Block::Mermaid { img, source, src } => match self.image_cell(*img, width) {
                Some(cell) => {
                    let start = self.out.lines.len();
                    let span = Some(Src::span(src.block.0, src.block.1));
                    self.flow(vec![Piece::Image(cell, None, span)], first, rest, width);
                    let caption =
                        Seg { src: span, ..Seg::decor("mermaid · click to copy source", Style::fg(t.dim).italic()) };
                    self.push(rest, vec![caption]);
                    let text = source.trim_end().to_string();
                    self.out.code_blocks.push(CodeSpan { start, end: self.out.lines.len(), text });
                }
                // Not rendered (yet): show the source, with the error if it failed.
                None => {
                    let label = match (self.env.image_error)(*img) {
                        Some(e) => format!("mermaid · ✖ {e}"),
                        None => "mermaid".to_string(),
                    };
                    self.code("mermaid", source, &label, Some(src), None, first, rest, width);
                }
            },
            Block::FrontMatter(text, src) => {
                self.code("yaml", text, "front matter", Some(src), None, first, rest, width)
            }
            Block::Quote { kind, blocks } => {
                let (color, title) = match kind {
                    Some(BlockQuoteKind::Note) => (t.note, Some("ⓘ Note")),
                    Some(BlockQuoteKind::Tip) => (t.tip, Some("✓ Tip")),
                    Some(BlockQuoteKind::Important) => (t.important, Some("❢ Important")),
                    Some(BlockQuoteKind::Warning) => (t.warning, Some("⚠ Warning")),
                    Some(BlockQuoteKind::Caution) => (t.caution, Some("✖ Caution")),
                    None => (t.quote, None),
                };
                let bar = Seg::decor("▎ ", Style::fg(color));
                let f = concat(first, bar.clone());
                let r = concat(rest, bar);
                let w = width.saturating_sub(2).max(4);
                match title {
                    Some(title) => {
                        self.push(&f, vec![Seg::new(title, Style::fg(color).bold())]);
                        self.blocks(blocks, &r, &r, w, depth);
                    }
                    None => self.blocks(blocks, &f, &r, w, depth),
                }
            }
            Block::List { start, items } => self.list(*start, items, first, rest, width, depth),
            Block::Table { aligns, head, rows, cell_aligns } => {
                self.table(aligns, cell_aligns, head, rows, first, rest, width)
            }
            Block::Rule(a, b) => {
                let seg = Seg { src: Some(Src::span(*a, *b)), ..Seg::new("─".repeat(width), Style::fg(t.border)) };
                self.push(first, vec![seg]);
            }
            Block::Footnote { label, blocks } => {
                self.out.anchors.insert(format!("fn-{label}"), self.out.lines.len());
                let marker = format!("[{label}] ");
                let mw = marker.width();
                let f = concat(first, Seg::new(marker, Style::fg(t.link)));
                let r = concat(rest, Seg::new(" ".repeat(mw), Style::default()));
                self.blocks(blocks, &f, &r, width.saturating_sub(mw).max(4), depth);
            }
            Block::DefTitle(inl) => {
                let pieces = self.pieces(inl, Style::default().bold(), width, true);
                self.flow(pieces, first, rest, width);
            }
            Block::DefBody(blocks) => {
                let f = concat(first, Seg::new("  → ", Style::fg(t.dim)));
                let r = concat(rest, Seg::new("    ", Style::default()));
                self.blocks(blocks, &f, &r, width.saturating_sub(4).max(4), depth);
            }
            Block::Center(blocks) => {
                let start = self.out.lines.len();
                self.blocks(blocks, first, rest, width, depth);
                self.center(start, first.len(), rest.len(), width);
            }
        }
    }

    /// Centers lines `start..` in `width` by indenting each after its prefix
    /// (`first` segments on the first line, `rest` on the others).
    fn center(&mut self, start: usize, first: usize, rest: usize, width: usize) {
        for (k, line) in self.out.lines[start..].iter_mut().enumerate() {
            let (at, used) = match line.kind {
                Kind::Text => {
                    let at = if k == 0 { first } else { rest };
                    (at, line.segs.iter().skip(at).map(Seg::width).sum::<usize>())
                }
                Kind::Heading { scale, prefix } => {
                    let text: String =
                        line.segs[prefix.min(line.segs.len())..].iter().map(|s| s.text.as_str()).collect();
                    (prefix, scale.width(&text))
                }
                Kind::HeadingCont { .. } => continue,
            };
            let pad = width.saturating_sub(used) / 2;
            if pad == 0 || used == 0 || line.segs.len() <= at {
                continue;
            }
            line.segs.insert(at, Seg::decor(" ".repeat(pad), Style::default()));
            if let Kind::Heading { prefix, .. } = &mut line.kind {
                *prefix += 1;
            }
        }
    }

    fn heading(&mut self, level: u8, inlines: &[Inline], anchor: &str, first: &[Seg], rest: &[Seg], width: usize) {
        let t = self.env.theme;
        let level = level.clamp(1, 6);
        let color = t.headings[level as usize - 1];
        let line = self.out.lines.len();
        self.out.anchors.entry(anchor.to_string()).or_insert(line);
        self.out.headings.push(HeadingEntry {
            level,
            title: plain_text(inlines, &self.doc.images).trim().to_string(),
            line,
        });

        let base = Style::fg(color).bold();
        let segs: Vec<Seg> = self
            .pieces(inlines, base, width, false)
            .into_iter()
            .flat_map(|p| match p {
                Piece::Segs(s) => s,
                Piece::Image(..) => Vec::new(),
            })
            .collect();
        let scale = match (self.env.text_sizing, level) {
            (true, 1) => Scale { s: 2, n: 0, d: 0 },
            (true, 2) => Scale { s: 2, n: 3, d: 4 },
            _ => Scale::ONE,
        };
        if scale.is_one() {
            self.flow(vec![Piece::Segs(segs)], first, rest, width);
        } else {
            // Leave one scaled cell of slack for rounding in fractional chunks.
            let usable = ((width.saturating_sub(scale.s as usize)) as f32 / scale.factor()).floor().max(4.0) as usize;
            for (i, row) in wrap(&segs, usable).into_iter().enumerate() {
                let top = self.out.lines.len();
                let mut v = if i == 0 { first } else { rest }.to_vec();
                let prefix = v.len();
                let text: String = row.iter().map(|s| s.text.as_str()).collect();
                let fits = scale.width(&text) <= width;
                v.extend(row);
                if !fits {
                    self.out.lines.push(Line { kind: Kind::Text, segs: v });
                    continue;
                }
                self.out.lines.push(Line { kind: Kind::Heading { scale, prefix }, segs: v });
                for _ in 1..scale.rows() {
                    self.out.lines.push(Line { kind: Kind::HeadingCont { top }, segs: rest.to_vec() });
                }
            }
        }
        match level {
            1 => self.push(rest, vec![Seg::new("━".repeat(width), Style::fg(color.mix(t.bg, 0.55)))]),
            2 => self.push(rest, vec![Seg::new("─".repeat(width), Style::fg(t.border))]),
            _ => {}
        }
    }

    fn list(&mut self, start: Option<u64>, items: &[Item], first: &[Seg], rest: &[Seg], width: usize, depth: usize) {
        let t = self.env.theme;
        let loose = items.iter().any(|i| i.blocks.iter().any(|b| matches!(b, Block::Paragraph(_))));
        let num_w = start.map(|s| (s + items.len() as u64).saturating_sub(1).to_string().len() + 2).unwrap_or(2);
        const BULLETS: [&str; 4] = ["•", "◦", "▪", "‣"];
        for (i, item) in items.iter().enumerate() {
            if i > 0 && loose {
                self.blank(rest);
            }
            let (marker, style) = match (item.task, start) {
                (Some(true), _) => ("☑ ".to_string(), Style::fg(t.check)),
                (Some(false), _) => ("☐ ".to_string(), Style::fg(t.dim)),
                (None, Some(s)) => {
                    (format!("{:>w$} ", format!("{}.", s + i as u64), w = num_w - 1), Style::fg(t.accent))
                }
                (None, None) => (format!("{} ", BULLETS[depth % 4]), Style::fg(t.accent)),
            };
            let mw = marker.width();
            let f = concat(if i == 0 { first } else { rest }, Seg::new(marker, style));
            let r = concat(rest, Seg::new(" ".repeat(mw), Style::default()));
            if item.blocks.is_empty() {
                self.push(&f, Vec::new());
            } else {
                self.blocks(&item.blocks, &f, &r, width.saturating_sub(mw).max(4), depth + 1);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn code(
        &mut self,
        lang: &str,
        text: &str,
        label: &str,
        src: Option<&CodeSrc>,
        data: Option<(usize, &Variants)>,
        first: &[Seg],
        rest: &[Seg],
        width: usize,
    ) {
        let t = self.env.theme;
        let bg = t.code_bg;
        let inner = width.saturating_sub(2).max(1);
        // Data blocks may be shown converted. Converted text isn't in the
        // source, so it has no source mapping.
        let shown = data.map(|(id, v)| (self.env.code_format)(id).unwrap_or(v.source));
        let converted = match (data, shown) {
            (Some((_, v)), Some(f)) if f != v.source => v.get(f).clone().ok(),
            _ => None,
        };
        let (text, lang, src) = match (&converted, shown) {
            (Some(c), Some(f)) => (c.as_str(), f.lang(), None),
            _ => (text, lang, src),
        };
        let pad = |segs: Vec<Seg>| {
            let used: usize = segs.iter().map(Seg::width).sum();
            let mut v = vec![Seg::decor(" ", Style::default().on(bg))];
            v.extend(segs);
            v.push(Seg::decor(" ".repeat(inner.saturating_sub(used) + 1), Style::default().on(bg)));
            v
        };
        let start = self.out.lines.len();
        let hint = "⧉ click to copy";
        let mut header = Vec::new();
        let mut used = 0;
        match (data, shown) {
            (Some((id, v)), Some(shown)) => {
                // Tabs: the active one highlighted, unavailable ones struck out.
                let line = self.out.lines.len();
                let x0 = first.iter().map(Seg::width).sum::<usize>() + 1;
                let wide = inner >= 34;
                for f in Format::ALL {
                    let error = v.get(f).as_ref().err().cloned();
                    let text = match (f == v.source, wide) {
                        (true, true) => format!(" {} (source) ", f.label()),
                        (true, false) => format!(" {}• ", f.label()),
                        (false, _) => format!(" {} ", f.label()),
                    };
                    let style = if f == shown {
                        Style::fg(t.on_accent).on(t.accent).bold()
                    } else if error.is_none() {
                        Style::fg(t.fg).on(t.border)
                    } else {
                        Style { strike: true, ..Style::fg(t.dim).on(bg) }
                    };
                    let w = text.width();
                    self.out.tabs.push(Tab {
                        line,
                        x0: x0 + used,
                        x1: x0 + used + w,
                        block: id,
                        format: f,
                        source: v.source,
                        error,
                    });
                    header.push(Seg::decor(text, style));
                    header.push(Seg::decor(" ", Style::default().on(bg)));
                    used += w + 1;
                }
            }
            _ => {
                let label = truncate(label, inner);
                used = label.width();
                header.push(Seg::decor(label, Style::fg(t.dim).italic().on(bg)));
            }
        }
        let free = inner.saturating_sub(used);
        if free > hint.width() + 2 {
            header.push(Seg::decor(" ".repeat(free - hint.width()), Style::default().on(bg)));
            header.push(Seg::decor(hint, Style::fg(t.dim).on(bg)));
        }
        // The header and footer rows stand for the fences when copying.
        let at = |off: usize| src.map(|_| Src::span(off, off));
        let with_src =
            |segs: Vec<Seg>, s: Option<Src>| segs.into_iter().map(|g| Seg { src: s, ..g }).collect::<Vec<_>>();
        self.push(first, with_src(pad(header), src.and_then(|c| at(c.block.0))));
        let code = text.strip_suffix('\n').unwrap_or(text);
        let line_lens: Vec<usize> = code.split_inclusive('\n').map(str::len).collect();
        let mut line_off = 0;
        for (i, line) in self.env.hl.highlight(code, lang).into_iter().enumerate() {
            let mut off = line_off;
            let segs: Vec<Seg> = line
                .into_iter()
                .map(|(mut st, text)| {
                    st.fg = st.fg.or(Some(t.fg));
                    st.bg = Some(bg);
                    let seg_src = src
                        .and_then(|c| c.offset(off))
                        .map(|start| Src { exact: !text.contains('\t'), ..Src::span(start, start + text.len()) });
                    off += text.len();
                    Seg { src: seg_src, ..Seg::new(text.replace('\t', "    "), st) }
                })
                .collect();
            line_off += line_lens.get(i).copied().unwrap_or(0);
            for row in hard_wrap(&segs, inner) {
                self.push(rest, pad(row));
            }
        }
        self.push(rest, with_src(pad(Vec::new()), src.and_then(|c| at(c.block.1))));
        self.out.code_blocks.push(CodeSpan { start, end: self.out.lines.len(), text: code.to_string() });
    }

    #[allow(clippy::too_many_arguments)]
    fn table(
        &mut self,
        aligns: &[Alignment],
        cell_aligns: &[Vec<Alignment>],
        head: &[Vec<Inline>],
        rows: &[Vec<Vec<Inline>>],
        first: &[Seg],
        rest: &[Seg],
        width: usize,
    ) {
        let t = self.env.theme;
        let ncols = rows.iter().map(Vec::len).chain([head.len()]).max().unwrap_or(0);
        if ncols == 0 {
            return;
        }
        fn cells(row: &[Vec<Inline>], n: usize) -> Vec<&[Inline]> {
            (0..n).map(|i| row.get(i).map_or(&[][..], Vec::as_slice)).collect()
        }
        let head_cells = cells(head, ncols);
        let body_cells: Vec<_> = rows.iter().map(|r| cells(r, ncols)).collect();
        let head_style = Style::default().bold();

        let mut natural = vec![1usize; ncols];
        for (row, style) in
            std::iter::once((&head_cells, head_style)).chain(body_cells.iter().map(|r| (r, Style::default())))
        {
            for (i, cell) in row.iter().enumerate() {
                for line in self.cell_lines(cell, style, MAX_PLACEHOLDER_CELLS as usize) {
                    natural[i] = natural[i].max(line.iter().map(Seg::width).sum());
                }
            }
        }
        // Water-filling: columns narrower than an even share keep their
        // natural width; the rest split what's left.
        let mut widths = natural.clone();
        let mut budget = width.saturating_sub(3 * ncols + 1).max(ncols);
        let mut open: Vec<usize> = (0..ncols).collect();
        while !open.is_empty() {
            let share = budget / open.len();
            let (fit, wide): (Vec<usize>, Vec<usize>) = open.iter().partition(|&&i| natural[i] <= share);
            if fit.is_empty() {
                let extra = budget % wide.len();
                for (k, &i) in wide.iter().enumerate() {
                    widths[i] = (share + usize::from(k < extra)).max(1);
                }
                break;
            }
            for &i in &fit {
                budget -= natural[i];
            }
            open = wide;
        }

        let border = Style::fg(t.border);
        let hline = |l: &str, m: &str, r: &str, fill: &str| {
            let mut s = String::from(l);
            for (i, w) in widths.iter().enumerate() {
                s.push_str(&fill.repeat(w + 2));
                s.push_str(if i + 1 < widths.len() { m } else { r });
            }
            vec![Seg::new(s, border)]
        };
        let row_lines = |b: &Self, k: usize, row: &[&[Inline]], style: Style| -> Vec<Vec<Seg>> {
            let wrapped: Vec<Vec<Vec<Seg>>> =
                row.iter().zip(&widths).map(|(c, w)| b.cell_lines(c, style, *w)).collect();
            let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
            (0..height)
                .map(|y| {
                    let mut segs = vec![Seg::new("│", border)];
                    for (i, w) in widths.iter().enumerate() {
                        let line = wrapped[i].get(y).cloned().unwrap_or_default();
                        let used: usize = line.iter().map(Seg::width).sum();
                        let free = w.saturating_sub(used);
                        let cell = cell_aligns.get(k).and_then(|r| r.get(i)).filter(|a| **a != Alignment::None);
                        let (l, r) = match cell.or(aligns.get(i)) {
                            Some(Alignment::Right) => (free, 0),
                            Some(Alignment::Center) => (free / 2, free - free / 2),
                            _ => (0, free),
                        };
                        segs.push(Seg::new(" ".repeat(l + 1), Style::default()));
                        segs.extend(line);
                        segs.push(Seg::new(" ".repeat(r + 1), Style::default()));
                        segs.push(Seg::new("│", border));
                    }
                    segs
                })
                .collect()
        };

        self.push(first, hline("┌", "┬", "┐", "─"));
        if !head.is_empty() {
            for l in row_lines(self, 0, &head_cells, head_style) {
                self.push(rest, l);
            }
            self.push(rest, hline("╞", "╪", "╡", "═"));
        }
        let skip = usize::from(!head.is_empty());
        for (k, row) in body_cells.iter().enumerate() {
            for l in row_lines(self, k + skip, row, Style::default()) {
                self.push(rest, l);
            }
        }
        self.push(rest, hline("└", "┴", "┘", "─"));
    }

    /// Lines of a table cell `width` wide; images that fit are drawn.
    fn cell_lines(&self, inl: &[Inline], style: Style, width: usize) -> Vec<Vec<Seg>> {
        let mut lines = Vec::new();
        for piece in self.pieces(inl, style, width, true) {
            match piece {
                Piece::Segs(segs) if segs.is_empty() => {}
                Piece::Segs(segs) => lines.extend(wrap(&segs, width)),
                Piece::Image(cell, link, src) => lines.extend((0..cell.rows).map(|row| {
                    vec![Seg { link, src, image: Some(ImageCell { row, ..cell }), ..Seg::new("", Style::default()) }]
                })),
            }
        }
        if lines.is_empty() {
            lines.push(Vec::new());
        }
        lines
    }

    /// Converts inlines to styled segments. Images that are loaded become
    /// either inline cells (one row tall) or block pieces of their own.
    fn pieces(&self, inl: &[Inline], base: Style, width: usize, images: bool) -> Vec<Piece> {
        let t = self.env.theme;
        let mut out = Vec::new();
        let mut cur: Vec<Seg> = Vec::new();
        for i in inl {
            match i {
                Inline::Text { text, style, link, src } => cur.push(self.seg(text, *style, *link, *src, base)),
                Inline::Break => cur.push(Seg::new("\n", base)),
                Inline::Image { idx, link, src } => {
                    let src = *src;
                    let cell = if images { self.image_cell(*idx, width) } else { None };
                    let link = link.or(self.doc.images[*idx].self_link);
                    match cell {
                        Some(cell) if cell.rows == 1 => {
                            cur.push(Seg { image: Some(cell), link, src, ..Seg::new("", base) })
                        }
                        Some(cell) => {
                            out.push(Piece::Segs(std::mem::take(&mut cur)));
                            out.push(Piece::Image(cell, link, src));
                        }
                        None => {
                            let alt = &self.doc.images[*idx].alt;
                            let label = if alt.trim().is_empty() { "image" } else { alt.trim() };
                            let color = if self.is_broken(link) { t.caution } else { t.dim };
                            let mut st = Style::fg(color).italic();
                            st.underline = true;
                            cur.push(Seg {
                                link,
                                src: src.map(Src::inexact),
                                ..Seg::new(format!("▣\u{a0}{label}"), st)
                            });
                        }
                    }
                }
            }
        }
        out.push(Piece::Segs(cur));
        out
    }

    fn seg(&self, text: &str, s: InlineStyle, link: Option<usize>, src: Option<Src>, base: Style) -> Seg {
        let t = self.env.theme;
        let mut st = base;
        let mut text = text.to_string();
        st.bold |= s.bold;
        st.italic |= s.italic;
        st.strike |= s.strike;
        if s.code {
            st.fg = Some(t.code_fg);
            st.bg = Some(t.code_bg);
            text = format!("\u{a0}{text}\u{a0}");
        }
        if s.kbd {
            st.fg = Some(t.fg);
            st.bg = Some(t.border);
            st.bold = true;
            text = format!("\u{a0}{text}\u{a0}");
        }
        if s.math {
            st.italic = true;
            st.fg = Some(t.math);
        }
        // Short runs like `x²` or `H₂O` use Unicode super/subscripts; longer
        // text (`<sub>small print</sub>`) is dimmed instead.
        let small = if s.sup {
            superscript(&text)
        } else if s.sub {
            subscript(&text)
        } else {
            None
        };
        match small {
            Some(small) if !text.trim().contains(char::is_whitespace) => text = small,
            _ if s.sup || s.sub => st.fg = Some(t.dim),
            _ => {}
        }
        if link.is_some() {
            st.fg = Some(if self.is_broken(link) { t.caution } else { t.link });
            st.underline = true;
        }
        // Padded or remapped text no longer lines up byte for byte.
        let src = if s.code || s.kbd || s.sup || s.sub { src.map(Src::inexact) } else { src };
        Seg { link, src, ..Seg::new(text, st) }
    }

    fn is_broken(&self, link: Option<usize>) -> bool {
        link.and_then(|l| self.env.broken_links.get(l)).copied().unwrap_or(false)
    }

    fn image_cell(&self, idx: usize, avail: usize) -> Option<ImageCell> {
        let (w, h) = (self.env.image_dims)(idx)?;
        if w == 0 || h == 0 {
            return None;
        }
        let (cw, ch) = (self.env.cell.0 as f64, self.env.cell.1 as f64);
        let max = MAX_PLACEHOLDER_CELLS as f64;
        let avail = (avail as f64).min(max);
        let (w, h) = self.sized(idx, w as f64, h as f64, avail * cw);
        let cols_for_rows = |rows: f64| (rows * ch * w / h / cw).round().clamp(1.0, avail);
        let (cols, rows) = if h <= ch * 1.5 {
            (cols_for_rows(1.0), 1.0)
        } else {
            let cols = (w / cw).ceil().clamp(1.0, avail);
            let rows = (cols * cw * h / w / ch).round().max(2.0);
            let max_rows = (self.env.max_image_rows.max(4) as f64).min(max);
            if rows > max_rows { (cols_for_rows(max_rows), max_rows) } else { (cols, rows) }
        };
        Some(ImageCell { img: idx, cols: cols as u16, rows: rows as u16, row: 0 })
    }

    /// Applies an image's HTML `width`/`height` to its pixel size `w`×`h`.
    /// The attributes are in CSS pixels, while the cell size is in device
    /// pixels, so they're scaled by a guess at the display's pixel ratio
    /// (cells are rarely narrower than 6 or wider than 12 CSS pixels).
    fn sized(&self, idx: usize, w: f64, h: f64, avail_px: f64) -> (f64, f64) {
        let img = &self.doc.images[idx];
        let ratio = (self.env.cell.0 as f64 / 10.0).round().max(1.0);
        let px = |l: Length, of: f64| match l {
            Length::Px(v) => v * ratio,
            Length::Percent(p) => of * p / 100.0,
        };
        match (img.width, img.height) {
            (Some(lw), Some(Length::Px(_))) => (px(lw, avail_px), px(img.height.unwrap(), 0.0)),
            (Some(lw), _) => {
                let nw = px(lw, avail_px);
                (nw, h * nw / w)
            }
            (None, Some(lh @ Length::Px(_))) => {
                let nh = px(lh, 0.0);
                (w * nh / h, nh)
            }
            _ => (w, h),
        }
    }

    fn flow(&mut self, pieces: Vec<Piece>, first: &[Seg], rest: &[Seg], width: usize) {
        let mut used_first = false;
        for piece in pieces {
            match piece {
                Piece::Segs(segs) => {
                    let blank =
                        segs.iter().all(|s| s.image.is_none() && s.text.trim().is_empty() && !s.text.contains('\n'));
                    if blank {
                        continue;
                    }
                    for line in wrap(&segs, width) {
                        self.push(if used_first { rest } else { first }, line);
                        used_first = true;
                    }
                }
                Piece::Image(cell, link, src) => {
                    for row in 0..cell.rows {
                        let image = Some(ImageCell { row, ..cell });
                        let seg = Seg { link, image, src, ..Seg::new("", Style::default()) };
                        self.push(if used_first { rest } else { first }, vec![seg]);
                        used_first = true;
                    }
                }
            }
        }
        if !used_first {
            self.push(first, Vec::new());
        }
    }
}

/// Appends bytes `off..off + text.len()` of `like`'s text, merging with the
/// previous segment when style, link and source position line up.
fn append(cur: &mut Vec<Seg>, text: &str, like: &Seg, off: usize) {
    let src = like.src.map(|s| s.slice(off, text.len(), like.text.len()));
    if let Some(last) = cur.last_mut()
        && last.image.is_none()
        && last.style == like.style
        && last.link == like.link
        && last.decor == like.decor
        && let Some(joined) = Src::join(last.src, src)
    {
        last.text.push_str(text);
        last.src = joined;
        return;
    }
    cur.push(Seg { text: text.to_string(), image: None, src, ..like.clone() });
}

fn finish(lines: &mut Vec<Vec<Seg>>, cur: &mut Vec<Seg>) {
    while let Some(last) = cur.last_mut() {
        if last.image.is_some() {
            break;
        }
        let trimmed = last.text.trim_end_matches(' ').len();
        let removed = last.text.len() - trimmed;
        last.text.truncate(trimmed);
        if removed > 0
            && let Some(s) = last.src.as_mut().filter(|s| s.exact)
        {
            s.end -= removed;
            s.close = None;
        }
        if last.text.is_empty() {
            cur.pop();
        } else {
            break;
        }
    }
    lines.push(std::mem::take(cur));
}

/// Greedy word wrap that keeps styles and links. Words longer than the
/// width are broken at character boundaries.
pub fn wrap(segs: &[Seg], width: usize) -> Vec<Vec<Seg>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut cur: Vec<Seg> = Vec::new();
    let mut w = 0usize;
    for seg in segs {
        if let Some(cell) = seg.image {
            if w + cell.cols as usize > width && w > 0 {
                finish(&mut lines, &mut cur);
                w = 0;
            }
            cur.push(seg.clone());
            w += cell.cols as usize;
            continue;
        }
        let mut rest = seg.text.as_str();
        while !rest.is_empty() {
            let first = rest.chars().next().unwrap();
            let end = if first == '\n' {
                1
            } else if first == ' ' {
                rest.find(|c| c != ' ').unwrap_or(rest.len())
            } else {
                rest.find([' ', '\n']).unwrap_or(rest.len())
            };
            let tok_off = seg.text.len() - rest.len();
            let tok = &rest[..end];
            rest = &rest[end..];
            if first == '\n' {
                finish(&mut lines, &mut cur);
                w = 0;
            } else if first == ' ' {
                if w == 0 {
                    continue;
                }
                if w + tok.len() > width {
                    finish(&mut lines, &mut cur);
                    w = 0;
                } else {
                    append(&mut cur, tok, seg, tok_off);
                    w += tok.len();
                }
            } else {
                let tw = tok.width();
                if w + tw > width && w > 0 {
                    finish(&mut lines, &mut cur);
                    w = 0;
                }
                if tw <= width {
                    append(&mut cur, tok, seg, tok_off);
                    w += tw;
                } else {
                    for (i, ch) in tok.char_indices() {
                        let cw = ch.width().unwrap_or(0);
                        if w + cw > width && w > 0 {
                            finish(&mut lines, &mut cur);
                            w = 0;
                        }
                        append(&mut cur, ch.encode_utf8(&mut [0; 4]), seg, tok_off + i);
                        w += cw;
                    }
                }
            }
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Character-level wrap for code, which must keep its whitespace.
fn hard_wrap(segs: &[Seg], width: usize) -> Vec<Vec<Seg>> {
    let mut lines = vec![Vec::new()];
    let mut w = 0;
    for seg in segs {
        for (i, ch) in seg.text.char_indices() {
            let cw = ch.width().unwrap_or(0);
            if w + cw > width {
                lines.push(Vec::new());
                w = 0;
            }
            append(lines.last_mut().unwrap(), ch.encode_utf8(&mut [0; 4]), seg, i);
            w += cw;
        }
    }
    lines
}

pub fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw + 1 > width {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

fn map_chars(s: &str, from: &str, to: &str) -> Option<String> {
    s.chars().map(|c| from.chars().position(|f| f == c).and_then(|i| to.chars().nth(i))).collect()
}

fn superscript(s: &str) -> Option<String> {
    map_chars(s, "0123456789+-=()abcdefghijklmnoprstuvwxyz ", "⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ᵃᵇᶜᵈᵉᶠᵍʰⁱʲᵏˡᵐⁿᵒᵖʳˢᵗᵘᵛʷˣʸᶻ ")
}

fn subscript(s: &str) -> Option<String> {
    map_chars(s, "0123456789+-=()aehijklmnoprstuvx ", "₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎ₐₑₕᵢⱼₖₗₘₙₒₚᵣₛₜᵤᵥₓ ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Vec<Seg>]) -> Vec<String> {
        lines.iter().map(|l| l.iter().map(|s| s.text.as_str()).collect()).collect()
    }

    #[test]
    fn wraps_words_and_keeps_styles() {
        let bold = Style::default().bold();
        let segs = vec![
            Seg::new("hello ", Style::default()),
            Seg::new("brave new", bold),
            Seg::new(" world", Style::default()),
        ];
        let lines = wrap(&segs, 11);
        assert_eq!(texts(&lines), ["hello brave", "new world"]);
        assert_eq!(lines[1][0].style, bold);
    }

    #[test]
    fn breaks_long_words() {
        let lines = wrap(&[Seg::new("abcdefghij", Style::default())], 4);
        assert_eq!(texts(&lines), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn superscripts() {
        assert_eq!(superscript("2").as_deref(), Some("²"));
        assert_eq!(superscript("x!"), None);
    }
}
