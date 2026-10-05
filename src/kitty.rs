//! Escape sequence encoders for the kitty graphics protocol (with Unicode
//! placeholders) and the kitty text sizing protocol (OSC 66).

use std::fmt::Write;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use unicode_width::UnicodeWidthChar;

use crate::diacritics::DIACRITICS;
use crate::style::Rgb;

/// Image placeholder character, see "Unicode placeholders" in the kitty docs.
const PLACEHOLDER: char = '\u{10EEEE}';

/// Largest row/column index a placeholder can address.
pub const MAX_PLACEHOLDER_CELLS: u16 = DIACRITICS.len() as u16 - 1;

/// Wraps a graphics command in an APC, and in tmux passthrough when needed.
fn apc(body: &str, tmux: bool) -> String {
    let seq = format!("\x1b_G{body}\x1b\\");
    if tmux { format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b")) } else { seq }
}

/// The graphics protocol support query, answered before the DA1 response.
pub fn query(tmux: bool) -> String {
    apc("i=31,s=1,v=1,a=q,t=d,f=24;AAAA", tmux)
}

/// Transmits PNG data under `id` without displaying it. `q=2` keeps the
/// terminal from writing responses back into our input stream.
pub fn transmit_png(id: u32, png: &[u8], tmux: bool, out: &mut String) {
    let b64 = STANDARD.encode(png);
    let chunks: Vec<&[u8]> = b64.as_bytes().chunks(4096).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        let payload = std::str::from_utf8(chunk).expect("base64 is ASCII");
        let body = if i == 0 {
            format!("a=t,f=100,t=d,i={id},q=2,m={more};{payload}")
        } else {
            format!("m={more},q=2;{payload}")
        };
        out.push_str(&apc(&body, tmux));
    }
}

/// Creates a virtual placement that placeholder cells can refer to. The
/// terminal fits the image into `cols` x `rows` cells, keeping its aspect.
pub fn virtual_placement(id: u32, cols: u16, rows: u16, tmux: bool, out: &mut String) {
    out.push_str(&apc(&format!("a=p,U=1,i={id},c={cols},r={rows},q=2;"), tmux));
}

/// Deletes the image and frees its data.
pub fn delete_image(id: u32, tmux: bool, out: &mut String) {
    out.push_str(&apc(&format!("a=d,d=I,i={id},q=2;"), tmux));
}

/// The foreground color that encodes an image id (ids are kept below 2^24).
pub fn id_color(id: u32) -> Rgb {
    Rgb((id >> 16) as u8, (id >> 8) as u8, id as u8)
}

/// Writes one row of placeholder cells. The caller must have set the
/// foreground color to `id_color(id)`.
pub fn placeholder_row(row: u16, cols: u16, out: &mut String) {
    let row = DIACRITICS[row.min(MAX_PLACEHOLDER_CELLS) as usize];
    for col in 0..cols.min(MAX_PLACEHOLDER_CELLS + 1) {
        out.push(PLACEHOLDER);
        out.push(row);
        out.push(DIACRITICS[col as usize]);
    }
}

/// A text size for OSC 66. `n/d` is an optional fractional scale applied
/// inside the `s` x `s` cell block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scale {
    pub s: u8,
    pub n: u8,
    pub d: u8,
}

impl Scale {
    pub const ONE: Scale = Scale { s: 1, n: 0, d: 0 };

    pub fn is_one(&self) -> bool {
        *self == Scale::ONE
    }

    pub fn rows(&self) -> usize {
        self.s as usize
    }

    fn fractional(&self) -> bool {
        self.n > 0 && self.d > self.n
    }

    /// Effective glyph size relative to normal text.
    pub fn factor(&self) -> f32 {
        if self.fractional() { self.s as f32 * self.n as f32 / self.d as f32 } else { self.s as f32 }
    }

    /// Splits text into OSC 66 payloads with their width in (unscaled) cells.
    ///
    /// Fractionally scaled glyphs still advance in whole `s`-sized cells, so
    /// we pack several characters into one sequence with an explicit `w`,
    /// choosing chunk boundaries where the scaled width is a whole number.
    fn chunks(&self, text: &str) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut acc = 0usize;
        if !self.fractional() {
            for ch in text.chars() {
                cur.push(ch);
                acc += ch.width().unwrap_or(0);
                if cur.len() > 1000 {
                    out.push((std::mem::take(&mut cur), acc));
                    acc = 0;
                }
            }
            if !cur.is_empty() {
                out.push((cur, acc));
            }
            return out;
        }
        let (n, d) = (self.n as usize, self.d as usize);
        let scaled = |acc: usize| (acc * n).div_ceil(d);
        for ch in text.chars() {
            cur.push(ch);
            acc += ch.width().unwrap_or(0);
            if acc > 0 && ((acc * n).is_multiple_of(d) || scaled(acc + 2) > 7) {
                out.push((std::mem::take(&mut cur), scaled(acc)));
                acc = 0;
            }
        }
        if !cur.is_empty() {
            out.push((cur, scaled(acc).max(1)));
        }
        out
    }

    /// Width in terminal cells that `text` occupies at this scale.
    pub fn width(&self, text: &str) -> usize {
        self.chunks(text).iter().map(|(_, w)| w).sum::<usize>() * self.s as usize
    }

    /// Writes `text` at this scale. Styling (SGR) applies as usual.
    pub fn render(&self, text: &str, out: &mut String) {
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        for (chunk, w) in self.chunks(&clean) {
            if self.fractional() {
                let _ = write!(out, "\x1b]66;s={}:n={}:d={}:w={w}:v=2;{chunk}\x1b\\", self.s, self.n, self.d);
            } else {
                let _ = write!(out, "\x1b]66;s={};{chunk}\x1b\\", self.s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_chunks_pack_without_gaps() {
        let s = Scale { s: 2, n: 3, d: 4 };
        let chunks = s.chunks("abcdefghij");
        assert_eq!(chunks[0], ("abcd".to_string(), 3));
        assert_eq!(chunks[1], ("efgh".to_string(), 3));
        assert_eq!(chunks[2], ("ij".to_string(), 2));
        assert_eq!(s.width("abcdefghij"), 16);
    }

    #[test]
    fn full_scale_width() {
        let s = Scale { s: 2, n: 0, d: 0 };
        assert_eq!(s.width("Hello"), 10);
    }

    #[test]
    fn tmux_passthrough_doubles_escapes() {
        let s = apc("a=d", true);
        assert_eq!(s, "\x1bPtmux;\x1b\x1b_Ga=d\x1b\x1b\\\x1b\\");
    }
}
