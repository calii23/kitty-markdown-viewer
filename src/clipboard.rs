//! Clipboard writes via kitty's OSC 5522 clipboard protocol, with OSC 52 as
//! the fallback for terminals (and tmux) that don't speak it.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    /// OSC 5522. The terminal answers each write with a status reply.
    Kitty,
    /// OSC 52. No reply.
    Osc52,
}

/// Appends the escape sequences that put `text` on the clipboard.
pub fn copy(text: &str, protocol: Protocol, out: &mut String) {
    match protocol {
        Protocol::Kitty => {
            let mime = STANDARD.encode("text/plain");
            out.push_str("\x1b]5522;type=write\x1b\\");
            // Chunks of at most 4096 bytes before encoding.
            for chunk in text.as_bytes().chunks(4096) {
                out.push_str(&format!("\x1b]5522;type=wdata:mime={mime};{}\x1b\\", STANDARD.encode(chunk)));
            }
            out.push_str("\x1b]5522;type=wdata\x1b\\");
        }
        Protocol::Osc52 => {
            out.push_str(&format!("\x1b]52;c;{}\x1b\\", STANDARD.encode(text)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_write_matches_spec_example() {
        let mut out = String::new();
        copy("Hello", Protocol::Kitty, &mut out);
        assert_eq!(
            out,
            "\x1b]5522;type=write\x1b\\\x1b]5522;type=wdata:mime=dGV4dC9wbGFpbg==;SGVsbG8=\x1b\\\x1b]5522;type=wdata\x1b\\"
        );
    }

    #[test]
    fn large_text_is_chunked() {
        let mut out = String::new();
        copy(&"x".repeat(9000), Protocol::Kitty, &mut out);
        assert_eq!(out.matches("type=wdata:mime=").count(), 3);
    }
}
