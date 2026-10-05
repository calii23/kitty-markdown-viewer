//! Background image loading (files, http(s), data URIs, SVG) and kitty
//! image bookkeeping.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use image::{DynamicImage, ImageFormat, RgbaImage};
use resvg::{tiny_skia, usvg};

use crate::kitty;
use crate::links::percent_decode;
use crate::style::Rgb;

/// Larger images are downscaled before transmission.
const MAX_SIDE: u32 = 2048;

pub struct Loaded {
    /// Natural size in pixels, before any downscaling.
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

enum Entry {
    Loading,
    Ready(Arc<Loaded>),
    Failed(String),
}

pub struct Images {
    enabled: bool,
    tmux: bool,
    entries: HashMap<String, Entry>,
    /// Kitty image id per (source, cols, rows): each size gets its own image
    /// so its single virtual placement can't be confused with another.
    kitty: HashMap<(String, u16, u16), u32>,
    next_id: u32,
    tx: Sender<(String, Result<Loaded, String>)>,
    rx: Receiver<(String, Result<Loaded, String>)>,
}

impl Images {
    pub fn new(enabled: bool, tmux: bool) -> Images {
        let (tx, rx) = channel();
        // Spread ids so two viewers in one kitty window are unlikely to clash.
        let next_id = (std::process::id().wrapping_mul(2_654_435_761) % 0x00E0_0000) + 0x0001_0000;
        Images { enabled, tmux, entries: HashMap::new(), kitty: HashMap::new(), next_id, tx, rx }
    }

    /// Turns an image reference into a loader key: URLs stay as they are,
    /// paths become absolute.
    pub fn resolve(url: &str, base: &Path) -> String {
        if ["http://", "https://", "data:", "mermaid:"].iter().any(|p| url.starts_with(p)) {
            return url.to_string();
        }
        let path = url.strip_prefix("file://").unwrap_or(url);
        let path = percent_decode(path.split(['?', '#']).next().unwrap_or(path));
        base.join(path).to_string_lossy().into_owned()
    }

    pub fn request(&mut self, key: &str) {
        if !self.enabled || self.entries.contains_key(key) {
            return;
        }
        self.entries.insert(key.to_string(), Entry::Loading);
        let tx = self.tx.clone();
        let key = key.to_string();
        std::thread::spawn(move || {
            let result = load(&key);
            let _ = tx.send((key, result));
        });
    }

    /// Renders a Mermaid diagram in the background, stored under `key`.
    pub fn request_mermaid(&mut self, key: &str, source: &str, dark: bool, bg: Rgb) {
        if !self.enabled || self.entries.contains_key(key) {
            return;
        }
        self.entries.insert(key.to_string(), Entry::Loading);
        let tx = self.tx.clone();
        let (key, source) = (key.to_string(), source.to_string());
        std::thread::spawn(move || {
            let result = render_mermaid(&source, dark, bg);
            let _ = tx.send((key, result));
        });
    }

    pub fn error(&self, key: &str) -> Option<String> {
        match self.entries.get(key) {
            Some(Entry::Failed(e)) => Some(e.clone()),
            _ => None,
        }
    }

    pub fn dims(&self, key: &str) -> Option<(u32, u32)> {
        match self.entries.get(key) {
            Some(Entry::Ready(img)) => Some((img.width, img.height)),
            _ => None,
        }
    }

    pub fn loading(&self) -> usize {
        self.entries.values().filter(|e| matches!(e, Entry::Loading)).count()
    }

    /// Collects finished loads. Returns true if anything became ready.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((key, result)) = self.rx.try_recv() {
            let entry = match result {
                Ok(img) => {
                    changed = true;
                    Entry::Ready(Arc::new(img))
                }
                Err(e) => Entry::Failed(e),
            };
            self.entries.insert(key, entry);
        }
        changed
    }

    /// Returns the kitty image id for `key` at this size, transmitting the
    /// image and creating its virtual placement on first use.
    pub fn ensure(&mut self, key: &str, cols: u16, rows: u16, out: &mut String) -> Option<u32> {
        let k = (key.to_string(), cols, rows);
        if let Some(id) = self.kitty.get(&k) {
            return Some(*id);
        }
        let Some(Entry::Ready(img)) = self.entries.get(key) else { return None };
        let id = self.next_id;
        self.next_id += 1;
        kitty::transmit_png(id, &img.png, self.tmux, out);
        kitty::virtual_placement(id, cols, rows, self.tmux, out);
        self.kitty.insert(k, id);
        Some(id)
    }

    /// Frees all images we transmitted.
    pub fn cleanup(&mut self, out: &mut String) {
        for id in self.kitty.values() {
            kitty::delete_image(*id, self.tmux, out);
        }
        self.kitty.clear();
    }
}

fn load(key: &str) -> Result<Loaded, String> {
    let bytes = fetch(key)?;
    let looks_svg = key.to_ascii_lowercase().split(['?', '#']).next().is_some_and(|p| p.ends_with(".svg"))
        || key.starts_with("data:image/svg")
        || String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).contains("<svg");
    let (width, height, img) = if looks_svg {
        render_svg(&bytes)?
    } else {
        let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
        (img.width(), img.height(), img)
    };
    encode(width, height, img)
}

/// Mermaid source to SVG (mermaid-rs-renderer), then to PNG like any SVG.
fn render_mermaid(source: &str, dark: bool, bg: Rgb) -> Result<Loaded, String> {
    use mermaid_rs_renderer::{RenderOptions, Theme};
    let mut opts = RenderOptions::modern();
    if dark {
        opts.theme = Theme::dark();
    }
    opts.theme.background = format!("#{:02x}{:02x}{:02x}", bg.0, bg.1, bg.2);
    let svg = mermaid_rs_renderer::render_with_options(source, opts)
        .map_err(|e| e.to_string().lines().next().unwrap_or("invalid diagram").to_string())?;
    let (_, _, img) = render_svg(svg.as_bytes())?;
    // Report the 2x raster size so diagrams come out readable, not thumbnails.
    encode(img.width(), img.height(), img)
}

fn encode(width: u32, height: u32, img: DynamicImage) -> Result<Loaded, String> {
    let img = if img.width() > MAX_SIDE || img.height() > MAX_SIDE {
        img.resize(MAX_SIDE, MAX_SIDE, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut png = Vec::new();
    DynamicImage::ImageRgba8(img.to_rgba8())
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(Loaded { width, height, png })
}

fn fetch(key: &str) -> Result<Vec<u8>, String> {
    if key.starts_with("http://") || key.starts_with("https://") {
        let agent: ureq::Agent =
            ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(20))).build().into();
        return agent
            .get(key)
            .header("User-Agent", concat!("mdv/", env!("CARGO_PKG_VERSION")))
            .call()
            .and_then(|mut r| r.body_mut().read_to_vec())
            .map_err(|e| e.to_string());
    }
    if let Some(rest) = key.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',').ok_or("malformed data URI")?;
        return if meta.ends_with(";base64") {
            STANDARD.decode(data.trim()).map_err(|e| e.to_string())
        } else {
            Ok(percent_decode(data).into_bytes())
        };
    }
    std::fs::read(key).map_err(|e| e.to_string())
}

fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

/// Rasterizes an SVG at twice its natural size so it stays crisp when the
/// terminal scales it.
fn render_svg(bytes: &[u8]) -> Result<(u32, u32, DynamicImage), String> {
    let opt = usvg::Options { fontdb: fonts(), ..usvg::Options::default() };
    let tree = usvg::Tree::from_data(bytes, &opt).map_err(|e| e.to_string())?;
    let size = tree.size();
    let (w, h) = (size.width().max(1.0), size.height().max(1.0));
    let scale = (2.0f32).min(MAX_SIDE as f32 / w.max(h));
    let (pw, ph) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph).ok_or("SVG has no size")?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let mut rgba = RgbaImage::new(pw, ph);
    for (dst, src) in rgba.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        *dst = image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Ok((w.round() as u32, h.round() as u32, DynamicImage::ImageRgba8(rgba)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_mermaid_to_png() {
        let img = render_mermaid("flowchart LR\n  A --> B --> C\n", true, Rgb(30, 30, 46)).unwrap();
        assert!(img.width > img.height, "LR flowchart should be wide: {}x{}", img.width, img.height);
        assert!(img.png.starts_with(b"\x89PNG"));
    }

    #[test]
    fn mermaid_errors_are_one_line() {
        let err = render_mermaid("not a diagram at all", true, Rgb(0, 0, 0)).err().unwrap();
        assert!(!err.contains('\n'));
    }
}
