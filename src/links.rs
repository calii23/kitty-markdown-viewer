//! Link classification and opening.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, PartialEq)]
pub enum Target {
    /// `#heading` in the current document.
    Anchor(String),
    /// Another Markdown file, with an optional `#fragment`.
    Doc(PathBuf, Option<String>),
    /// URLs and non-Markdown files, handed to the OS.
    External(String),
    Missing(String),
}

const MARKDOWN_EXTS: [&str; 5] = ["md", "markdown", "mdown", "mkd", "mdx"];

fn has_scheme(s: &str) -> bool {
    match s.find(':') {
        Some(i) if i >= 2 => {
            let scheme = &s[..i];
            scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        _ => false,
    }
}

/// True for anything with a URL scheme (`https:`, `mailto:`, …).
pub fn is_url(s: &str) -> bool {
    has_scheme(s)
}

pub fn classify(raw: &str, base: &Path) -> Target {
    let raw = raw.trim();
    if let Some(anchor) = raw.strip_prefix('#') {
        return Target::Anchor(anchor.to_string());
    }
    if has_scheme(raw) && !raw.starts_with("file:") {
        return Target::External(raw.to_string());
    }
    let raw = raw.strip_prefix("file://").unwrap_or(raw);
    let (path, fragment) = match raw.split_once('#') {
        Some((p, f)) => (p, Some(f.to_string())),
        None => (raw, None),
    };
    let path = percent_decode(path.split('?').next().unwrap_or(path));
    let path = if let Some(home) = path.strip_prefix("~/") {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(home)
    } else {
        base.join(path)
    };
    if !path.exists() {
        return Target::Missing(path.display().to_string());
    }
    let is_markdown = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| MARKDOWN_EXTS.contains(&e.to_ascii_lowercase().as_str()));
    if is_markdown {
        Target::Doc(path, fragment)
    } else if path.is_dir() {
        let readme = ["README.md", "readme.md", "index.md"].iter().map(|n| path.join(n)).find(|p| p.exists());
        match readme {
            Some(p) => Target::Doc(p, fragment),
            None => Target::External(path.display().to_string()),
        }
    } else {
        Target::External(path.display().to_string())
    }
}

/// Opens a URL or file with the system handler (`open` on macOS), or with
/// the command in `$MDV_OPENER` (e.g. `open -a Firefox`) when set.
pub fn open_external(target: &str) -> std::io::Result<()> {
    let default = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let opener = std::env::var("MDV_OPENER").ok().filter(|s| !s.trim().is_empty());
    let mut words = opener.as_deref().unwrap_or(default).split_whitespace();
    let program = words.next().unwrap_or(default);
    Command::new(program)
        .args(words)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_targets() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(classify("#usage", base), Target::Anchor("usage".into()));
        assert_eq!(classify("https://example.com", base), Target::External("https://example.com".into()));
        assert_eq!(classify("mailto:a@b.c", base), Target::External("mailto:a@b.c".into()));
        assert_eq!(classify("README.md#keys", base), Target::Doc(base.join("README.md"), Some("keys".into())));
        assert!(matches!(classify("nope.md", base), Target::Missing(_)));
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("my%20file.md"), "my file.md");
        assert_eq!(percent_decode("100%"), "100%");
    }
}
