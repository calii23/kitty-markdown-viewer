//! Settings remembered between runs, for all files: whether the table of
//! contents is shown and the width it was resized to.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub toc: bool,
    /// Width set by dragging the contents border; `None` sizes it to fit.
    pub toc_width: Option<u16>,
}

impl Default for State {
    fn default() -> State {
        State { toc: true, toc_width: None }
    }
}

/// `$XDG_STATE_HOME/mdv/state.toml`, or `~/.local/state/mdv/state.toml`.
fn path() -> Option<PathBuf> {
    let nonempty = |v: std::ffi::OsString| (!v.is_empty()).then(|| PathBuf::from(v));
    let base = std::env::var_os("XDG_STATE_HOME")
        .and_then(nonempty)
        .or_else(|| std::env::var_os("HOME").and_then(nonempty).map(|h| h.join(".local/state")))?;
    Some(base.join("mdv").join("state.toml"))
}

pub fn load() -> State {
    path().and_then(|p| std::fs::read_to_string(p).ok()).map(|text| parse(&text)).unwrap_or_default()
}

/// Saves the state, ignoring errors: losing it isn't worth interrupting for.
pub fn save(state: State) {
    let Some(path) = path() else { return };
    let Some(dir) = path.parent() else { return };
    let tmp = path.with_extension("toml.tmp");
    let _ = std::fs::create_dir_all(dir)
        .and_then(|_| std::fs::write(&tmp, format(state)))
        .and_then(|_| std::fs::rename(&tmp, &path));
}

fn parse(text: &str) -> State {
    let Ok(table) = text.parse::<toml::Table>() else { return State::default() };
    State {
        toc: table.get("toc").and_then(|v| v.as_bool()).unwrap_or(true),
        toc_width: table.get("toc_width").and_then(|v| v.as_integer()).and_then(|w| u16::try_from(w).ok()),
    }
}

fn format(state: State) -> String {
    let mut out = format!("toc = {}\n", state.toc);
    if let Some(w) = state.toc_width {
        out.push_str(&format!("toc_width = {w}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for state in [State::default(), State { toc: false, toc_width: Some(31) }] {
            assert_eq!(parse(&format(state)), state);
        }
        assert_eq!(parse("not toml ["), State::default());
    }
}
