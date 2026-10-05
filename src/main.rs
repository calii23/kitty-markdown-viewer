mod app;
mod clipboard;
mod diacritics;
mod doc;
mod highlight;
mod images;
mod kitty;
mod layout;
mod links;
mod probe;
mod render;
mod style;
mod theme;

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use crossterm::event;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use crate::app::{App, Options, Source};
use crate::highlight::Highlighter;
use crate::images::Images;
use crate::layout::{Env, Kind};
use crate::theme::Theme;

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ThemeChoice {
    Auto,
    Dark,
    Light,
}

/// A full-screen Markdown viewer for kitty: inline images, scaled headings,
/// clickable links and a live table of contents.
#[derive(Parser)]
#[command(name = "mdv", version)]
struct Args {
    /// Markdown file to show. Reads standard input when omitted or `-`.
    file: Option<PathBuf>,

    /// Start with the table of contents hidden (toggle with `t`).
    #[arg(long)]
    no_toc: bool,

    /// Don't load or display images.
    #[arg(long)]
    no_images: bool,

    /// Don't use the text sizing protocol for headings.
    #[arg(long)]
    no_text_sizing: bool,

    /// Maximum width of the text column. By default text fills the window.
    #[arg(long)]
    width: Option<u16>,

    /// Color theme. `auto` asks the terminal for its background color.
    #[arg(long, value_enum, default_value_t = ThemeChoice::Auto)]
    theme: ThemeChoice,

    /// Print the laid-out document as plain text and exit (no terminal UI).
    #[arg(long)]
    dump: bool,

    /// Print detected terminal capabilities and exit.
    #[arg(long)]
    probe: bool,
}

fn read_source(file: Option<&PathBuf>) -> Result<Source> {
    match file {
        Some(p) if p.as_os_str() != "-" => Source::from_path(p).with_context(|| format!("can't read {}", p.display())),
        _ => {
            if std::io::stdin().is_terminal() {
                bail!("no file given; usage: mdv FILE.md (or pipe Markdown into mdv)");
            }
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            Ok(Source { path: None, name: "stdin".into(), text, mtime: None })
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.probe {
        let caps = with_terminal(probe::probe)?;
        println!("{caps:#?}");
        return Ok(());
    }
    let source = read_source(args.file.as_ref())?;
    if args.dump || !std::io::stdout().is_terminal() {
        dump(&source, &args);
        return Ok(());
    }
    with_terminal(|| run(source, &args))?
}

// Mouse: 1003 reports motion too (for link hover popups), 1006 = SGR encoding.
const ENTER: &str = "\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[?1000h\x1b[?1003h\x1b[?1006h\x1b[?1004h";
const LEAVE: &str = "\x1b[?1004l\x1b[?1006l\x1b[?1003l\x1b[?1000l\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l";

fn restore() {
    let mut out = std::io::stdout();
    let _ = out.write_all(LEAVE.as_bytes());
    let _ = out.flush();
    let _ = disable_raw_mode();
}

/// Runs `f` in raw mode on the alternate screen, restoring the terminal
/// afterwards, including on panic.
fn with_terminal<T>(f: impl FnOnce() -> T) -> Result<T> {
    enable_raw_mode().context("can't enable raw mode")?;
    let mut out = std::io::stdout();
    out.write_all(ENTER.as_bytes())?;
    out.flush()?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        hook(info);
    }));
    let result = f();
    restore();
    Ok(result)
}

fn run(source: Source, args: &Args) -> Result<()> {
    let mut caps = probe::probe();
    caps.graphics &= !args.no_images;
    caps.text_sizing &= !args.no_text_sizing;
    let dark = match args.theme {
        ThemeChoice::Dark => true,
        ThemeChoice::Light => false,
        ThemeChoice::Auto => caps.bg.is_none_or(|c| c.luminance() < 0.5),
    };
    let theme = Theme::new(dark, caps.bg);
    let images = Images::new(caps.graphics, caps.tmux);
    let opts = Options { toc: !args.no_toc, max_width: args.width.map(|w| w.max(20)) };
    let mut app = App::new(caps, theme, images, source, opts);

    let mut out = std::io::stdout();
    let mut dirty = true;
    let mut last_check = Instant::now();
    while !app.quit {
        if dirty {
            app.draw(&mut out)?;
            dirty = false;
        }
        // Poll faster while a drag rests on an edge so the selection keeps scrolling.
        let timeout = if app.dragging() { 30 } else { 100 };
        if event::poll(Duration::from_millis(timeout))? {
            loop {
                dirty |= app.handle(event::read()?);
                if app.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        dirty |= app.autoscroll();
        if app.images.poll() {
            app.relayout();
            dirty = true;
        }
        if last_check.elapsed() >= Duration::from_millis(750) {
            last_check = Instant::now();
            dirty |= app.check_reload();
            dirty |= app.expire_message();
        }
    }
    let mut cleanup = String::new();
    app.images.cleanup(&mut cleanup);
    out.write_all(cleanup.as_bytes())?;
    out.flush()?;
    Ok(())
}

/// Plain-text rendering of the layout, for pipes and debugging.
fn dump(source: &Source, args: &Args) {
    let doc = doc::parse(&source.text);
    let theme = Theme::new(true, None);
    let hl = Highlighter::new(theme.syntect_theme);
    let no_images = |_: usize| None;
    let env = Env {
        theme: &theme,
        hl: &hl,
        text_sizing: !args.no_text_sizing,
        image_dims: &no_images,
        cell: (10, 20),
        max_image_rows: 40,
        broken_links: &[],
    };
    let width = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80).min(args.width.unwrap_or(u16::MAX)) as usize;
    let layout = layout::layout(&doc, width, &env);
    let mut out = std::io::stdout().lock();
    for line in &layout.lines {
        let text = match line.kind {
            Kind::Heading { scale, .. } => format!("{}  [×{}]", line.plain(), scale.factor()),
            Kind::HeadingCont { .. } => continue,
            Kind::Text => line.plain(),
        };
        if writeln!(out, "{}", text.trim_end()).is_err() {
            return;
        }
    }
}
