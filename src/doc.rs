//! Markdown parsing into a small document model that the layout engine walks.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use std::ops::Range;

use crate::convert::{Format, Variants};
use pulldown_cmark::{Alignment, BlockQuoteKind, CodeBlockKind, Event, OffsetIter, Options, Parser, Tag, TagEnd};

/// Where a piece of rendered text came from in the Markdown source (byte
/// offsets), so copying a selection can return the source instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Src {
    pub start: usize,
    pub end: usize,
    /// The text has the same bytes as `source[start..end]`, so positions
    /// inside it map 1:1. Otherwise only the span as a whole is known.
    pub exact: bool,
    /// Used instead of `start`/`end` when a selection begins at the first
    /// (or ends at the last) character, to take enclosing markup with it:
    /// `**`, backticks, `[`…`](url)`.
    pub open: Option<usize>,
    pub close: Option<usize>,
}

impl Src {
    pub fn span(start: usize, end: usize) -> Src {
        Src { start, end, exact: false, open: None, close: None }
    }

    /// Forgets the 1:1 mapping, for text that was transformed for display.
    pub fn inexact(self) -> Src {
        Src { exact: false, ..self }
    }

    /// The part covering bytes `off..off + len` of a text `total` bytes long.
    pub fn slice(self, off: usize, len: usize, total: usize) -> Src {
        let open = if off == 0 { self.open } else { None };
        let close = if off + len == total { self.close } else { None };
        if self.exact {
            Src { start: self.start + off, end: self.start + off + len, exact: true, open, close }
        } else {
            Src { open, close, ..self }
        }
    }

    /// Joins two adjacent spans, if they can be: both 1:1 and contiguous,
    /// or the same opaque span. The outer `None` means they can't.
    pub fn join(a: Option<Src>, b: Option<Src>) -> Option<Option<Src>> {
        match (a, b) {
            (None, None) => Some(None),
            (Some(a), Some(b)) if a.exact && b.exact && a.end == b.start && b.open.is_none() => {
                Some(Some(Src { end: b.end, close: b.close, ..a }))
            }
            (Some(a), Some(b)) if !a.exact && a == b => Some(Some(a)),
            _ => None,
        }
    }

    /// Source offset where a selection starting at byte `off` begins.
    pub fn char_start(&self, off: usize) -> usize {
        if self.exact && off > 0 { self.start + off } else { self.open.unwrap_or(self.start) }
    }

    /// Source offset where a selection ending with the char at `off..off + len` ends.
    pub fn char_end(&self, off: usize, len: usize, total: usize) -> usize {
        if !self.exact || off + len >= total {
            self.close.unwrap_or(if self.exact { self.start + off + len } else { self.end })
        } else {
            self.start + off + len
        }
    }
}

/// Source positions of a code block: the whole block (fences included) and,
/// per text chunk, `(offset in the code text, offset in the source)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CodeSrc {
    pub block: (usize, usize),
    pub chunks: Vec<(usize, usize)>,
}

impl CodeSrc {
    /// Source offset of byte `off` of the code text.
    pub fn offset(&self, off: usize) -> Option<usize> {
        let i = self.chunks.partition_point(|(t, _)| *t <= off).checked_sub(1)?;
        let (t, s) = self.chunks[i];
        Some(s + off - t)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub math: bool,
    pub sup: bool,
    pub sub: bool,
    pub kbd: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Inline {
    Text {
        text: String,
        style: InlineStyle,
        link: Option<usize>,
        src: Option<Src>,
    },
    /// Index into `Doc::images`; `link` is set when the image is wrapped in a link.
    Image {
        idx: usize,
        link: Option<usize>,
        src: Option<Src>,
    },
    Break,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageRef {
    pub url: String,
    pub alt: String,
    /// Link id pointing at the image itself, so it can be opened.
    pub self_link: Option<usize>,
    /// Diagram source for images rendered from Mermaid code blocks.
    pub mermaid: Option<String>,
    /// Size from HTML `width`/`height` attributes.
    pub width: Option<Length>,
    pub height: Option<Length>,
}

/// An HTML size: CSS pixels or a percentage of the available width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    Px(f64),
    Percent(f64),
}

impl Length {
    fn parse(s: &str) -> Option<Length> {
        let s = s.trim();
        let (num, pct) = match s.strip_suffix('%') {
            Some(n) => (n, true),
            None => (s.strip_suffix("px").unwrap_or(s), false),
        };
        let v: f64 = num.trim().parse().ok().filter(|v: &f64| *v > 0.0)?;
        Some(if pct { Length::Percent(v) } else { Length::Px(v) })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        inlines: Vec<Inline>,
        anchor: String,
    },
    Paragraph(Vec<Inline>),
    /// Text in a tight list item, not wrapped in a paragraph.
    Plain(Vec<Inline>),
    Code {
        lang: String,
        text: String,
        src: CodeSrc,
        /// Distinguishes blocks, e.g. to remember which format one is shown in.
        id: usize,
        /// JSON/YAML/TOML blocks that parse, with their conversions.
        variants: Option<Variants>,
    },
    /// A Mermaid code block, rendered as the image `img`.
    Mermaid {
        img: usize,
        source: String,
        src: CodeSrc,
    },
    Quote {
        kind: Option<BlockQuoteKind>,
        blocks: Vec<Block>,
    },
    List {
        start: Option<u64>,
        items: Vec<Item>,
    },
    Table {
        aligns: Vec<Alignment>,
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
        /// Per-cell alignment from HTML tables, header row first.
        cell_aligns: Vec<Vec<Alignment>>,
    },
    Rule(usize, usize),
    Footnote {
        label: String,
        blocks: Vec<Block>,
    },
    DefTitle(Vec<Inline>),
    DefBody(Vec<Block>),
    FrontMatter(String, CodeSrc),
    /// Blocks inside HTML with `align="center"` or `<center>`.
    Center(Vec<Block>),
}

#[derive(Clone, Debug, Default)]
pub struct Doc {
    pub blocks: Vec<Block>,
    /// Link targets, indexed by the `link` ids in inlines.
    pub links: Vec<String>,
    pub images: Vec<ImageRef>,
}

pub fn parse(src: &str) -> Doc {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT;
    let mut p = P {
        ev: Parser::new_ext(src, opts).into_offset_iter(),
        source: src,
        range: 0..0,
        pending_open: None,
        html_src: None,
        code_blocks: 0,
        doc: Doc::default(),
        depth: Depths::default(),
        links: Vec::new(),
        tasks: Vec::new(),
        slugs: HashMap::new(),
    };
    let blocks = p.blocks(&|_| false);
    p.doc.blocks = blocks;
    p.doc
}

/// Nesting counters for inline styles (they can't overlap improperly in
/// valid event streams, so counters are enough).
#[derive(Clone, Default)]
struct Depths {
    bold: u32,
    italic: u32,
    strike: u32,
    sup: u32,
    sub: u32,
    code: u32,
    kbd: u32,
}

struct P<'a> {
    ev: OffsetIter<'a>,
    source: &'a str,
    /// Source range of the event last returned by `next`.
    range: Range<usize>,
    /// Start of an inline container (emphasis, link, …) whose first text
    /// hasn't been pushed yet.
    pending_open: Option<usize>,
    /// Source span for text produced from raw HTML, which has no finer map.
    html_src: Option<Src>,
    /// Code blocks seen so far, for their ids.
    code_blocks: usize,
    doc: Doc,
    depth: Depths,
    links: Vec<usize>,
    tasks: Vec<Option<bool>>,
    slugs: HashMap<String, usize>,
}

impl<'a> P<'a> {
    fn next(&mut self) -> Option<Event<'a>> {
        let (e, r) = self.ev.next()?;
        self.range = r;
        Some(e)
    }

    fn style(&self) -> InlineStyle {
        let d = &self.depth;
        InlineStyle {
            bold: d.bold > 0,
            italic: d.italic > 0,
            strike: d.strike > 0,
            code: d.code > 0,
            math: false,
            sup: d.sup > 0,
            sub: d.sub > 0,
            kbd: d.kbd > 0,
        }
    }

    fn add_link(&mut self, target: String) -> usize {
        self.doc.links.push(target);
        self.doc.links.len() - 1
    }

    fn blocks(&mut self, stop: &dyn Fn(&TagEnd) -> bool) -> Vec<Block> {
        let mut out = Vec::new();
        let mut loose = Vec::new();
        // Containers like `<div align="center">` left open by an HTML block,
        // with where their content starts in `out` when they center it.
        let mut open: Vec<Option<usize>> = Vec::new();
        let flush = |loose: &mut Vec<Inline>, out: &mut Vec<Block>| {
            if !loose.is_empty() {
                out.push(Block::Plain(std::mem::take(loose)));
            }
        };
        while let Some(e) = self.next() {
            match e {
                Event::End(t) if stop(&t) => break,
                Event::Start(Tag::Paragraph) => {
                    flush(&mut loose, &mut out);
                    let inl = self.inlines(&|t| matches!(t, TagEnd::Paragraph));
                    out.push(Block::Paragraph(inl));
                }
                Event::Start(Tag::Heading { level, id, .. }) => {
                    flush(&mut loose, &mut out);
                    let inlines = self.inlines(&|t| matches!(t, TagEnd::Heading(_)));
                    let anchor = match id {
                        Some(id) => id.to_string(),
                        None => self.unique_slug(&plain_text(&inlines, &self.doc.images)),
                    };
                    out.push(Block::Heading { level: level as u8, inlines, anchor });
                }
                Event::Start(Tag::BlockQuote(kind)) => {
                    flush(&mut loose, &mut out);
                    let blocks = self.blocks(&|t| matches!(t, TagEnd::BlockQuote(_)));
                    out.push(Block::Quote { kind, blocks });
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    flush(&mut loose, &mut out);
                    let block = (self.range.start, self.range.end);
                    let lang = match kind {
                        CodeBlockKind::Fenced(info) => info
                            .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
                            .next()
                            .unwrap_or("")
                            .to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    let (text, chunks) = self.collect_code(&|t| matches!(t, TagEnd::CodeBlock));
                    if lang.eq_ignore_ascii_case("mermaid") {
                        out.push(self.mermaid(text, CodeSrc { block, chunks }));
                    } else {
                        let id = self.code_blocks;
                        self.code_blocks += 1;
                        let variants = Format::from_lang(&lang).and_then(|f| Variants::new(f, &text));
                        out.push(Block::Code { lang, text, src: CodeSrc { block, chunks }, id, variants });
                    }
                }
                Event::Start(Tag::List(start)) => {
                    flush(&mut loose, &mut out);
                    let mut items = Vec::new();
                    loop {
                        match self.next() {
                            Some(Event::Start(Tag::Item)) => {
                                self.tasks.push(None);
                                let blocks = self.blocks(&|t| matches!(t, TagEnd::Item));
                                let task = self.tasks.pop().flatten();
                                items.push(Item { task, blocks });
                            }
                            Some(Event::End(TagEnd::List(_))) | None => break,
                            _ => {}
                        }
                    }
                    out.push(Block::List { start, items });
                }
                Event::Start(Tag::Table(aligns)) => {
                    flush(&mut loose, &mut out);
                    out.push(self.table(aligns));
                }
                Event::Start(Tag::HtmlBlock) => {
                    flush(&mut loose, &mut out);
                    let span = Src::span(self.range.start, self.range.end);
                    let html = self.collect_text(&|t| matches!(t, TagEnd::HtmlBlock));
                    self.html_block(&html, span, &mut out, &mut open);
                }
                Event::Start(Tag::FootnoteDefinition(label)) => {
                    flush(&mut loose, &mut out);
                    let blocks = self.blocks(&|t| matches!(t, TagEnd::FootnoteDefinition));
                    out.push(Block::Footnote { label: label.to_string(), blocks });
                }
                Event::Start(Tag::MetadataBlock(_)) => {
                    flush(&mut loose, &mut out);
                    let block = (self.range.start, self.range.end);
                    let (text, chunks) = self.collect_code(&|t| matches!(t, TagEnd::MetadataBlock(_)));
                    out.push(Block::FrontMatter(text, CodeSrc { block, chunks }));
                }
                Event::Start(Tag::DefinitionList) => {
                    flush(&mut loose, &mut out);
                    let blocks = self.blocks(&|t| matches!(t, TagEnd::DefinitionList));
                    out.extend(blocks);
                }
                Event::Start(Tag::DefinitionListTitle) => {
                    flush(&mut loose, &mut out);
                    let inl = self.inlines(&|t| matches!(t, TagEnd::DefinitionListTitle));
                    out.push(Block::DefTitle(inl));
                }
                Event::Start(Tag::DefinitionListDefinition) => {
                    flush(&mut loose, &mut out);
                    let blocks = self.blocks(&|t| matches!(t, TagEnd::DefinitionListDefinition));
                    out.push(Block::DefBody(blocks));
                }
                Event::Rule => {
                    flush(&mut loose, &mut out);
                    out.push(Block::Rule(self.range.start, self.range.end));
                }
                e => self.inline_event(e, &mut loose),
            }
        }
        flush(&mut loose, &mut out);
        while let Some(start) = open.pop() {
            center_from(&mut out, start);
        }
        out
    }

    /// Like `collect_text`, also recording where each chunk sits in the source.
    fn collect_code(&mut self, stop: &dyn Fn(&TagEnd) -> bool) -> (String, Vec<(usize, usize)>) {
        let mut s = String::new();
        let mut chunks = Vec::new();
        while let Some(e) = self.next() {
            match e {
                Event::Text(t) => {
                    chunks.push((s.len(), self.range.start));
                    s.push_str(&t);
                }
                Event::End(t) if stop(&t) => break,
                _ => {}
            }
        }
        (s, chunks)
    }

    fn collect_text(&mut self, stop: &dyn Fn(&TagEnd) -> bool) -> String {
        let mut s = String::new();
        while let Some(e) = self.next() {
            match e {
                Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) | Event::Code(t) => s.push_str(&t),
                Event::SoftBreak | Event::HardBreak => s.push('\n'),
                Event::End(t) if stop(&t) => break,
                _ => {}
            }
        }
        s
    }

    fn inlines(&mut self, stop: &dyn Fn(&TagEnd) -> bool) -> Vec<Inline> {
        let mut out = Vec::new();
        while let Some(e) = self.next() {
            match e {
                Event::End(t) if stop(&t) => break,
                e => self.inline_event(e, &mut out),
            }
        }
        out
    }

    fn table(&mut self, aligns: Vec<Alignment>) -> Block {
        let mut head = Vec::new();
        let mut rows = Vec::new();
        let mut row = Vec::new();
        loop {
            match self.next() {
                Some(Event::Start(Tag::TableHead)) | Some(Event::Start(Tag::TableRow)) => row = Vec::new(),
                Some(Event::End(TagEnd::TableHead)) => head = std::mem::take(&mut row),
                Some(Event::End(TagEnd::TableRow)) => rows.push(std::mem::take(&mut row)),
                Some(Event::Start(Tag::TableCell)) => row.push(self.inlines(&|t| matches!(t, TagEnd::TableCell))),
                Some(Event::End(TagEnd::Table)) | None => break,
                _ => {}
            }
        }
        Block::Table { aligns, head, rows, cell_aligns: Vec::new() }
    }

    /// Source span of the current event's text, which pulldown-cmark gives
    /// verbatim for plain text but wrapped in delimiters for code and math.
    fn text_src(&mut self, text: &str) -> Src {
        let r = self.range.clone();
        let raw = &self.source[r.clone()];
        let open = self.pending_open.take();
        if raw == text {
            return Src { start: r.start, end: r.end, exact: true, open, close: None };
        }
        match raw.find(text) {
            Some(i) if !text.is_empty() => Src {
                start: r.start + i,
                end: r.start + i + text.len(),
                exact: true,
                open: open.or(Some(r.start)),
                close: Some(r.end),
            },
            _ => Src { open: open.or(Some(r.start)), close: Some(r.end), ..Src::span(r.start, r.end) },
        }
    }

    fn push_text(&self, text: &str, style: InlineStyle, out: &mut Vec<Inline>, src: Option<Src>) {
        if text.is_empty() {
            return;
        }
        let clean: String = text.chars().map(|c| if c == '\t' { ' ' } else { c }).filter(|c| !c.is_control()).collect();
        let mut src = self.html_src.or(src);
        if clean.len() != text.len() {
            src = src.map(Src::inexact);
        }
        let link = self.links.last().copied();
        if let Some(Inline::Text { text: prev, style: s, link: l, src: prev_src }) = out.last_mut()
            && *s == style
            && *l == link
            && let Some(merged) = Src::join(*prev_src, src)
        {
            prev.push_str(&clean);
            *prev_src = merged;
            return;
        }
        out.push(Inline::Text { text: clean, style, link, src });
    }

    /// Records where the container just closed ends, on the last inline.
    fn close_container(&self, out: &mut [Inline]) {
        let end = self.range.end;
        if let Some(Inline::Text { src: Some(s), .. } | Inline::Image { src: Some(s), .. }) = out.last_mut() {
            s.close = Some(s.close.map_or(end, |c| c.max(end)));
        }
    }

    fn inline_event(&mut self, e: Event, out: &mut Vec<Inline>) {
        match e {
            Event::Text(t) => {
                let src = self.text_src(&t);
                self.push_text(&t, self.style(), out, Some(src));
            }
            Event::Code(t) => {
                let style = InlineStyle { code: true, ..self.style() };
                let src = self.text_src(&t);
                self.push_text(&t, style, out, Some(src));
            }
            Event::InlineMath(t) => {
                let style = InlineStyle { math: true, ..self.style() };
                let src = self.text_src(&t);
                self.push_text(&t, style, out, Some(src));
            }
            Event::DisplayMath(t) => {
                if !out.is_empty() {
                    out.push(Inline::Break);
                }
                let style = InlineStyle { math: true, ..self.style() };
                let src = self.text_src(&t).inexact();
                for (i, line) in t.trim().lines().enumerate() {
                    if i > 0 {
                        out.push(Inline::Break);
                    }
                    self.push_text(&format!("  {line}"), style, out, Some(src));
                }
                out.push(Inline::Break);
            }
            Event::Html(h) | Event::InlineHtml(h) => {
                let outer = self.html_src;
                self.html_src = outer.or(Some(Src::span(self.range.start, self.range.end)));
                self.html(&h, out);
                self.html_src = outer;
            }
            Event::FootnoteReference(label) => {
                let id = self.add_link(format!("#fn-{label}"));
                let src = Src { open: self.pending_open.take(), ..Src::span(self.range.start, self.range.end) };
                out.push(Inline::Text {
                    text: format!("[{label}]"),
                    style: self.style(),
                    link: Some(id),
                    src: Some(src),
                });
            }
            Event::SoftBreak => {
                let r = self.range.clone();
                let src = Src { exact: r.len() == 1, ..Src::span(r.start, r.end) };
                self.push_text(" ", self.style(), out, Some(src));
            }
            Event::HardBreak => out.push(Inline::Break),
            Event::TaskListMarker(done) => {
                if let Some(t) = self.tasks.last_mut() {
                    *t = Some(done);
                }
            }
            Event::Start(tag) => {
                if matches!(
                    tag,
                    Tag::Emphasis
                        | Tag::Strong
                        | Tag::Strikethrough
                        | Tag::Superscript
                        | Tag::Subscript
                        | Tag::Link { .. }
                ) {
                    self.pending_open.get_or_insert(self.range.start);
                }
                match tag {
                    Tag::Emphasis => self.depth.italic += 1,
                    Tag::Strong => self.depth.bold += 1,
                    Tag::Strikethrough => self.depth.strike += 1,
                    Tag::Superscript => self.depth.sup += 1,
                    Tag::Subscript => self.depth.sub += 1,
                    Tag::Link { dest_url, .. } => {
                        let id = self.add_link(dest_url.to_string());
                        self.links.push(id);
                    }
                    Tag::Image { dest_url, .. } => {
                        let src = Src { open: self.pending_open.take(), ..Src::span(self.range.start, self.range.end) };
                        let alt = self.collect_text(&|t| matches!(t, TagEnd::Image));
                        self.push_image(dest_url.to_string(), alt, out, Some(src));
                    }
                    _ => {}
                }
            }
            Event::End(tag) => {
                if matches!(
                    tag,
                    TagEnd::Emphasis
                        | TagEnd::Strong
                        | TagEnd::Strikethrough
                        | TagEnd::Superscript
                        | TagEnd::Subscript
                        | TagEnd::Link
                ) {
                    self.close_container(out);
                }
                match tag {
                    TagEnd::Emphasis => self.depth.italic = self.depth.italic.saturating_sub(1),
                    TagEnd::Strong => self.depth.bold = self.depth.bold.saturating_sub(1),
                    TagEnd::Strikethrough => self.depth.strike = self.depth.strike.saturating_sub(1),
                    TagEnd::Superscript => self.depth.sup = self.depth.sup.saturating_sub(1),
                    TagEnd::Subscript => self.depth.sub = self.depth.sub.saturating_sub(1),
                    TagEnd::Link => {
                        self.links.pop();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn push_image(&mut self, url: String, alt: String, out: &mut Vec<Inline>, src: Option<Src>) {
        let self_link = Some(self.add_link(url.clone()));
        self.doc.images.push(ImageRef { url, alt, self_link, mermaid: None, width: None, height: None });
        let src = self.html_src.or(src);
        out.push(Inline::Image { idx: self.doc.images.len() - 1, link: self.links.last().copied(), src });
    }

    fn mermaid(&mut self, source: String, src: CodeSrc) -> Block {
        let mut hasher = DefaultHasher::new();
        source.hash(&mut hasher);
        self.doc.images.push(ImageRef {
            url: format!("mermaid:{:016x}", hasher.finish()),
            alt: "Mermaid diagram".into(),
            self_link: None,
            mermaid: Some(source.clone()),
            width: None,
            height: None,
        });
        Block::Mermaid { img: self.doc.images.len() - 1, source, src }
    }

    /// Handles raw HTML: common formatting tags, `<img>`, `<a>`, and `<br>`
    /// are interpreted, other tags dropped, and text between tags kept.
    fn html(&mut self, html: &str, out: &mut Vec<Inline>) {
        let mut rest = html;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("<!--") {
                rest = after.find("-->").map(|i| &after[i + 3..]).unwrap_or("");
                continue;
            }
            if rest.starts_with('<')
                && let Some(end) = rest.find('>')
            {
                let tag = &rest[1..end];
                rest = &rest[end + 1..];
                let name = tag_name(tag);
                if (name == "script" || name == "style") && !tag.starts_with('/') {
                    let close = format!("</{name}");
                    rest = rest.to_ascii_lowercase().find(&close).map_or("", |i| &rest[i..]);
                    continue;
                }
                self.html_tag(tag, out);
                continue;
            }
            let skip = rest.chars().next().map_or(1, char::len_utf8);
            let next = rest[skip..].find('<').map(|i| i + skip).unwrap_or(rest.len());
            let text = decode_entities(&rest[..next]);
            let collapsed = collapse_ws(&text);
            if !collapsed.trim().is_empty() || (!collapsed.is_empty() && !out.is_empty()) {
                self.push_text(&collapsed, self.style(), out, None);
            }
            rest = &rest[next..];
        }
    }

    fn html_tag(&mut self, tag: &str, out: &mut Vec<Inline>) {
        let closing = tag.starts_with('/');
        let name = tag_name(tag);
        let d = &mut self.depth;
        let counter = match name.as_str() {
            "b" | "strong" => Some(&mut d.bold),
            "i" | "em" => Some(&mut d.italic),
            "s" | "del" | "strike" => Some(&mut d.strike),
            "sup" => Some(&mut d.sup),
            "sub" => Some(&mut d.sub),
            "code" | "tt" => Some(&mut d.code),
            "kbd" => Some(&mut d.kbd),
            _ => None,
        };
        if let Some(c) = counter {
            *c = if closing { c.saturating_sub(1) } else { *c + 1 };
            return;
        }
        match name.as_str() {
            "br" | "hr" => out.push(Inline::Break),
            "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "summary" | "li" | "tr" => {
                if closing && !matches!(out.last(), Some(Inline::Break) | None) {
                    out.push(Inline::Break);
                }
            }
            "img" if !closing => {
                if let Some(src) = attr(tag, "src") {
                    let alt = attr(tag, "alt").unwrap_or_default();
                    self.push_image(src, alt, out, None);
                    if let Some(img) = self.doc.images.last_mut() {
                        img.width = attr(tag, "width").and_then(|v| Length::parse(&v));
                        img.height = attr(tag, "height").and_then(|v| Length::parse(&v));
                    }
                }
            }
            "input" if attr(tag, "type").is_some_and(|t| t.eq_ignore_ascii_case("checkbox")) => {
                let checked = has_attr(tag, "checked");
                self.push_text(if checked { "☑" } else { "☐" }, self.style(), out, None);
            }
            "a" if closing => {
                self.links.pop();
            }
            "a" => {
                let id = self.add_link(attr(tag, "href").unwrap_or_default());
                self.links.push(id);
            }
            _ => {}
        }
    }

    /// Turns a raw HTML block into blocks (headings, paragraphs, tables,
    /// lists, …), keeping `align="center"`. A container left open, like
    /// GitHub's `<div align="center">` around Markdown, is recorded in `open`
    /// so the blocks up to its closing tag can be centered too.
    fn html_block(&mut self, html: &str, span: Src, out: &mut Vec<Block>, open: &mut Vec<Option<usize>>) {
        let tree = html_tree(html);
        for _ in 0..tree.stray_closes {
            if let Some(start) = open.pop() {
                center_from(out, start);
            }
        }
        let depth = self.depth.clone();
        let links = self.links.len();
        self.html_src = Some(span);
        let (els, tail) = split_open_tail(&tree.els);
        let end = tail.map_or(html.len(), |t| t.outer.start);
        let blocks = self.html_children(html, els, 0..end);
        out.extend(blocks);
        if let Some(t) = tail {
            self.html_open(html, t, out, open);
        }
        self.html_src = None;
        self.depth = depth;
        self.links.truncate(links);
    }

    /// Handles an element whose closing tag comes in a later block.
    fn html_open(&mut self, html: &str, el: &El, out: &mut Vec<Block>, open: &mut Vec<Option<usize>>) {
        open.push(is_centered(el).then_some(out.len()));
        let (els, tail) = split_open_tail(&el.children);
        let end = tail.map_or(el.inner.end, |t| t.outer.start);
        let blocks = self.html_children(html, els, el.inner.start..end);
        out.extend(blocks);
        if let Some(t) = tail {
            self.html_open(html, t, out, open);
        }
    }

    /// Converts the content in `range`, whose child elements are `els`.
    fn html_children(&mut self, html: &str, els: &[El], range: Range<usize>) -> Vec<Block> {
        let mut out = Vec::new();
        let mut pos = range.start;
        for el in els.iter().filter(|e| BLOCK_TAGS.contains(&e.name.as_str())) {
            self.html_paragraph(&html[pos..el.outer.start], &mut out);
            self.html_element(html, el, &mut out);
            pos = el.outer.end;
        }
        self.html_paragraph(&html[pos..range.end], &mut out);
        out
    }

    fn html_inlines(&mut self, html: &str) -> Vec<Inline> {
        let mut inl = Vec::new();
        self.html(html, &mut inl);
        trim_breaks(&mut inl);
        if let Some(Inline::Text { text, .. }) = inl.last_mut() {
            text.truncate(text.trim_end().len());
            if text.is_empty() {
                inl.pop();
            }
        }
        inl
    }

    fn html_paragraph(&mut self, html: &str, out: &mut Vec<Block>) {
        let inl = self.html_inlines(html);
        let content = inl.iter().any(|i| match i {
            Inline::Text { text, .. } => !text.trim().is_empty(),
            Inline::Image { .. } => true,
            Inline::Break => false,
        });
        if content {
            out.push(Block::Paragraph(inl));
        }
    }

    fn html_element(&mut self, html: &str, el: &El, out: &mut Vec<Block>) {
        let inner = &html[el.inner.clone()];
        let name = el.name.as_str();
        let blocks = match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let inlines = self.html_inlines(inner);
                let anchor = match attr(el.tag, "id") {
                    Some(id) => id,
                    None => self.unique_slug(&plain_text(&inlines, &self.doc.images)),
                };
                vec![Block::Heading { level: name.as_bytes()[1] - b'0', inlines, anchor }]
            }
            "hr" => {
                let span = self.html_src.unwrap_or(Src::span(0, 0));
                vec![Block::Rule(span.start, span.end)]
            }
            "pre" => vec![self.html_pre(html, el)],
            "table" => vec![self.html_table(html, el)],
            "ul" | "ol" => vec![self.html_list(html, el)],
            "blockquote" => {
                vec![Block::Quote { kind: None, blocks: self.html_children(html, &el.children, el.inner.clone()) }]
            }
            "dt" => vec![Block::DefTitle(self.html_inlines(inner))],
            "dd" => vec![Block::DefBody(self.html_children(html, &el.children, el.inner.clone()))],
            // Details are always shown open.
            "summary" => {
                self.depth.bold += 1;
                let mut inl = self.html_inlines(inner);
                self.depth.bold -= 1;
                inl.insert(0, Inline::Text { text: "▾ ".into(), style: InlineStyle::default(), link: None, src: None });
                vec![Block::Paragraph(inl)]
            }
            _ => self.html_children(html, &el.children, el.inner.clone()),
        };
        if is_centered(el) && !blocks.is_empty() {
            out.push(Block::Center(blocks));
        } else {
            out.extend(blocks);
        }
    }

    fn html_pre(&mut self, html: &str, el: &El) -> Block {
        let lang = el
            .children
            .iter()
            .find(|c| c.name == "code")
            .and_then(|c| attr(c.tag, "class"))
            .or_else(|| attr(el.tag, "lang"))
            .and_then(|class| {
                let mut words = class.split_whitespace();
                let tagged =
                    words.clone().find_map(|c| c.strip_prefix("language-").or_else(|| c.strip_prefix("lang-")));
                tagged.or_else(|| words.next()).map(str::to_string)
            })
            .unwrap_or_default();
        let raw = decode_entities(&strip_tags(&html[el.inner.clone()]));
        let mut text = raw.strip_prefix('\n').unwrap_or(&raw).to_string();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        let span = self.html_src.unwrap_or(Src::span(0, 0));
        let id = self.code_blocks;
        self.code_blocks += 1;
        let variants = Format::from_lang(&lang).and_then(|f| Variants::new(f, &text));
        Block::Code { lang, text, src: CodeSrc { block: (span.start, span.end), chunks: Vec::new() }, id, variants }
    }

    fn html_list(&mut self, html: &str, el: &El) -> Block {
        let start = (el.name == "ol").then(|| attr(el.tag, "start").and_then(|s| s.trim().parse().ok()).unwrap_or(1));
        let mut items = Vec::new();
        for li in el.children.iter().filter(|c| c.name == "li") {
            let mut blocks = self.html_children(html, &li.children, li.inner.clone());
            for b in &mut blocks {
                if let Block::Paragraph(inl) = b {
                    *b = Block::Plain(std::mem::take(inl));
                }
            }
            items.push(Item { task: None, blocks });
        }
        Block::List { start, items }
    }

    fn html_table(&mut self, html: &str, el: &El) -> Block {
        fn rows<'e, 'h>(els: &'e [El<'h>], head: bool, out: &mut Vec<(&'e El<'h>, bool)>) {
            for e in els {
                match e.name.as_str() {
                    "tr" => out.push((e, head)),
                    "thead" => rows(&e.children, true, out),
                    "tbody" | "tfoot" => rows(&e.children, false, out),
                    _ => {}
                }
            }
        }
        let mut trs = Vec::new();
        rows(&el.children, false, &mut trs);
        let cells = |tr: &El<'_>| -> Vec<(Range<usize>, Alignment, bool)> {
            tr.children
                .iter()
                .filter(|c| c.name == "td" || c.name == "th")
                .map(|c| (c.inner.clone(), cell_align(c), c.name == "th"))
                .collect()
        };
        let grid: Vec<_> = trs.iter().map(|(tr, head)| (cells(tr), *head)).collect();
        // The first row is the header when it's in <thead> or all <th>.
        let header = grid.first().is_some_and(|(c, head)| *head || (!c.is_empty() && c.iter().all(|x| x.2)));
        let ncols = grid.iter().map(|(c, _)| c.len()).max().unwrap_or(0);
        let cell_aligns = grid.iter().map(|(c, _)| c.iter().map(|x| x.1).collect()).collect();
        let mut head = Vec::new();
        let mut body = Vec::new();
        for (k, (row, _)) in grid.iter().enumerate() {
            let mut out = Vec::new();
            for (range, _, _) in row {
                out.push(self.html_inlines(&html[range.clone()]));
            }
            if k == 0 && header {
                head = out;
            } else {
                body.push(out);
            }
        }
        Block::Table { aligns: vec![Alignment::None; ncols], head, rows: body, cell_aligns }
    }

    fn unique_slug(&mut self, text: &str) -> String {
        let base = slugify(text);
        let n = self.slugs.entry(base.clone()).or_insert(0);
        let slug = if *n == 0 { base } else { format!("{base}-{n}") };
        *n += 1;
        slug
    }
}

fn trim_breaks(inl: &mut Vec<Inline>) {
    while matches!(inl.last(), Some(Inline::Break)) {
        inl.pop();
    }
    while matches!(inl.first(), Some(Inline::Break)) {
        inl.remove(0);
    }
}

/// Wraps `out[start..]` in a centered block.
fn center_from(out: &mut Vec<Block>, start: Option<usize>) {
    if let Some(start) = start.filter(|&s| s < out.len()) {
        let inner = out.split_off(start);
        out.push(Block::Center(inner));
    }
}

/// An element of a raw HTML block, with byte ranges into the block.
struct El<'h> {
    name: String,
    /// The opening tag, between `<` and `>`.
    tag: &'h str,
    outer: Range<usize>,
    inner: Range<usize>,
    /// False when the block ended before the element did.
    closed: bool,
    children: Vec<El<'h>>,
}

struct HtmlTree<'h> {
    els: Vec<El<'h>>,
    /// Closing tags of containers opened in an earlier block.
    stray_closes: usize,
}

const VOID_TAGS: [&str; 13] =
    ["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr"];

/// Elements that become blocks of their own; everything else is inline.
const BLOCK_TAGS: [&str; 34] = [
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "dd",
    "details",
    "dialog",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "ul",
    "li",
];

/// Elements that are commonly opened in one HTML block and closed in another.
const CONTAINERS: [&str; 12] = [
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "div",
    "figure",
    "footer",
    "header",
    "main",
    "p",
    "section",
];

/// Builds an element tree, closing elements the way browsers do when a tag
/// implies it (`<li>` ends the previous `<li>`, a block ends a `<p>`, …).
fn html_tree(html: &str) -> HtmlTree<'_> {
    fn attach<'h>(stack: &mut [El<'h>], root: &mut Vec<El<'h>>, el: El<'h>) {
        match stack.last_mut() {
            Some(parent) => parent.children.push(el),
            None => root.push(el),
        }
    }
    fn close_to<'h>(stack: &mut Vec<El<'h>>, root: &mut Vec<El<'h>>, to: usize, at: usize, closed: bool) {
        while stack.len() > to {
            let mut el = stack.pop().unwrap();
            el.inner.end = at;
            el.outer.end = at;
            el.closed = closed;
            attach(stack, root, el);
        }
    }
    let mut stack: Vec<El> = Vec::new();
    let mut root = Vec::new();
    let mut stray_closes = 0;
    let mut i = 0;
    while let Some(off) = html[i..].find('<') {
        let at = i + off;
        if html[at..].starts_with("<!--") {
            i = html[at + 4..].find("-->").map_or(html.len(), |e| at + 4 + e + 3);
            continue;
        }
        let Some(len) = html[at..].find('>') else { break };
        let end = at + len + 1;
        let tag = &html[at + 1..end - 1];
        i = end;
        let name = tag_name(tag);
        if name.is_empty() {
            continue;
        }
        if tag.starts_with('/') {
            match stack.iter().rposition(|e| e.name == name) {
                Some(pos) => {
                    close_to(&mut stack, &mut root, pos + 1, at, true);
                    let mut el = stack.pop().unwrap();
                    el.inner.end = at;
                    el.outer.end = end;
                    el.closed = true;
                    attach(&mut stack, &mut root, el);
                }
                None if CONTAINERS.contains(&name.as_str()) => stray_closes += 1,
                None => {}
            }
            continue;
        }
        let implied: &[&str] = match name.as_str() {
            "li" => &["li"],
            "td" | "th" => &["td", "th"],
            "tr" => &["tr", "td", "th"],
            "dt" | "dd" => &["dt", "dd"],
            n if BLOCK_TAGS.contains(&n) => &["p"],
            _ => &[],
        };
        while let Some(top) = stack.last()
            && implied.contains(&top.name.as_str())
        {
            let to = stack.len() - 1;
            close_to(&mut stack, &mut root, to, at, true);
        }
        let void = VOID_TAGS.contains(&name.as_str()) || tag.ends_with('/');
        let el = El { name, tag, outer: at..end, inner: end..end, closed: void, children: Vec::new() };
        if void {
            attach(&mut stack, &mut root, el);
        } else {
            stack.push(el);
        }
    }
    close_to(&mut stack, &mut root, 0, html.len(), false);
    HtmlTree { els: root, stray_closes }
}

/// Splits off the last element when it's a container the block left open.
fn split_open_tail<'e, 'h>(els: &'e [El<'h>]) -> (&'e [El<'h>], Option<&'e El<'h>>) {
    match els.split_last() {
        Some((last, rest)) if !last.closed && CONTAINERS.contains(&last.name.as_str()) => (rest, Some(last)),
        _ => (els, None),
    }
}

fn is_centered(el: &El) -> bool {
    el.name == "center" || cell_align(el) == Alignment::Center
}

/// Alignment from an `align` attribute or a `text-align` style.
fn cell_align(el: &El) -> Alignment {
    let style = attr(el.tag, "style").unwrap_or_default().to_ascii_lowercase().replace(' ', "");
    let value = attr(el.tag, "align").map(|a| a.to_ascii_lowercase()).or_else(|| {
        let i = style.find("text-align:")?;
        Some(style[i + 11..].split(';').next().unwrap_or("").to_string())
    });
    match value.as_deref().map(str::trim) {
        Some("center") => Alignment::Center,
        Some("right") => Alignment::Right,
        Some("left") => Alignment::Left,
        _ => Alignment::None,
    }
}

/// Lowercase element name of a tag (without `<>`), or "" if it isn't one.
fn tag_name(tag: &str) -> String {
    let t = tag.strip_prefix('/').unwrap_or(tag);
    if !t.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return String::new();
    }
    t.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect::<String>().to_ascii_lowercase()
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let skip = if rest.starts_with("<!--") {
            rest.find("-->").map_or(rest.len(), |e| e + 3)
        } else if tag_name(&rest[1..]).is_empty() {
            out.push('<');
            1
        } else {
            rest.find('>').map_or(rest.len(), |e| e + 1)
        };
        rest = &rest[skip..];
    }
    out.push_str(rest);
    out
}

fn has_attr(tag: &str, name: &str) -> bool {
    tag.split(|c: char| c.is_whitespace() || c == '/')
        .any(|w| w.split('=').next().is_some_and(|n| n.eq_ignore_ascii_case(name)))
}

/// GitHub-style heading anchor.
pub fn slugify(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

pub fn plain_text(inl: &[Inline], images: &[ImageRef]) -> String {
    let mut s = String::new();
    for i in inl {
        match i {
            Inline::Text { text, .. } => s.push_str(text),
            Inline::Image { idx, .. } => s.push_str(&images[*idx].alt),
            Inline::Break => s.push(' '),
        }
    }
    s
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(name) {
        let start = from + pos;
        from = start + name.len();
        let before_ok = start == 0 || lower.as_bytes()[start - 1].is_ascii_whitespace();
        let rest = tag[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let rest = rest[1..].trim_start();
        let value = match rest.chars().next()? {
            q @ ('"' | '\'') => rest[1..].split(q).next()?,
            _ => rest.split(|c: char| c.is_whitespace() || c == '/' || c == '>').next()?,
        };
        return Some(decode_entities(value));
    }
    None
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest[1..].find(';').filter(|&e| e > 0 && e <= 32).and_then(|e| {
            let name = &rest[1..1 + e];
            let c = match name.strip_prefix('#') {
                Some(num) => {
                    let n = match num.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => num.parse().ok(),
                    };
                    char::from_u32(n?)?
                }
                None => named_entity(name)?,
            };
            Some((c, e + 2))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "ensp" => '\u{2002}',
        "emsp" => '\u{2003}',
        "thinsp" => '\u{2009}',
        "zwj" => '\u{200d}',
        "zwnj" => '\u{200c}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "middot" => '·',
        "bull" => '•',
        "times" => '×',
        "divide" => '÷',
        "deg" => '°',
        "plusmn" => '±',
        "sect" => '§',
        "para" => '¶',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "harr" => '↔',
        "laquo" => '«',
        "raquo" => '»',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "check" => '✓',
        "hearts" => '♥',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_follow_github() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("API v2.0 — Notes"), "api-v20--notes");
    }

    #[test]
    fn duplicate_headings_get_suffixes() {
        let doc = parse("# Intro\n\n# Intro\n");
        let anchors: Vec<_> = doc
            .blocks
            .iter()
            .filter_map(|b| if let Block::Heading { anchor, .. } = b { Some(anchor.as_str()) } else { None })
            .collect();
        assert_eq!(anchors, ["intro", "intro-1"]);
    }

    #[test]
    fn task_lists_and_links() {
        let doc = parse("- [x] done [site](https://example.com)\n- [ ] todo\n");
        let Block::List { items, .. } = &doc.blocks[0] else { panic!() };
        assert_eq!(items[0].task, Some(true));
        assert_eq!(items[1].task, Some(false));
        assert_eq!(doc.links, ["https://example.com"]);
    }

    #[test]
    fn inline_source_spans() {
        let src = "Some **bold** and `code` text.\n";
        let doc = parse(src);
        let Block::Paragraph(inl) = &doc.blocks[0] else { panic!() };
        let spans: Vec<(&str, Src)> = inl
            .iter()
            .filter_map(
                |i| if let Inline::Text { text, src: Some(s), .. } = i { Some((text.as_str(), *s)) } else { None },
            )
            .collect();
        let bold = spans.iter().find(|(t, _)| *t == "bold").unwrap().1;
        assert_eq!(&src[bold.start..bold.end], "bold");
        assert_eq!(&src[bold.open.unwrap()..bold.close.unwrap()], "**bold**");
        let code = spans.iter().find(|(t, _)| *t == "code").unwrap().1;
        assert_eq!(&src[code.open.unwrap()..code.close.unwrap()], "`code`");
    }

    #[test]
    fn code_blocks_map_lines_to_source() {
        let src = "- item\n\n  ```rust\n  let a = 1;\n  let b = 2;\n  ```\n";
        let doc = parse(src);
        let Block::List { items, .. } = &doc.blocks[0] else { panic!() };
        let Some(Block::Code { text, src: cs, .. }) = items[0].blocks.get(1) else { panic!("{:?}", items[0].blocks) };
        let b = text.find("let b").unwrap();
        let at = cs.offset(b).unwrap();
        assert_eq!(&src[at..at + 5], "let b");
        assert!(src[cs.block.0..cs.block.1].starts_with("```rust"));
    }

    fn texts(inl: &[Inline]) -> String {
        plain_text(inl, &[])
    }

    #[test]
    fn html_blocks_become_blocks() {
        let doc = parse(
            "<h1 align=\"center\">Title</h1>\n\n<p align=\"center\">\n  <b>Bold</b> &mdash; &#x2713;<br>\n  next\n</p>\n\n<!-- hidden -->\n",
        );
        let Block::Center(h) = &doc.blocks[0] else { panic!("{:?}", doc.blocks) };
        assert!(matches!(&h[0], Block::Heading { level: 1, anchor, .. } if anchor == "title"));
        let Block::Center(p) = &doc.blocks[1] else { panic!("{:?}", doc.blocks) };
        let Block::Paragraph(inl) = &p[0] else { panic!() };
        assert_eq!(texts(inl), "Bold — ✓  next");
        assert_eq!(doc.blocks.len(), 2);
    }

    #[test]
    fn html_tables_keep_cell_alignment() {
        let doc = parse(
            "<table>\n<tr><th>A</th><th>B</th></tr>\n<tr><td>1<td align=\"right\">2</tr>\n<tr><td><img src=\"x.png\"></td></tr>\n</table>\n",
        );
        let Block::Table { head, rows, cell_aligns, .. } = &doc.blocks[0] else { panic!("{:?}", doc.blocks) };
        assert_eq!(head.iter().map(|c| texts(c)).collect::<Vec<_>>(), ["A", "B"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(texts(&rows[0][1]), "2");
        assert_eq!(cell_aligns[1], [Alignment::None, Alignment::Right]);
        assert!(matches!(rows[1][0][0], Inline::Image { .. }));
    }

    #[test]
    fn open_div_centers_following_markdown() {
        let doc = parse("<div align=\"center\">\n\n# Name\n\nText\n\n</div>\n\nAfter\n");
        let Block::Center(inner) = &doc.blocks[0] else { panic!("{:?}", doc.blocks) };
        assert_eq!(inner.len(), 2);
        assert!(matches!(&doc.blocks[1], Block::Paragraph(_)));
    }

    #[test]
    fn html_lists_and_pre() {
        let doc = parse(
            "<ol start=\"3\"><li>one<li>two</ol>\n\n<pre lang=\"rust\"><code>let a = &lt;b&gt;;\n</code></pre>\n",
        );
        let Block::List { start: Some(3), items } = &doc.blocks[0] else { panic!("{:?}", doc.blocks) };
        assert_eq!(items.len(), 2);
        let Block::Code { lang, text, .. } = &doc.blocks[1] else { panic!("{:?}", doc.blocks) };
        assert_eq!((lang.as_str(), text.as_str()), ("rust", "let a = <b>;\n"));
    }

    #[test]
    fn decodes_entities() {
        assert_eq!(decode_entities("a &amp; b &lt;c&gt; &#65;&#x42; &bogus; & x"), "a & b <c> AB &bogus; & x");
    }

    #[test]
    fn html_images_are_extracted() {
        let doc = parse("<p align=\"center\"><img src=\"logo.png\" alt=\"Logo\" width=200></p>\n");
        assert_eq!(doc.images[0].url, "logo.png");
        assert_eq!(doc.images[0].alt, "Logo");
        assert_eq!(doc.images[0].width, Some(Length::Px(200.0)));
    }
}
