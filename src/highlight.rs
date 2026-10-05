use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::style::{Rgb, Style};

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    pub fn new(theme_name: &str) -> Highlighter {
        let mut themes = ThemeSet::load_defaults().themes;
        let theme = themes.remove(theme_name).or_else(|| themes.into_values().next()).expect("syntect ships themes");
        Highlighter { syntaxes: SyntaxSet::load_defaults_newlines(), theme }
    }

    fn syntax(&self, lang: &str) -> Option<&SyntaxReference> {
        let lang = lang.to_ascii_lowercase();
        let alias = match lang.as_str() {
            "ts" | "typescript" | "tsx" | "jsx" | "mjs" | "cjs" | "json5" => "js",
            "sh" | "zsh" | "fish" | "shell" | "console" => "bash",
            "py3" => "python",
            "yml" => "yaml",
            "rs" => "rust",
            "golang" => "go",
            "c++" | "hpp" => "cpp",
            "kt" => "java",
            "toml" | "conf" => "ini",
            other => other,
        };
        self.syntaxes.find_syntax_by_token(alias)
    }

    /// Highlights `code` line by line. Unknown languages come back unstyled
    /// (`fg: None`), so the caller's default code color applies.
    pub fn highlight(&self, code: &str, lang: &str) -> Vec<Vec<(Style, String)>> {
        let Some(syntax) = self.syntax(lang) else {
            return code.lines().map(|l| vec![(Style::default(), l.to_string())]).collect();
        };
        let mut h = HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::new();
        for line in LinesWithEndings::from(code) {
            let ranges = h.highlight_line(line, &self.syntaxes).unwrap_or_default();
            let segs = ranges
                .into_iter()
                .map(|(st, text)| {
                    let c = st.foreground;
                    let style = Style {
                        fg: Some(Rgb(c.r, c.g, c.b)),
                        bold: st.font_style.contains(FontStyle::BOLD),
                        italic: st.font_style.contains(FontStyle::ITALIC),
                        ..Style::default()
                    };
                    (style, text.trim_end_matches(['\n', '\r']).to_string())
                })
                .filter(|(_, t)| !t.is_empty())
                .collect();
            out.push(segs);
        }
        out
    }
}
