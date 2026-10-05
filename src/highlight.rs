use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxDefinition, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::style::{Rgb, Style};

/// Grammars missing from bat's set. They live in their own small set:
/// merging them into bat's would rebuild it, adding ~200 ms to startup.
fn extra_syntaxes() -> SyntaxSet {
    let mut builder = SyntaxSet::new().into_builder();
    let prisma = SyntaxDefinition::load_from_str(include_str!("syntaxes/prisma.sublime-syntax"), true, None)
        .expect("bundled Prisma grammar is valid");
    builder.add(prisma);
    builder.build()
}

pub struct Highlighter {
    /// bat's extended syntax set, via two-face.
    syntaxes: SyntaxSet,
    extra: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    pub fn new(theme_name: &str) -> Highlighter {
        let mut themes = ThemeSet::load_defaults().themes;
        let theme = themes.remove(theme_name).or_else(|| themes.into_values().next()).expect("syntect ships themes");
        Highlighter { syntaxes: two_face::syntax::extra_newlines(), extra: extra_syntaxes(), theme }
    }

    /// The syntax for a fence tag, with the set it belongs to.
    fn syntax(&self, lang: &str) -> Option<(&SyntaxReference, &SyntaxSet)> {
        let lang = lang.to_ascii_lowercase();
        // Fence tags people use that aren't a syntax name or file extension.
        let alias = match lang.as_str() {
            "jsx" => "tsx",
            "objc" | "objectivec" | "obj-c" => "objective-c",
            "golang" => "go",
            "shell" | "console" | "shellscript" => "bash",
            "h" => "c",
            "py3" => "python",
            "proto3" => "proto",
            "jsonc" | "json5" => "json",
            other => other,
        };
        [&self.extra, &self.syntaxes].into_iter().find_map(|set| set.find_syntax_by_token(alias).map(|s| (s, set)))
    }

    /// Highlights `code` line by line. Unknown languages come back unstyled
    /// (`fg: None`), so the caller's default code color applies.
    pub fn highlight(&self, code: &str, lang: &str) -> Vec<Vec<(Style, String)>> {
        let Some((syntax, set)) = self.syntax(lang) else {
            return code.lines().map(|l| vec![(Style::default(), l.to_string())]).collect();
        };
        let mut h = HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::new();
        for line in LinesWithEndings::from(code) {
            let ranges = h.highlight_line(line, set).unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_languages_resolve() {
        let h = Highlighter::new("base16-ocean.dark");
        let expect = [
            ("js", "JavaScript"),
            ("jsx", "TypeScriptReact"),
            ("ts", "TypeScript"),
            ("tsx", "TypeScriptReact"),
            ("java", "Java"),
            ("kotlin", "Kotlin"),
            ("rust", "Rust"),
            ("json", "JSON"),
            ("yaml", "YAML"),
            ("toml", "TOML"),
            ("prisma", "Prisma"),
            ("python", "Python"),
            ("ruby", "Ruby"),
            ("c", "C"),
            ("h", "C"),
            ("cpp", "C++"),
            ("proto", "Protocol Buffer"),
            ("css", "CSS"),
            ("scss", "SCSS"),
            ("sass", "Sass"),
            ("html", "HTML"),
            ("sql", "SQL"),
            ("xml", "XML"),
            ("php", "PHP"),
            ("perl", "Perl"),
            ("lua", "Lua"),
            ("objc", "Objective-C"),
            ("go", "Go"),
            ("diff", "Diff"),
            ("bash", "Bourne Again Shell (bash)"),
            ("fish", "Fish"),
        ];
        for (tag, name) in expect {
            assert_eq!(h.syntax(tag).map(|(s, _)| s.name.as_str()), Some(name), "fence tag {tag}");
        }
    }

    #[test]
    fn new_languages_are_highlighted() {
        let h = Highlighter::new("base16-ocean.dark");
        let samples = [
            ("prisma", "model User {\n  id Int @id @default(autoincrement())\n}\n"),
            ("tsx", "const a: number = 1;\nexport const C = () => <div className=\"x\">{a}</div>;\n"),
            ("kotlin", "data class User(val id: Int)\nfun main() = println(\"hi\")\n"),
            ("toml", "[package]\nname = \"mdv\"\nversion = 1\n"),
            ("diff", "--- a\n+++ b\n-old\n+new\n"),
            ("fish", "for f in *.md\n    echo $f # comment\nend\n"),
            ("proto", "syntax = \"proto3\";\nmessage A { int32 id = 1; }\n"),
            ("scss", "$c: red;\n.a { color: $c; }\n"),
        ];
        for (lang, code) in samples {
            let colors: std::collections::HashSet<_> =
                h.highlight(code, lang).iter().flatten().map(|(s, _)| s.fg).collect();
            assert!(colors.len() >= 3, "{lang}: expected several token colors, got {colors:?}");
        }
    }
}
