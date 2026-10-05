//! Terminal capability detection. Must run in raw mode, inside the alternate
//! screen, before crossterm starts reading input.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::kitty;
use crate::style::Rgb;

#[derive(Clone, Debug)]
pub struct Caps {
    pub graphics: bool,
    pub text_sizing: bool,
    /// Cell size in pixels (width, height).
    pub cell: (u16, u16),
    pub bg: Option<Rgb>,
    pub tmux: bool,
    /// Supports kitty's OSC 5522 clipboard protocol.
    pub clipboard: bool,
}

#[derive(Debug, PartialEq)]
enum Reply {
    Cursor(u16, u16),
    CellSize(u16, u16),
    Background(Rgb),
    Graphics,
    /// DECRQM reply for mode 5522: set or reset means supported.
    Clipboard(bool),
    DeviceAttributes,
}

pub fn probe() -> Caps {
    let tmux = std::env::var_os("TMUX").is_some();
    // Text sizing: if the cursor advances 2 cells for `w=2` and 2 more for
    // `s=2` (each a single space), the protocol is supported.
    let mut query = String::from("\x1b[H\x1b[6n\x1b]66;w=2; \x1b\\\x1b[6n\x1b]66;s=2; \x1b\\\x1b[6n");
    query.push_str("\x1b[16t\x1b]11;?\x1b\\\x1b[?5522$p");
    if !tmux {
        query.push_str(&kitty::query(false));
    }
    // Every terminal answers DA1, so it marks the end of the replies.
    query.push_str("\x1b[c");

    let replies = exchange(&query).unwrap_or_default();
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b[2J\x1b[H");
    let _ = out.flush();

    let cursors: Vec<u16> =
        replies.iter().filter_map(|r| if let Reply::Cursor(_, col) = r { Some(*col) } else { None }).collect();
    let text_sizing = cursors.len() >= 3 && cursors[1] == cursors[0] + 2 && cursors[2] == cursors[1] + 2;

    let mut cell = replies.iter().find_map(|r| if let Reply::CellSize(w, h) = r { Some((*w, *h)) } else { None });
    if cell.is_none()
        && let Ok(ws) = crossterm::terminal::window_size()
        && ws.width > 0
        && ws.columns > 0
    {
        cell = Some((ws.width / ws.columns, ws.height / ws.rows.max(1)));
    }
    let bg = replies.iter().find_map(|r| if let Reply::Background(c) = r { Some(*c) } else { None });

    let graphics = if tmux { tmux_graphics() } else { replies.contains(&Reply::Graphics) && placeholders_likely() };

    let clipboard = !tmux && replies.contains(&Reply::Clipboard(true));
    Caps { graphics, text_sizing, cell: cell.filter(|c| c.0 > 0 && c.1 > 0).unwrap_or((10, 20)), bg, tmux, clipboard }
}

/// WezTerm answers the graphics query but lacks Unicode placeholders.
fn placeholders_likely() -> bool {
    std::env::var("TERM_PROGRAM").map(|t| t != "WezTerm").unwrap_or(true)
}

/// Inside tmux the outer terminal's replies don't reach us, so ask tmux
/// what the client terminal is and whether passthrough is on.
fn tmux_graphics() -> bool {
    let run = |args: &[&str]| {
        Command::new("tmux")
            .args(args)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let term = run(&["display-message", "-p", "#{client_termname}"]);
    let passthrough = run(&["show-options", "-gv", "allow-passthrough"]);
    (term.contains("kitty") || term.contains("ghostty")) && (passthrough == "on" || passthrough == "all")
}

fn exchange(query: &str) -> std::io::Result<Vec<Reply>> {
    let mut tty = File::options().read(true).write(true).open("/dev/tty")?;
    tty.write_all(query.as_bytes())?;
    tty.flush()?;

    let deadline = Instant::now() + Duration::from_millis(1500);
    let mut buf = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut pfd = libc::pollfd { fd: tty.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let ready = unsafe { libc::poll(&mut pfd, 1, left.as_millis() as libc::c_int) };
        if ready <= 0 {
            break;
        }
        let mut chunk = [0u8; 1024];
        let n = tty.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if parse(&buf).contains(&Reply::DeviceAttributes) {
            break;
        }
    }
    Ok(parse(&buf))
}

fn parse(buf: &[u8]) -> Vec<Reply> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] != 0x1b {
            i += 1;
            continue;
        }
        match buf[i + 1] {
            b'[' => {
                let start = i + 2;
                let mut j = start;
                while j < buf.len() && !(0x40..=0x7e).contains(&buf[j]) {
                    j += 1;
                }
                if j >= buf.len() {
                    break;
                }
                let params = String::from_utf8_lossy(&buf[start..j]);
                let nums: Vec<u16> = params.trim_start_matches('?').split(';').filter_map(|p| p.parse().ok()).collect();
                match buf[j] {
                    b'R' if nums.len() == 2 => out.push(Reply::Cursor(nums[0], nums[1])),
                    b't' if nums.len() == 3 && nums[0] == 6 => out.push(Reply::CellSize(nums[2], nums[1])),
                    b'c' if params.starts_with('?') => out.push(Reply::DeviceAttributes),
                    b'y' if params.starts_with("?5522;") => {
                        let ps = params.trim_start_matches("?5522;").trim_end_matches('$');
                        out.push(Reply::Clipboard(matches!(ps, "1" | "2")));
                    }
                    _ => {}
                }
                i = j + 1;
            }
            b']' | b'_' => {
                let start = i + 2;
                let mut j = start;
                let mut end = None;
                while j < buf.len() {
                    if buf[j] == 0x07 {
                        end = Some((j, j + 1));
                        break;
                    }
                    if buf[j] == 0x1b && buf.get(j + 1) == Some(&b'\\') {
                        end = Some((j, j + 2));
                        break;
                    }
                    j += 1;
                }
                let Some((body_end, next)) = end else { break };
                let body = String::from_utf8_lossy(&buf[start..body_end]);
                if buf[i + 1] == b'_' && body.starts_with('G') {
                    out.push(Reply::Graphics);
                } else if let Some(rgb) = body.strip_prefix("11;rgb:") {
                    let parts: Vec<u8> =
                        rgb.split('/').filter_map(|p| u8::from_str_radix(p.get(..2)?, 16).ok()).collect();
                    if parts.len() == 3 {
                        out.push(Reply::Background(Rgb(parts[0], parts[1], parts[2])));
                    }
                }
                i = next;
            }
            _ => i += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_replies() {
        let buf =
            b"\x1b[1;1R\x1b[1;3R\x1b[1;5R\x1b[6;20;10t\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[?5522;2$y\x1b_Gi=31;OK\x1b\\\x1b[?62;22c";
        let r = parse(buf);
        assert_eq!(
            r,
            vec![
                Reply::Cursor(1, 1),
                Reply::Cursor(1, 3),
                Reply::Cursor(1, 5),
                Reply::CellSize(10, 20),
                Reply::Background(Rgb(0x1e, 0x1e, 0x2e)),
                Reply::Clipboard(true),
                Reply::Graphics,
                Reply::DeviceAttributes,
            ]
        );
    }
}
