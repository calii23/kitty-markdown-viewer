# mdv: Markdown viewer for kitty

`mdv` is a full-screen terminal Markdown viewer written in Rust. It's built for
[kitty](https://sw.kovidgoyal.net/kitty/) and uses kitty's terminal protocols:

- **Images** through the [graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).
  `mdv` uses Unicode placeholders, so images scroll and clip like text and also work in tmux.
  It handles PNG, JPEG, GIF, WebP, BMP and SVG, from local files, `http(s)` URLs and `data:` URIs.
- **Mermaid diagrams**: ` ```mermaid ` code blocks are rendered to images in-process with
  [mermaid-rs-renderer](https://github.com/1jehuang/mermaid-rs-renderer) (no Node or browser
  needed) and shown inline. Click a diagram to copy its source. Diagrams that fail to parse
  fall back to their source with the error shown.
- **Big headings** through the [text sizing protocol](https://sw.kovidgoyal.net/kitty/text-sizing-protocol/):
  H1 renders at 2× and H2 at 1.5×.
- **Links** you can use: `#anchors` jump to the heading, links to other Markdown files open
  in the viewer (with back navigation), and everything else goes to macOS `open`
  (`xdg-open` on Linux). External links are also emitted as OSC 8 hyperlinks.
  Hovering a link shows where it goes, and links to missing local files or
  headings are drawn in red.
- **Table of contents** sidebar that highlights the section you're reading. Click an entry to jump to it.
- GitHub-flavored Markdown: tables, task lists, strikethrough, footnotes, alerts
  (`> [!NOTE]`), definition lists, math, front matter, and inline HTML (`<img>`, `<br>`, `<kbd>`, …).
- Syntax highlighting for fenced code blocks.
- Copying through kitty's [clipboard protocol](https://sw.kovidgoyal.net/kitty/clipboard/):
  click a code block to copy its source, or drag to select text (like tmux copy mode).
  Falls back to OSC 52 on other terminals and inside tmux.
- Search, live reload when the file changes, mouse support, dark and light themes.

When the terminal doesn't support a protocol, `mdv` falls back: headings render at
normal size with color, and images show as clickable alt text.

## Install

```sh
cargo install --path .
```

## Usage

```sh
mdv README.md
mdv examples/demo.md
cat notes.md | mdv
mdv --dump README.md     # print the laid-out text, no UI
mdv --probe              # show detected terminal capabilities
```

Set `MDV_OPENER` (e.g. `open -a Firefox`) to choose what opens external links.

Options: `--no-toc`, `--no-images`, `--no-text-sizing`, `--width N` (cap the text
column; by default it fills the window), `--theme auto|dark|light`.

## Keys

| Key                     | Action                              |
|-------------------------|-------------------------------------|
| `j` `k` / `↓` `↑`       | Scroll by line                      |
| `d` `u`                 | Half page down / up                 |
| `space` `b` / PgDn PgUp | Page down / up                      |
| `g` `G` / Home End      | Top / bottom                        |
| `]` `[`                 | Next / previous heading             |
| `1`–`9`                 | Jump to top-level section           |
| `Tab` `Shift-Tab`       | Focus next / previous link          |
| `Enter`                 | Follow the focused link             |
| Hover                   | Show a link's target                |
| Click a code block      | Copy its source to the clipboard    |
| Drag                    | Select text; copied on release      |
| Click                   | Follow a link, or jump via the contents |
| `Backspace` `h` / right-click | Go back                       |
| `/` `n` `N`             | Search, next / previous match       |
| `t`                     | Toggle the table of contents        |
| `r`                     | Reload                              |
| `Esc`                   | Clear selection, focus or search    |
| `?`                     | Help                                |
| `q`                     | Quit                                |

## tmux

Text sizing doesn't work inside tmux. Images do, as long as passthrough is on:

```tmux
set -g allow-passthrough on
```

## How it works

| Module         | Job                                                                   |
|----------------|-----------------------------------------------------------------------|
| `doc.rs`       | Parses Markdown with pulldown-cmark into a block/inline model         |
| `layout.rs`    | Lays the document out into terminal lines for a width                 |
| `render.rs`    | Draws a frame inside a synchronized update                            |
| `kitty.rs`     | Encodes graphics commands, placeholders and OSC 66 sized text         |
| `probe.rs`     | Detects terminal capabilities (CPR, DA1, OSC 11, CSI 16 t)            |
| `images.rs`    | Loads and decodes images on background threads                        |
| `app.rs`       | State, keys, mouse, links, search and history                         |
