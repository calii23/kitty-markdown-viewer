use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Linear blend: `t = 0` gives `self`, `t = 1` gives `other`.
    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let f = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8;
        Rgb(f(self.0, other.0), f(self.1, other.1), f(self.2, other.2))
    }

    pub fn luminance(self) -> f32 {
        (0.2126 * self.0 as f32 + 0.7152 * self.1 as f32 + 0.0722 * self.2 as f32) / 255.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub reverse: bool,
}

impl Style {
    pub fn fg(c: Rgb) -> Style {
        Style { fg: Some(c), ..Style::default() }
    }

    pub fn bold(mut self) -> Style {
        self.bold = true;
        self
    }

    pub fn italic(mut self) -> Style {
        self.italic = true;
        self
    }

    pub fn on(mut self, bg: Rgb) -> Style {
        self.bg = Some(bg);
        self
    }

    /// Writes a full SGR sequence that resets and then applies this style.
    pub fn write_sgr(&self, out: &mut String) {
        out.push_str("\x1b[0");
        if self.bold {
            out.push_str(";1");
        }
        if self.dim {
            out.push_str(";2");
        }
        if self.italic {
            out.push_str(";3");
        }
        if self.underline {
            out.push_str(";4");
        }
        if self.reverse {
            out.push_str(";7");
        }
        if self.strike {
            out.push_str(";9");
        }
        if let Some(Rgb(r, g, b)) = self.fg {
            let _ = write!(out, ";38;2;{r};{g};{b}");
        }
        if let Some(Rgb(r, g, b)) = self.bg {
            let _ = write!(out, ";48;2;{r};{g};{b}");
        }
        out.push('m');
    }
}
