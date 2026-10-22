//! Application state and input handling.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::clipboard;
use crate::convert::Format;
use crate::doc::{self, Block, Doc, ImageRef};
use crate::highlight::Highlighter;
use crate::images::Images;
use crate::layout::{self, Env, Kind, Layout};
use crate::links::{self, Target, percent_decode};
use crate::probe::Caps;
use crate::state::{self, State};
use crate::theme::Theme;

#[derive(Clone)]
pub struct Source {
    pub path: Option<PathBuf>,
    pub name: String,
    pub text: String,
    pub mtime: Option<SystemTime>,
}

impl Source {
    pub fn from_path(path: &Path) -> std::io::Result<Source> {
        let text = std::fs::read_to_string(path)?;
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Source { path: Some(path), name, text, mtime })
    }

    pub fn base_dir(&self) -> PathBuf {
        self.path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
    }
}

#[derive(PartialEq)]
pub enum Mode {
    Normal,
    Search(String),
    Help,
}

#[derive(Clone, Copy, Debug)]
pub struct Match {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Default)]
pub struct Search {
    pub query: String,
    pub matches: Vec<Match>,
    pub current: usize,
}

#[derive(Clone, Copy)]
pub enum HitTarget {
    Link(usize),
    Toc(usize),
    /// Index into `Layout::tabs`.
    Tab(usize),
}

#[derive(Clone, Copy)]
pub struct Hit {
    pub y: u16,
    pub x0: u16,
    pub x1: u16,
    pub target: HitTarget,
}

#[derive(Clone, Copy)]
pub struct Geometry {
    pub toc_w: u16,
    pub content_x: u16,
    pub content_w: u16,
    pub rows: u16,
}

/// What a link points at, resolved once per document load.
pub struct LinkInfo {
    /// Human-readable target shown in the hover popup.
    pub label: String,
    /// A local file or `#anchor` that doesn't exist.
    pub broken: bool,
}

/// The link under the mouse pointer and where its hit area starts.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Hover {
    pub link: usize,
    pub x: u16,
    pub y: u16,
}

/// A position in the laid-out document: layout line and display column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

/// Mouse selection, tmux style: the cell under both ends is included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Pos,
    pub cur: Pos,
}

impl Selection {
    /// Start (inclusive) and end (exclusive column) in reading order.
    pub fn range(&self) -> (Pos, Pos) {
        let (a, b) = if self.anchor <= self.cur { (self.anchor, self.cur) } else { (self.cur, self.anchor) };
        (a, Pos { line: b.line, col: b.col + 1 })
    }

    /// Selected display columns `start..end` of layout line `line`.
    pub fn cols(&self, line: usize) -> Option<(usize, usize)> {
        let (a, b) = self.range();
        if line < a.line || line > b.line {
            return None;
        }
        let start = if line == a.line { a.col } else { 0 };
        let end = if line == b.line { b.col } else { usize::MAX };
        Some((start, end))
    }
}

#[derive(Clone, Copy)]
struct Press {
    x: u16,
    y: u16,
    pos: Pos,
    in_content: bool,
}

pub struct Options {
    pub toc: bool,
    /// Optional cap on the text column width; `None` fills the window.
    pub max_width: Option<u16>,
    /// Contents width the user dragged to; `None` fits the headings.
    pub toc_width: Option<u16>,
    /// Remember contents visibility and width across runs.
    pub persist: bool,
}

/// Narrowest contents column, and the least text width it leaves.
const MIN_TOC: u16 = 12;
const MIN_TEXT: u16 = 40;

pub struct App {
    pub caps: Caps,
    pub theme: Theme,
    pub hl: Highlighter,
    pub images: Images,
    pub source: Source,
    pub doc: Doc,
    pub layout: Layout,
    /// Loader key per `doc.images` index.
    pub image_keys: Vec<String>,
    /// Link ids in reading order with the line they first appear on.
    pub link_order: Vec<(usize, usize)>,
    /// Resolution of each `doc.links` entry.
    pub link_info: Vec<LinkInfo>,
    pub hover: Option<Hover>,
    /// Format each data code block (by id) is shown in, when switched.
    formats: HashMap<usize, Format>,
    pub selection: Option<Selection>,
    press: Option<Press>,
    /// Last drag position, and -1/1 while it sits on the top/bottom edge.
    drag: (u16, u16, i8),
    last_autoscroll: Instant,
    /// Partial OSC 5522 reply being swallowed from the key stream.
    reply: Option<(String, Instant)>,
    /// Escape sequences (clipboard writes) to emit with the next frame.
    pub pending: String,
    /// Contents width that fits this document's headings.
    pub toc_width: u16,
    pub scroll: usize,
    pub toc_scroll: usize,
    /// Heading the contents was last scrolled to follow, so scrolling the
    /// contents by hand sticks until the reading position moves on.
    pub toc_followed: Option<Option<usize>>,
    /// Dragging the contents border.
    resizing: bool,
    /// Mouse pointer shape last set with OSC 22.
    cursor: &'static str,
    pub size: (u16, u16),
    pub opts: Options,
    pub focus: Option<usize>,
    pub search: Search,
    pub mode: Mode,
    history: Vec<(Source, usize)>,
    pub message: Option<(String, Instant)>,
    pub hits: Vec<Hit>,
    pub quit: bool,
}

impl App {
    pub fn new(caps: Caps, theme: Theme, images: Images, source: Source, opts: Options) -> App {
        let hl = Highlighter::new(theme.syntect_theme);
        let size = crossterm::terminal::size().unwrap_or((80, 24));
        let mut app = App {
            caps,
            theme,
            hl,
            images,
            source: source.clone(),
            doc: Doc::default(),
            layout: Layout::default(),
            image_keys: Vec::new(),
            link_order: Vec::new(),
            link_info: Vec::new(),
            hover: None,
            formats: HashMap::new(),
            selection: None,
            press: None,
            drag: (0, 0, 0),
            last_autoscroll: Instant::now(),
            reply: None,
            pending: String::new(),
            toc_width: 0,
            scroll: 0,
            toc_scroll: 0,
            toc_followed: None,
            resizing: false,
            cursor: "default",
            size,
            opts,
            focus: None,
            search: Search::default(),
            mode: Mode::Normal,
            history: Vec::new(),
            message: None,
            hits: Vec::new(),
            quit: false,
        };
        app.load(source);
        app
    }

    fn load(&mut self, source: Source) {
        if source.path != self.source.path || source.path.is_none() {
            self.formats.clear();
        }
        self.doc = doc::parse(&source.text);
        let base = source.base_dir();
        self.image_keys = self.doc.images.iter().map(|i| Images::resolve(&i.url, &base)).collect();
        let dark = self.theme.bg.luminance() < 0.5;
        let em = self.caps.cell.1 as f32 * 0.8;
        for (img, key) in self.doc.images.iter().zip(&self.image_keys) {
            match &img.mermaid {
                Some(source) => self.images.request_mermaid(key, source, dark, self.theme.bg),
                None => match &img.math {
                    // Formulas are drawn at about the size of the text.
                    Some(m) => self.images.request_math(key, &m.tex, m.display, self.theme.fg, em),
                    None => self.images.request(key),
                },
            }
        }
        let mut widest = 0;
        visit_headings(&self.doc.blocks, &self.doc.images, &mut |level, title| {
            widest = widest.max(title.width() + 2 * (level as usize).saturating_sub(1));
        });
        self.toc_width = if widest == 0 { 0 } else { (widest + 4).clamp(20, 36) as u16 };
        self.link_info = resolve_links(&self.doc, &base);
        self.source = source;
        self.focus = None;
        self.hover = None;
        self.selection = None;
        self.press = None;
        self.scroll = 0;
        self.toc_scroll = 0;
        self.toc_followed = None;
        self.relayout();
        if !self.search.query.is_empty() {
            self.run_search();
        }
    }

    pub fn geometry(&self) -> Geometry {
        let (w, h) = self.size;
        let rows = h.saturating_sub(1).max(1);
        let toc_w = match self.opts.toc_width {
            _ if !self.opts.toc || self.toc_width == 0 || w < 72 => 0,
            Some(custom) => custom.min(w.saturating_sub(MIN_TEXT)).max(MIN_TOC),
            None => self.toc_width.min(w / 3),
        };
        let content_x = if toc_w > 0 { toc_w + 3 } else { 2 };
        let content_w = w.saturating_sub(content_x + 2).min(self.opts.max_width.unwrap_or(u16::MAX)).max(12);
        Geometry { toc_w, content_x, content_w, rows }
    }

    pub fn current_heading(&self) -> Option<usize> {
        self.layout.headings.iter().rposition(|h| h.line <= self.scroll + 1)
    }

    pub fn relayout(&mut self) {
        let g = self.geometry();
        let anchor = self.current_heading().map(|i| (i, self.scroll - self.layout.headings[i].line.min(self.scroll)));
        let (keys, images) = (&self.image_keys, &self.images);
        let dims = |i: usize| images.dims(&keys[i]);
        let errors = |i: usize| images.error(&keys[i]);
        let formats = &self.formats;
        let code_format = |id: usize| formats.get(&id).copied();
        let broken: Vec<bool> = self.link_info.iter().map(|l| l.broken).collect();
        let env = Env {
            theme: &self.theme,
            hl: &self.hl,
            text_sizing: self.caps.text_sizing,
            image_dims: &dims,
            cell: self.caps.cell,
            max_image_rows: (g.rows as usize).saturating_sub(3),
            image_error: &errors,
            code_format: &code_format,
            broken_links: &broken,
        };
        self.layout = layout::layout(&self.doc, g.content_w as usize, &env);
        if let Some((i, offset)) = anchor
            && let Some(h) = self.layout.headings.get(i)
        {
            self.scroll = h.line + offset;
        }
        self.scroll = self.scroll.min(self.max_scroll());

        self.link_order.clear();
        let mut seen = std::collections::HashSet::new();
        for (li, line) in self.layout.lines.iter().enumerate() {
            for seg in &line.segs {
                if let Some(l) = seg.link
                    && seen.insert(l)
                {
                    self.link_order.push((l, li));
                }
            }
        }
        if !self.search.query.is_empty() {
            self.run_search();
        }
    }

    fn max_scroll(&self) -> usize {
        self.layout.lines.len().saturating_sub(self.geometry().rows as usize)
    }

    fn scroll_by(&mut self, delta: isize) {
        if delta < 0 {
            self.scroll = self.scroll.saturating_sub(delta.unsigned_abs());
        } else {
            // Jumps may overscroll past the end; scrolling never adds to that.
            let limit = self.max_scroll().max(self.scroll);
            self.scroll = (self.scroll + delta as usize).min(limit);
        }
    }

    fn jump_to_line(&mut self, line: usize) {
        self.scroll = line.min(self.layout.lines.len().saturating_sub(1));
    }

    fn reveal(&mut self, line: usize) {
        let rows = self.geometry().rows as usize;
        if line < self.scroll || line >= self.scroll + rows {
            self.scroll = line.saturating_sub(rows / 3).min(self.max_scroll().max(line.saturating_sub(rows / 3)));
        }
    }

    pub fn set_message(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now()));
    }

    pub fn expire_message(&mut self) -> bool {
        if self.message.as_ref().is_some_and(|(_, t)| t.elapsed().as_secs() >= 4) {
            self.message = None;
            return true;
        }
        false
    }

    /// Reloads the file if it changed on disk. Returns true if it did.
    pub fn check_reload(&mut self) -> bool {
        let Some(path) = self.source.path.clone() else { return false };
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if mtime.is_none() || mtime == self.source.mtime {
            return false;
        }
        match Source::from_path(&path) {
            Ok(src) => {
                let (scroll, focus) = (self.scroll, self.focus);
                self.load(src);
                self.scroll = scroll.min(self.max_scroll());
                self.focus = focus;
                self.set_message("Reloaded");
            }
            Err(e) => {
                self.source.mtime = mtime;
                self.set_message(format!("Reload failed: {e}"));
            }
        }
        true
    }

    fn push_history(&mut self) {
        self.history.push((self.source.clone(), self.scroll));
    }

    fn back(&mut self) {
        let Some((src, scroll)) = self.history.pop() else {
            self.set_message("No previous location");
            return;
        };
        if src.path.is_some() && src.path == self.source.path {
            self.scroll = scroll;
        } else {
            let fresh = src.path.as_deref().and_then(|p| Source::from_path(p).ok()).unwrap_or(src);
            self.load(fresh);
            self.scroll = scroll.min(self.max_scroll().max(scroll));
        }
    }

    fn find_anchor(&self, anchor: &str) -> Option<usize> {
        lookup_anchor(&self.layout.anchors, anchor).copied()
    }

    fn jump_anchor(&mut self, anchor: &str) -> bool {
        match self.find_anchor(anchor) {
            Some(line) => {
                self.push_history();
                self.jump_to_line(line);
                true
            }
            None => {
                self.set_message(format!("No heading #{anchor}"));
                false
            }
        }
    }

    pub fn follow(&mut self, link: usize) {
        let Some(raw) = self.doc.links.get(link).cloned() else { return };
        if raw.is_empty() {
            return;
        }
        match links::classify(&raw, &self.source.base_dir()) {
            Target::Anchor(a) => {
                self.jump_anchor(&a);
            }
            Target::Doc(path, fragment) => match Source::from_path(&path) {
                Ok(src) => {
                    self.push_history();
                    self.load(src);
                    if let Some(f) = fragment
                        && let Some(line) = self.find_anchor(&f)
                    {
                        self.jump_to_line(line);
                    }
                    self.set_message(format!("Opened {}  (Backspace to go back)", self.source.name));
                }
                Err(e) => self.set_message(format!("Can't open {}: {e}", path.display())),
            },
            Target::External(url) => match links::open_external(&url) {
                Ok(()) => self.set_message(format!("Opened {url}")),
                Err(e) => self.set_message(format!("Can't open {url}: {e}")),
            },
            Target::Missing(p) => self.set_message(format!("Not found: {p}")),
        }
    }

    fn focus_step(&mut self, forward: bool) {
        if self.link_order.is_empty() {
            self.set_message("No links in this document");
            return;
        }
        let rows = self.geometry().rows as usize;
        let n = self.link_order.len();
        let current = self.focus.and_then(|f| self.link_order.iter().position(|(l, _)| *l == f));
        let next = match (current, forward) {
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
            (None, true) => self.link_order.iter().position(|(_, line)| *line >= self.scroll).unwrap_or(0),
            (None, false) => self.link_order.iter().rposition(|(_, line)| *line < self.scroll + rows).unwrap_or(n - 1),
        };
        let (link, line) = self.link_order[next];
        self.focus = Some(link);
        self.reveal(line);
        let target = self.doc.links[link].clone();
        self.set_message(format!("→ {target}"));
    }

    fn heading_step(&mut self, forward: bool) {
        let hs = &self.layout.headings;
        let target = if forward {
            hs.iter().find(|h| h.line > self.scroll).map(|h| h.line)
        } else {
            hs.iter().rev().find(|h| h.line < self.scroll).map(|h| h.line)
        };
        if let Some(line) = target {
            self.jump_to_line(line);
        }
    }

    pub fn run_search(&mut self) {
        self.search.matches.clear();
        let needle: Vec<char> = self.search.query.chars().map(lower).collect();
        if needle.is_empty() {
            return;
        }
        for (li, line) in self.layout.lines.iter().enumerate() {
            if matches!(line.kind, Kind::HeadingCont { .. }) {
                continue;
            }
            let mut cells: Vec<(char, usize, usize)> = Vec::new();
            let mut col = 0;
            for seg in &line.segs {
                if let Some(cell) = seg.image {
                    col += cell.cols as usize;
                    continue;
                }
                for ch in seg.text.chars() {
                    let w = ch.width().unwrap_or(0);
                    cells.push((lower(ch), col, w));
                    col += w;
                }
            }
            let mut i = 0;
            while i + needle.len() <= cells.len() {
                if cells[i..i + needle.len()].iter().zip(&needle).all(|(c, n)| c.0 == *n) {
                    let last = cells[i + needle.len() - 1];
                    self.search.matches.push(Match { line: li, start: cells[i].1, end: last.1 + last.2 });
                    i += needle.len();
                } else {
                    i += 1;
                }
            }
        }
        self.search.current = self.search.current.min(self.search.matches.len().saturating_sub(1));
    }

    fn search_step(&mut self, forward: bool) {
        let n = self.search.matches.len();
        if n == 0 {
            if !self.search.query.is_empty() {
                self.set_message(format!("No matches for “{}”", self.search.query));
            }
            return;
        }
        self.search.current = if forward { (self.search.current + 1) % n } else { (self.search.current + n - 1) % n };
        let line = self.search.matches[self.search.current].line;
        self.reveal(line);
    }

    fn submit_search(&mut self, query: String) {
        self.search.query = query;
        self.search.current = 0;
        self.run_search();
        if self.search.matches.is_empty() {
            self.set_message(format!("No matches for “{}”", self.search.query));
            return;
        }
        self.search.current = self.search.matches.iter().position(|m| m.line >= self.scroll).unwrap_or(0);
        let line = self.search.matches[self.search.current].line;
        self.reveal(line);
    }

    /// Handles one input event. Returns true if a redraw is needed.
    pub fn handle(&mut self, ev: Event) -> bool {
        if let Event::Key(k) = &ev
            && self.swallow_reply(k)
        {
            return false;
        }
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                self.hover = None;
                self.key(k);
            }
            Event::Mouse(m) => return self.mouse(m),
            Event::Resize(w, h) => {
                self.hover = None;
                self.size = (w, h);
                self.relayout();
            }
            Event::FocusGained => {}
            _ => return false,
        }
        true
    }

    fn key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match &mut self.mode {
            Mode::Search(q) => {
                match k.code {
                    KeyCode::Esc => self.mode = Mode::Normal,
                    KeyCode::Enter => {
                        let q = std::mem::take(q);
                        self.mode = Mode::Normal;
                        self.submit_search(q);
                    }
                    KeyCode::Backspace => {
                        if q.pop().is_none() {
                            self.mode = Mode::Normal;
                        }
                    }
                    KeyCode::Char('u') if ctrl => q.clear(),
                    KeyCode::Char(c) => q.push(c),
                    _ => {}
                }
                return;
            }
            Mode::Help => {
                self.mode = Mode::Normal;
                return;
            }
            Mode::Normal => {}
        }
        let rows = self.geometry().rows as isize;
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if self.selection.is_some() {
                    self.selection = None;
                } else if self.focus.is_some() {
                    self.focus = None;
                } else if !self.search.query.is_empty() {
                    self.search = Search::default();
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.scroll_by(1),
            KeyCode::Char('e') if ctrl => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::Char('y') if ctrl => self.scroll_by(-1),
            KeyCode::Char('d') | KeyCode::Char('D') => self.scroll_by(rows / 2),
            KeyCode::Char('u') | KeyCode::Char('U') => self.scroll_by(-rows / 2),
            KeyCode::Char('f') if ctrl => self.scroll_by(rows - 2),
            KeyCode::Char('b') if ctrl => self.scroll_by(-(rows - 2)),
            KeyCode::Char(' ') | KeyCode::PageDown => self.scroll_by(rows - 2),
            KeyCode::Char('b') | KeyCode::PageUp => self.scroll_by(-(rows - 2)),
            KeyCode::Char('g') | KeyCode::Home => self.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll = self.max_scroll(),
            KeyCode::Char(']') | KeyCode::Char('}') => self.heading_step(true),
            KeyCode::Char('[') | KeyCode::Char('{') => self.heading_step(false),
            KeyCode::Char('t') => {
                self.opts.toc = !self.opts.toc;
                self.relayout();
                self.save_state();
            }
            KeyCode::Tab => self.focus_step(true),
            KeyCode::BackTab => self.focus_step(false),
            KeyCode::Enter => match self.focus {
                Some(l) => self.follow(l),
                None => self.focus_step(true),
            },
            KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left => self.back(),
            KeyCode::Char('/') => self.mode = Mode::Search(String::new()),
            KeyCode::Char('n') => self.search_step(true),
            KeyCode::Char('N') => self.search_step(false),
            KeyCode::Char('r') => {
                self.source.mtime = None;
                if !self.check_reload() {
                    self.relayout();
                }
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char(c @ '1'..='9') => {
                // Jump to the n-th top-level section.
                let top = self.layout.headings.iter().map(|h| h.level).min().unwrap_or(1);
                let n = c as usize - '1' as usize;
                if let Some(h) = self.layout.headings.iter().filter(|h| h.level == top).nth(n) {
                    let line = h.line;
                    self.jump_to_line(line);
                }
            }
            _ => {}
        }
    }

    fn mouse(&mut self, m: MouseEvent) -> bool {
        let redraw = self.mouse_action(m);
        let shape = self.cursor_shape(m.column, m.row);
        if shape == self.cursor {
            return redraw;
        }
        self.cursor = shape;
        self.push_cursor();
        true
    }

    /// Queues OSC 22 for the current pointer shape.
    fn push_cursor(&mut self) {
        let seq = format!("\x1b]22;{}\x1b\\", self.cursor);
        if self.caps.tmux {
            self.pending.push_str(&format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b")));
        } else {
            self.pending.push_str(&seq);
        }
    }

    /// Resets the pointer shape, for leaving the viewer.
    pub fn reset_cursor(&mut self) -> String {
        self.cursor = "default";
        self.push_cursor();
        std::mem::take(&mut self.pending)
    }

    /// The pointer shape for the cell at `x`, `y`: a hand over things that
    /// can be clicked, "copy" over code blocks, and a resize arrow on the
    /// contents border.
    fn cursor_shape(&self, x: u16, y: u16) -> &'static str {
        if self.resizing || self.on_toc_border(x, y) {
            return "ew-resize";
        }
        if self.press.is_some() || self.mode != Mode::Normal {
            return "default";
        }
        if self.hits.iter().any(|h| h.y == y && (h.x0..h.x1).contains(&x)) {
            return "pointer";
        }
        let g = self.geometry();
        let in_text = y < g.rows && (g.content_x..g.content_x + g.content_w).contains(&x);
        // `pos_at` clamps to the last line; rows past the end aren't in a block.
        if in_text && self.scroll + (y as usize) < self.layout.lines.len() {
            let pos = self.pos_at(x, y);
            let in_block = self.layout.code_blocks.iter().any(|b| (b.start..b.end).contains(&pos.line));
            if in_block || self.inline_math_at(pos).is_some() {
                return "copy";
            }
        }
        "default"
    }

    fn on_toc_border(&self, x: u16, y: u16) -> bool {
        let g = self.geometry();
        g.toc_w > 0 && x == g.toc_w && y < g.rows
    }

    fn save_state(&self) {
        if self.opts.persist {
            state::save(State { toc: self.opts.toc, toc_width: self.opts.toc_width });
        }
    }

    pub fn toc_scroll_by(&mut self, delta: isize) {
        let rows = (self.geometry().rows as usize).saturating_sub(2);
        let max = self.layout.headings.len().saturating_sub(rows);
        self.toc_scroll = self.toc_scroll.saturating_add_signed(delta).min(max);
        self.toc_followed = Some(self.current_heading());
    }

    fn mouse_action(&mut self, m: MouseEvent) -> bool {
        let hit = self.hits.iter().find(|h| h.y == m.row && (h.x0..h.x1).contains(&m.column)).copied();
        let over_toc = m.column <= self.geometry().toc_w && self.geometry().toc_w > 0;
        match m.kind {
            MouseEventKind::Moved => {
                let hover = match hit {
                    Some(Hit { target: HitTarget::Link(link), x0, y, .. }) if self.mode == Mode::Normal => {
                        Some(Hover { link, x: x0, y })
                    }
                    _ => None,
                };
                // Only redraw when the hovered link changes.
                let changed = hover != self.hover;
                self.hover = hover;
                return changed;
            }
            MouseEventKind::ScrollDown if over_toc => self.toc_scroll_by(3),
            MouseEventKind::ScrollUp if over_toc => self.toc_scroll_by(-3),
            MouseEventKind::ScrollDown => {
                self.hover = None;
                self.scroll_by(3);
            }
            MouseEventKind::ScrollUp => {
                self.hover = None;
                self.scroll_by(-3);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.hover = None;
                self.selection = None;
                if self.mode != Mode::Normal {
                    self.mode = Mode::Normal;
                    return true;
                }
                if self.on_toc_border(m.column, m.row) {
                    self.resizing = true;
                    return true;
                }
                let g = self.geometry();
                let in_content = m.column + 1 >= g.content_x && m.row < g.rows;
                self.press = Some(Press { x: m.column, y: m.row, pos: self.pos_at(m.column, m.row), in_content });
            }
            MouseEventKind::Drag(MouseButton::Left) if self.resizing => {
                let max = self.size.0.saturating_sub(MIN_TEXT).max(MIN_TOC);
                let width = m.column.clamp(MIN_TOC, max);
                if self.opts.toc_width == Some(width) {
                    return false;
                }
                self.opts.toc_width = Some(width);
                self.relayout();
            }
            MouseEventKind::Up(MouseButton::Left) if self.resizing => {
                self.resizing = false;
                self.save_state();
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(press) = self.press.filter(|p| p.in_content) else { return false };
                let rows = self.geometry().rows;
                let row = m.row.min(rows - 1);
                let edge = if m.row == 0 {
                    -1
                } else if m.row + 1 >= rows {
                    1
                } else {
                    0
                };
                self.drag = (m.column, row, edge);
                self.hover = None;
                self.selection = Some(Selection { anchor: press.pos, cur: self.pos_at(m.column, row) });
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag.2 = 0;
                let Some(press) = self.press.take() else { return false };
                match self.selection {
                    Some(sel) if sel.anchor != sel.cur => {
                        // Copy the Markdown behind the selection; fall back to
                        // the rendered text where there's no source mapping.
                        let (text, kind) = match self.selected_source(&sel) {
                            Some(src) => (src, " of Markdown"),
                            None => (self.selected_text(&sel), ""),
                        };
                        let lines = text.lines().count();
                        let what = if lines > 1 {
                            format!("{lines} lines{kind}")
                        } else {
                            format!("{} chars{kind}", text.chars().count())
                        };
                        self.copy(&text, &what);
                    }
                    _ => {
                        self.selection = None;
                        self.click(press);
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) => self.back(),
            _ => return false,
        }
        true
    }
}

/// Finds an anchor the way GitHub resolves fragments: exact, then
/// percent-decoded, slugified and lowercased.
fn lookup_anchor<'m, T>(map: &'m HashMap<String, T>, anchor: &str) -> Option<&'m T> {
    let decoded = percent_decode(anchor);
    [anchor.to_string(), decoded.clone(), doc::slugify(&decoded), decoded.to_lowercase()]
        .iter()
        .find_map(|a| map.get(a))
}

fn resolve_links(doc: &Doc, base: &Path) -> Vec<LinkInfo> {
    let mut anchors = HashMap::new();
    collect_anchors(&doc.blocks, &doc.images, &mut anchors);
    let relative = |p: &Path| p.strip_prefix(base).unwrap_or(p).display().to_string();
    doc.links
        .iter()
        .map(|raw| {
            if raw.trim().is_empty() {
                return LinkInfo { label: String::new(), broken: false };
            }
            match links::classify(raw, base) {
                Target::Anchor(a) => match lookup_anchor(&anchors, &a) {
                    Some(title) => LinkInfo { label: format!("§ {title}"), broken: false },
                    None => LinkInfo { label: format!("✖ No heading #{a}"), broken: true },
                },
                Target::Doc(path, fragment) => {
                    let frag = fragment.map(|f| format!("#{f}")).unwrap_or_default();
                    LinkInfo { label: format!("→ {}{frag}", relative(&path)), broken: false }
                }
                Target::External(url) => {
                    let label = if links::is_url(&url) { url } else { relative(Path::new(&url)) };
                    LinkInfo { label: format!("↗ {label}"), broken: false }
                }
                Target::Missing(path) => {
                    LinkInfo { label: format!("✖ Not found: {}", relative(Path::new(&path))), broken: true }
                }
            }
        })
        .collect()
}

/// Maps every anchor in the document (headings and footnotes) to a title.
fn collect_anchors(blocks: &[Block], images: &[ImageRef], out: &mut HashMap<String, String>) {
    for b in blocks {
        match b {
            Block::Heading { inlines, anchor, .. } => {
                out.entry(anchor.clone()).or_insert_with(|| doc::plain_text(inlines, images).trim().to_string());
            }
            Block::Footnote { label, blocks } => {
                out.insert(format!("fn-{label}"), format!("Footnote {label}"));
                collect_anchors(blocks, images, out);
            }
            Block::Quote { blocks, .. } | Block::DefBody(blocks) | Block::Center(blocks) => {
                collect_anchors(blocks, images, out)
            }
            Block::List { items, .. } => items.iter().for_each(|i| collect_anchors(&i.blocks, images, out)),
            _ => {}
        }
    }
}

impl App {
    fn pos_at(&self, column: u16, row: u16) -> Pos {
        let g = self.geometry();
        let mut line = (self.scroll + row as usize).min(self.layout.lines.len().saturating_sub(1));
        // Rows under a scaled heading belong to its first row.
        if let Some(Kind::HeadingCont { top }) = self.layout.lines.get(line).map(|l| l.kind) {
            line = top;
        }
        Pos { line, col: column.saturating_sub(g.content_x) as usize }
    }

    /// The inline formula drawn at `pos`: its TeX, column and width.
    fn inline_math_at(&self, pos: Pos) -> Option<(String, usize, usize)> {
        let line = self.layout.lines.get(pos.line)?;
        line.char_cols().into_iter().find_map(|(si, _, col, w)| {
            let cell = line.segs[si].image?;
            let math = self.doc.images[cell.img].math.as_ref()?;
            (col..col + w).contains(&pos.col).then(|| (math.tex.clone(), col, w))
        })
    }

    /// A press and release without dragging: follow a link, jump via the
    /// contents, or copy the code block under the pointer.
    fn click(&mut self, press: Press) {
        let hit = self.hits.iter().find(|h| h.y == press.y && (h.x0..h.x1).contains(&press.x)).copied();
        match hit.map(|h| h.target) {
            Some(HitTarget::Link(l)) => {
                self.focus = Some(l);
                self.follow(l);
            }
            Some(HitTarget::Toc(i)) => {
                let line = self.layout.headings[i].line;
                self.jump_to_line(line);
            }
            Some(HitTarget::Tab(i)) => {
                let tab = self.layout.tabs[i].clone();
                if let Some(e) = tab.error {
                    self.set_message(format!("Can't show as {}: {e}", tab.format.label()));
                    return;
                }
                self.formats.insert(tab.block, tab.format);
                self.relayout();
                self.set_message(if tab.format == tab.source {
                    format!("Showing the original {}", tab.format.label())
                } else {
                    format!("Showing as {} (converted from {})", tab.format.label(), tab.source.label())
                });
            }
            None if press.in_content => {
                if let Some((tex, col, w)) = self.inline_math_at(press.pos) {
                    let line = press.pos.line;
                    self.selection = Some(Selection {
                        anchor: Pos { line, col },
                        cur: Pos { line, col: col + w.saturating_sub(1) },
                    });
                    self.copy(&tex, "formula");
                    return;
                }
                let line = press.pos.line;
                let Some(block) = self.layout.code_blocks.iter().find(|b| (b.start..b.end).contains(&line)).cloned()
                else {
                    return;
                };
                // Highlight the block as feedback until the next click or key.
                self.selection = Some(Selection {
                    anchor: Pos { line: block.start, col: 0 },
                    cur: Pos { line: block.end - 1, col: usize::MAX - 1 },
                });
                let n = block.text.lines().count();
                let lines = if n > 1 { format!(" ({n} lines)") } else { String::new() };
                self.copy(&block.text, &format!("{}{lines}", block.kind));
            }
            None => {}
        }
    }

    /// Text under a selection, without decoration (code block padding,
    /// quote bars) and with trailing whitespace trimmed per line.
    pub fn selected_text(&self, sel: &Selection) -> String {
        let (a, b) = sel.range();
        let mut out = Vec::new();
        for li in a.line..=b.line.min(self.layout.lines.len().saturating_sub(1)) {
            let line = &self.layout.lines[li];
            let Some((from, to)) = sel.cols(li) else { continue };
            if matches!(line.kind, Kind::HeadingCont { .. }) {
                continue;
            }
            let mut text = String::new();
            for (si, off, col, w) in line.char_cols() {
                let seg = &line.segs[si];
                if seg.image.is_some() || seg.decor || !overlaps(col, w, from, to) {
                    continue;
                }
                let ch = seg.text[off..].chars().next().unwrap_or(' ');
                text.push(if ch == '\u{a0}' { ' ' } else { ch });
            }
            out.push(text.trim_end().to_string());
        }
        out.join("\n")
    }

    /// The Markdown source behind a selection. The selected cells map to a
    /// byte range of the file; a selection that starts (or ends) a rendered
    /// line takes the whole source line's markup with it (`- `, `> `, `## `,
    /// table pipes), so whole blocks copy as valid Markdown.
    pub fn selected_source(&self, sel: &Selection) -> Option<String> {
        let source = self.source.text.as_str();
        let (a, b) = sel.range();
        let mut start: Option<(usize, bool)> = None;
        let mut end: Option<(usize, bool)> = None;
        for li in a.line..=b.line.min(self.layout.lines.len().saturating_sub(1)) {
            let line = &self.layout.lines[li];
            let Some((from, to)) = sel.cols(li) else { continue };
            let cells = src_cells(line);
            for (i, &(col, w, s, e)) in cells.iter().enumerate() {
                if overlaps(col, w, from, to) {
                    start.get_or_insert((s, i == 0));
                    end = Some((e, i + 1 == cells.len()));
                }
            }
        }
        let ((s, line_start), (e, line_end)) = (start?, end?);
        let (s, e) = (s.min(e), s.max(e));
        let s = if line_start { snap_line_start(source, s) } else { s };
        let e = if line_end { snap_line_end(source, e) } else { e };
        let text = source.get(s..e)?.trim_end_matches('\n');
        (!text.is_empty()).then(|| text.to_string())
    }

    fn copy(&mut self, text: &str, what: &str) {
        let protocol = if self.caps.clipboard { clipboard::Protocol::Kitty } else { clipboard::Protocol::Osc52 };
        clipboard::copy(text, protocol, &mut self.pending);
        self.set_message(format!("Copied {what}"));
    }

    pub fn dragging(&self) -> bool {
        self.press.is_some() && self.drag.2 != 0
    }

    /// Keeps the selection growing while the pointer rests on the top or
    /// bottom edge during a drag. Returns true if it scrolled.
    pub fn autoscroll(&mut self) -> bool {
        let (x, y, edge) = self.drag;
        if edge == 0 || self.press.is_none() || self.last_autoscroll.elapsed().as_millis() < 50 {
            return false;
        }
        self.last_autoscroll = Instant::now();
        let before = self.scroll;
        self.scroll_by(edge as isize);
        let cur = self.pos_at(x, y);
        if let Some(sel) = &mut self.selection {
            sel.cur = cur;
        }
        self.scroll != before
    }

    /// crossterm doesn't parse OSC, so the terminal's reply to a clipboard
    /// write (`ESC ] 5522 ; … ESC \`) arrives as Alt+`]`, plain keys, then
    /// Alt+`\`. Swallow those keys and report errors from the status field.
    fn swallow_reply(&mut self, k: &KeyEvent) -> bool {
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        if let Some((buf, since)) = &mut self.reply {
            if since.elapsed().as_secs() >= 2 {
                self.reply = None;
            } else {
                let bel = k.code == KeyCode::Char('g') && k.modifiers.contains(KeyModifiers::CONTROL);
                if (alt && k.code == KeyCode::Char('\\')) || bel {
                    let reply = std::mem::take(buf);
                    self.reply = None;
                    if let Some(i) = reply.find("status=") {
                        let status = reply[i + 7..].split([':', ';']).next().unwrap_or("");
                        if status != "DONE" {
                            self.set_message(format!("Clipboard write failed ({status})"));
                        }
                    }
                } else if let KeyCode::Char(c) = k.code {
                    buf.push(c);
                }
                return true;
            }
        }
        if self.caps.clipboard && alt && k.code == KeyCode::Char(']') {
            self.reply = Some((String::new(), Instant::now()));
            return true;
        }
        false
    }
}

/// `(column, width, source start, source end)` for every cell of a line
/// that has a source position.
fn src_cells(line: &layout::Line) -> Vec<(usize, usize, usize, usize)> {
    let mut cells = Vec::new();
    for (si, off, col, w) in line.char_cols() {
        let seg = &line.segs[si];
        let Some(s) = seg.src else { continue };
        if seg.image.is_some() {
            cells.push((col, w, s.char_start(0), s.char_end(0, 0, 0)));
        } else {
            let len = seg.text[off..].chars().next().map_or(0, char::len_utf8);
            cells.push((col, w, s.char_start(off), s.char_end(off, len, seg.text.len())));
        }
    }
    cells
}

/// True when a cell at `col` (`w` wide) is inside columns `from..to`.
fn overlaps(col: usize, w: usize, from: usize, to: usize) -> bool {
    col < to && col + w.max(1) > from
}

/// Moves `off` back to the start of its source line if only block markup
/// (list markers, quote marks, heading hashes, table pipes) precedes it.
fn snap_line_start(source: &str, off: usize) -> usize {
    let line_start = source[..off].rfind('\n').map_or(0, |i| i + 1);
    let lead = &source[line_start..off];
    if lead.chars().all(|c| c.is_whitespace() || "#>*+-.)[]xX|^:0123456789".contains(c)) { line_start } else { off }
}

/// Moves `off` to the end of its source line if only closing markup follows.
fn snap_line_end(source: &str, off: usize) -> usize {
    let line_end = source[off..].find('\n').map_or(source.len(), |i| off + i);
    let tail = &source[off..line_end];
    if tail.chars().all(|c| c.is_whitespace() || "|*_~`".contains(c)) { line_end } else { off }
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn visit_headings(blocks: &[Block], images: &[ImageRef], f: &mut dyn FnMut(u8, &str)) {
    for b in blocks {
        match b {
            Block::Heading { level, inlines, .. } => f(*level, &doc::plain_text(inlines, images)),
            Block::Quote { blocks, .. }
            | Block::Footnote { blocks, .. }
            | Block::DefBody(blocks)
            | Block::Center(blocks) => visit_headings(blocks, images, f),
            Block::List { items, .. } => items.iter().for_each(|i| visit_headings(&i.blocks, images, f)),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(md: &str) -> App {
        let caps =
            Caps { graphics: false, text_sizing: false, cell: (10, 20), bg: None, tmux: false, clipboard: false };
        let source = Source { path: None, name: "t.md".into(), text: md.into(), mtime: None };
        let opts = Options { toc: false, max_width: Some(60), toc_width: None, persist: false };
        App::new(caps, Theme::new(true, None), Images::new(false, false), source, opts)
    }

    /// Position of the `nth` occurrence of `needle` in the rendered lines.
    fn find(app: &App, needle: &str, nth: usize) -> Pos {
        app.layout
            .lines
            .iter()
            .enumerate()
            .flat_map(|(i, l)| {
                let plain = l.plain();
                plain.match_indices(needle).map(|(b, _)| Pos { line: i, col: plain[..b].width() }).collect::<Vec<_>>()
            })
            .nth(nth)
            .unwrap_or_else(|| panic!("{needle:?} not rendered"))
    }

    /// Copies from the first char of `from` to the last char of `to`.
    fn copy_between(md: &str, from: &str, to: &str) -> String {
        let app = app(md);
        let a = find(&app, from, 0);
        let b = find(&app, to, 0);
        let cur = Pos { line: b.line, col: b.col + to.width() - 1 };
        app.selected_source(&Selection { anchor: a, cur }).expect("source for selection")
    }

    #[test]
    fn copies_markdown_source_of_selection() {
        let p = "Some **bold** text and [a link](https://x.y) here.\n";
        assert_eq!(copy_between(p, "bold", "bold"), "**bold**");
        assert_eq!(copy_between(p, "ome", "bo"), "ome **bo");
        assert_eq!(copy_between(p, "a link", "a link"), "[a link](https://x.y)");
        assert_eq!(copy_between(p, "Some", "here."), "Some **bold** text and [a link](https://x.y) here.");

        assert_eq!(copy_between("- one\n- two\n\nAfter.\n", "one", "two"), "- one\n- two");
        assert_eq!(copy_between("> quoted *words*\n", "quoted", "words"), "> quoted *words*");
        assert_eq!(copy_between("## Title\n\ntext\n", "Title", "Title"), "## Title");
        assert_eq!(copy_between("So $E = mc^2$ holds.\n", "So", "holds."), "So $E = mc^2$ holds.");

        let table = "| A | B |\n|---|---|\n| 1 | 2 |\n";
        assert_eq!(copy_between(table, "A", "2"), table.trim_end());
    }

    #[test]
    fn copies_parts_of_scaled_headings() {
        let mut app = app("# Hello World\n\n## Second heading\n");
        app.caps.text_sizing = true;
        app.relayout();
        assert!(matches!(app.layout.lines[0].kind, Kind::Heading { .. }));
        // H1 characters are two cells wide: "World" is at columns 12..22.
        let sel = Selection { anchor: Pos { line: 0, col: 12 }, cur: Pos { line: 0, col: 21 } };
        assert_eq!(app.selected_source(&sel).unwrap(), "World");
        assert_eq!(app.selected_text(&sel), "World");
        let sel = Selection { anchor: Pos { line: 0, col: 0 }, cur: Pos { line: 0, col: 3 } };
        assert_eq!(app.selected_source(&sel).unwrap(), "# He");

        // H2 is drawn in chunks of four characters, selected as a whole.
        let h2 = app.layout.lines.iter().position(|l| l.plain().starts_with("Second")).unwrap();
        let sel = Selection { anchor: Pos { line: h2, col: 0 }, cur: Pos { line: h2, col: 1 } };
        assert_eq!(app.selected_source(&sel).unwrap(), "## Seco");
    }

    #[test]
    fn mouse_shapes_resizes_and_scrolls_contents() {
        let mut md = String::from("[link](https://x.y)\n\n```sh\necho hi\n```\n\n");
        for i in 0..60 {
            md.push_str(&format!("## Section {i}\n\ntext\n\n"));
        }
        let mut app = app(&md);
        app.opts.toc = true;
        app.size = (120, 40);
        app.relayout();
        app.draw(&mut Vec::new()).unwrap();
        let ev = |app: &mut App, kind, column, row| {
            app.handle(Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }));
            std::mem::take(&mut app.pending)
        };
        let g = app.geometry();
        let link = app.hits.iter().find(|h| matches!(h.target, HitTarget::Link(_))).copied().unwrap();
        assert!(ev(&mut app, MouseEventKind::Moved, link.x0, link.y).contains("\x1b]22;pointer"));
        let code = find(&app, "echo", 0);
        let x = g.content_x + code.col as u16;
        assert!(ev(&mut app, MouseEventKind::Moved, x, code.line as u16).contains("]22;copy"));
        assert!(ev(&mut app, MouseEventKind::Moved, g.toc_w, 5).contains("]22;ew-resize"));

        // Drag the border to resize the contents.
        ev(&mut app, MouseEventKind::Down(MouseButton::Left), g.toc_w, 5);
        ev(&mut app, MouseEventKind::Drag(MouseButton::Left), 30, 5);
        ev(&mut app, MouseEventKind::Up(MouseButton::Left), 30, 5);
        assert_eq!((app.opts.toc_width, app.geometry().toc_w), (Some(30), 30));

        // Scrolling over the contents scrolls only the contents, and sticks.
        ev(&mut app, MouseEventKind::ScrollDown, 3, 10);
        app.draw(&mut Vec::new()).unwrap();
        assert_eq!((app.scroll, app.toc_scroll), (0, 3));
    }

    #[test]
    fn data_blocks_switch_format() {
        let mut app = app("```json\n{\"name\": \"mdv\", \"tags\": [\"a\"], \"gone\": null}\n```\n");
        let labels: Vec<_> = app.layout.tabs.iter().map(|t| (t.format, t.error.is_some())).collect();
        assert_eq!(labels, [(Format::Json, false), (Format::Yaml, false), (Format::Toml, true)]);
        assert!(app.layout.lines[0].plain().contains("JSON (source)"));

        app.formats.insert(0, Format::Yaml);
        app.relayout();
        let text: Vec<String> = app.layout.lines.iter().map(|l| l.plain()).collect();
        assert!(text.iter().any(|l| l.trim() == "name: mdv"), "{text:#?}");
        assert_eq!(app.layout.code_blocks[0].text, "name: mdv\ntags:\n- a\ngone: null");
    }

    #[test]
    fn copies_fenced_code_with_fences() {
        let md = "```rust\nlet a = 1;\n```\n";
        let app = app(md);
        let header = find(&app, "rust", 0);
        let bottom = Pos { line: header.line + 2, col: 3 };
        let sel = Selection { anchor: Pos { line: header.line, col: 0 }, cur: bottom };
        assert_eq!(app.selected_source(&sel).unwrap(), md.trim_end());
        assert_eq!(copy_between(md, "a = 1", "a = 1"), "a = 1");
    }

    #[test]
    fn resolves_and_flags_broken_links() {
        let doc = doc::parse(
            "# Tables\n\n[a](#tables) [b](missing.md) [c](#nope) [d](https://example.com) [e](README.md#keys)\n\n[^1]\n\n[^1]: note\n",
        );
        let base = Path::new(env!("CARGO_MANIFEST_DIR"));
        let info = resolve_links(&doc, base);
        let got: Vec<(&str, bool)> = info.iter().map(|i| (i.label.as_str(), i.broken)).collect();
        assert_eq!(
            got,
            [
                ("§ Tables", false),
                ("✖ Not found: missing.md", true),
                ("✖ No heading #nope", true),
                ("↗ https://example.com", false),
                ("→ README.md#keys", false),
                ("§ Footnote 1", false),
            ]
        );
    }
}
