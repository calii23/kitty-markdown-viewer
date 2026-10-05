use crate::style::Rgb;

/// Color palette. Based on Catppuccin (Mocha for dark, Latte for light), with
/// surface colors derived from the terminal's real background when known.
pub struct Theme {
    pub fg: Rgb,
    pub bg: Rgb,
    pub headings: [Rgb; 6],
    pub link: Rgb,
    pub code_fg: Rgb,
    pub code_bg: Rgb,
    pub math: Rgb,
    pub quote: Rgb,
    pub dim: Rgb,
    pub border: Rgb,
    pub accent: Rgb,
    pub on_accent: Rgb,
    pub bar_bg: Rgb,
    pub bar_fg: Rgb,
    pub sel_bg: Rgb,
    /// Background of selected text.
    pub select_bg: Rgb,
    pub search_bg: Rgb,
    pub search_cur_bg: Rgb,
    pub search_fg: Rgb,
    pub check: Rgb,
    pub note: Rgb,
    pub tip: Rgb,
    pub important: Rgb,
    pub warning: Rgb,
    pub caution: Rgb,
    pub syntect_theme: &'static str,
}

impl Theme {
    pub fn new(dark: bool, terminal_bg: Option<Rgb>) -> Theme {
        if dark {
            let bg = terminal_bg.unwrap_or(Rgb(30, 30, 46));
            let fg = Rgb(205, 214, 244);
            Theme {
                fg,
                bg,
                headings: [
                    Rgb(203, 166, 247),
                    Rgb(137, 180, 250),
                    Rgb(148, 226, 213),
                    Rgb(166, 227, 161),
                    Rgb(249, 226, 175),
                    Rgb(250, 179, 135),
                ],
                link: Rgb(116, 199, 236),
                code_fg: Rgb(245, 194, 231),
                code_bg: bg.mix(fg, 0.07),
                math: Rgb(250, 179, 135),
                quote: Rgb(147, 153, 178),
                dim: Rgb(127, 132, 156),
                border: bg.mix(fg, 0.28),
                accent: Rgb(203, 166, 247),
                on_accent: Rgb(30, 30, 46),
                bar_bg: bg.mix(fg, 0.12),
                bar_fg: Rgb(186, 194, 222),
                sel_bg: bg.mix(fg, 0.18),
                select_bg: bg.mix(Rgb(137, 180, 250), 0.45),
                search_bg: Rgb(110, 95, 60),
                search_cur_bg: Rgb(249, 226, 175),
                search_fg: Rgb(30, 30, 46),
                check: Rgb(166, 227, 161),
                note: Rgb(137, 180, 250),
                tip: Rgb(166, 227, 161),
                important: Rgb(203, 166, 247),
                warning: Rgb(249, 226, 175),
                caution: Rgb(243, 139, 168),
                syntect_theme: "base16-ocean.dark",
            }
        } else {
            let bg = terminal_bg.unwrap_or(Rgb(239, 241, 245));
            let fg = Rgb(76, 79, 105);
            Theme {
                fg,
                bg,
                headings: [
                    Rgb(136, 57, 239),
                    Rgb(30, 102, 245),
                    Rgb(23, 146, 153),
                    Rgb(64, 160, 43),
                    Rgb(223, 142, 29),
                    Rgb(254, 100, 11),
                ],
                link: Rgb(32, 159, 181),
                code_fg: Rgb(234, 118, 203),
                code_bg: bg.mix(fg, 0.07),
                math: Rgb(254, 100, 11),
                quote: Rgb(108, 111, 133),
                dim: Rgb(140, 143, 161),
                border: bg.mix(fg, 0.3),
                accent: Rgb(136, 57, 239),
                on_accent: Rgb(239, 241, 245),
                bar_bg: bg.mix(fg, 0.1),
                bar_fg: Rgb(76, 79, 105),
                sel_bg: bg.mix(fg, 0.15),
                select_bg: bg.mix(Rgb(30, 102, 245), 0.25),
                search_bg: Rgb(250, 225, 160),
                search_cur_bg: Rgb(223, 142, 29),
                search_fg: Rgb(30, 30, 46),
                check: Rgb(64, 160, 43),
                note: Rgb(30, 102, 245),
                tip: Rgb(64, 160, 43),
                important: Rgb(136, 57, 239),
                warning: Rgb(223, 142, 29),
                caution: Rgb(210, 15, 57),
                syntect_theme: "InspiredGitHub",
            }
        }
    }
}
