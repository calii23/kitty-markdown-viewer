//! Markdown parsing into a small document model that the layout engine walks.

use std::collections::HashMap;

use pulldown_cmark::{Alignment, BlockQuoteKind, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

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
    },
    /// Index into `Doc::images`; `link` is set when the image is wrapped in a link.
    Image {
        idx: usize,
        link: Option<usize>,
    },
    Break,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageRef {
    pub url: String,
    pub alt: String,
    /// Link id pointing at the image itself, so it can be opened.
    pub self_link: usize,
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
    },
    Rule,
    Footnote {
        label: String,
        blocks: Vec<Block>,
    },
    DefTitle(Vec<Inline>),
    DefBody(Vec<Block>),
    FrontMatter(String),
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
        ev: Parser::new_ext(src, opts),
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
#[derive(Default)]
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
    ev: Parser<'a>,
    doc: Doc,
    depth: Depths,
    links: Vec<usize>,
    tasks: Vec<Option<bool>>,
    slugs: HashMap<String, usize>,
}

impl P<'_> {
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
        let flush = |loose: &mut Vec<Inline>, out: &mut Vec<Block>| {
            if !loose.is_empty() {
                out.push(Block::Plain(std::mem::take(loose)));
            }
        };
        while let Some(e) = self.ev.next() {
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
                    let lang = match kind {
                        CodeBlockKind::Fenced(info) => info
                            .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
                            .next()
                            .unwrap_or("")
                            .to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    let text = self.collect_text(&|t| matches!(t, TagEnd::CodeBlock));
                    out.push(Block::Code { lang, text });
                }
                Event::Start(Tag::List(start)) => {
                    flush(&mut loose, &mut out);
                    let mut items = Vec::new();
                    loop {
                        match self.ev.next() {
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
                    let html = self.collect_text(&|t| matches!(t, TagEnd::HtmlBlock));
                    let mut inl = Vec::new();
                    self.html(&html, &mut inl);
                    trim_breaks(&mut inl);
                    if !inl.is_empty() {
                        out.push(Block::Paragraph(inl));
                    }
                }
                Event::Start(Tag::FootnoteDefinition(label)) => {
                    flush(&mut loose, &mut out);
                    let blocks = self.blocks(&|t| matches!(t, TagEnd::FootnoteDefinition));
                    out.push(Block::Footnote { label: label.to_string(), blocks });
                }
                Event::Start(Tag::MetadataBlock(_)) => {
                    flush(&mut loose, &mut out);
                    let text = self.collect_text(&|t| matches!(t, TagEnd::MetadataBlock(_)));
                    out.push(Block::FrontMatter(text));
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
                    out.push(Block::Rule);
                }
                e => self.inline_event(e, &mut loose),
            }
        }
        flush(&mut loose, &mut out);
        out
    }

    fn collect_text(&mut self, stop: &dyn Fn(&TagEnd) -> bool) -> String {
        let mut s = String::new();
        for e in self.ev.by_ref() {
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
        while let Some(e) = self.ev.next() {
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
            match self.ev.next() {
                Some(Event::Start(Tag::TableHead)) | Some(Event::Start(Tag::TableRow)) => row = Vec::new(),
                Some(Event::End(TagEnd::TableHead)) => head = std::mem::take(&mut row),
                Some(Event::End(TagEnd::TableRow)) => rows.push(std::mem::take(&mut row)),
                Some(Event::Start(Tag::TableCell)) => row.push(self.inlines(&|t| matches!(t, TagEnd::TableCell))),
                Some(Event::End(TagEnd::Table)) | None => break,
                _ => {}
            }
        }
        Block::Table { aligns, head, rows }
    }

    fn push_text(&self, text: &str, style: InlineStyle, out: &mut Vec<Inline>) {
        if text.is_empty() {
            return;
        }
        let text: String = text.chars().map(|c| if c == '\t' { ' ' } else { c }).filter(|c| !c.is_control()).collect();
        let link = self.links.last().copied();
        if let Some(Inline::Text { text: prev, style: s, link: l }) = out.last_mut()
            && *s == style
            && *l == link
        {
            prev.push_str(&text);
            return;
        }
        out.push(Inline::Text { text, style, link });
    }

    fn inline_event(&mut self, e: Event, out: &mut Vec<Inline>) {
        match e {
            Event::Text(t) => self.push_text(&t, self.style(), out),
            Event::Code(t) => {
                let style = InlineStyle { code: true, ..self.style() };
                self.push_text(&t, style, out);
            }
            Event::InlineMath(t) => {
                let style = InlineStyle { math: true, ..self.style() };
                self.push_text(&t, style, out);
            }
            Event::DisplayMath(t) => {
                if !out.is_empty() {
                    out.push(Inline::Break);
                }
                let style = InlineStyle { math: true, ..self.style() };
                for (i, line) in t.trim().lines().enumerate() {
                    if i > 0 {
                        out.push(Inline::Break);
                    }
                    self.push_text(&format!("  {line}"), style, out);
                }
                out.push(Inline::Break);
            }
            Event::Html(h) | Event::InlineHtml(h) => self.html(&h, out),
            Event::FootnoteReference(label) => {
                let id = self.add_link(format!("#fn-{label}"));
                out.push(Inline::Text { text: format!("[{label}]"), style: self.style(), link: Some(id) });
            }
            Event::SoftBreak => self.push_text(" ", self.style(), out),
            Event::HardBreak => out.push(Inline::Break),
            Event::TaskListMarker(done) => {
                if let Some(t) = self.tasks.last_mut() {
                    *t = Some(done);
                }
            }
            Event::Start(tag) => match tag {
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
                    let alt = self.collect_text(&|t| matches!(t, TagEnd::Image));
                    self.push_image(dest_url.to_string(), alt, out);
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Emphasis => self.depth.italic = self.depth.italic.saturating_sub(1),
                TagEnd::Strong => self.depth.bold = self.depth.bold.saturating_sub(1),
                TagEnd::Strikethrough => self.depth.strike = self.depth.strike.saturating_sub(1),
                TagEnd::Superscript => self.depth.sup = self.depth.sup.saturating_sub(1),
                TagEnd::Subscript => self.depth.sub = self.depth.sub.saturating_sub(1),
                TagEnd::Link => {
                    self.links.pop();
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn push_image(&mut self, url: String, alt: String, out: &mut Vec<Inline>) {
        let self_link = self.add_link(url.clone());
        self.doc.images.push(ImageRef { url, alt, self_link });
        out.push(Inline::Image { idx: self.doc.images.len() - 1, link: self.links.last().copied() });
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
                self.html_tag(&rest[1..end], out);
                rest = &rest[end + 1..];
                continue;
            }
            let skip = rest.chars().next().map_or(1, char::len_utf8);
            let next = rest[skip..].find('<').map(|i| i + skip).unwrap_or(rest.len());
            let text = decode_entities(&rest[..next]);
            let collapsed = collapse_ws(&text);
            if !collapsed.trim().is_empty() || (!collapsed.is_empty() && !out.is_empty()) {
                self.push_text(&collapsed, self.style(), out);
            }
            rest = &rest[next..];
        }
    }

    fn html_tag(&mut self, tag: &str, out: &mut Vec<Inline>) {
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
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
                    self.push_image(src, alt, out);
                }
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
    s.replace("&nbsp;", "\u{a0}")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&copy;", "©")
        .replace("&amp;", "&")
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
    fn html_images_are_extracted() {
        let doc = parse("<p align=\"center\"><img src=\"logo.png\" alt=\"Logo\" width=200></p>\n");
        assert_eq!(doc.images[0].url, "logo.png");
        assert_eq!(doc.images[0].alt, "Logo");
    }
}
