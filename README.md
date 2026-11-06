# Supersprinter

RSVP (rapid serial visual presentation) speed reader. Reads a plain-text book
and presents one word at a time with a fixed pivot (ORP) letter, so the eye
never has to move. Ships a terminal reader, a graphical (gpui) reader and an
MP4 renderer.

## Crates

| Crate | What it is |
|---|---|
| `crates/core` | No-UI logic: book loading (`load_book`), frame generation (`frames` — a lazy, allocation-free view over the text: pivot at ~30% of the word, Glance-style pacing: extra ticks after sentence ends, commas, long words; `first_pos`/`frame_at`/`next_pos`/`seek_pos` give readers a seekable cursor without materializing the stream), theme loading from TOML. |
| `crates/render` | Renders each frame to an RGBA image (ab_glyph + image) and encodes 1280×720/24fps MP4 via libx264 (ffmpeg-next). |
| `crates/cli` | `supersprinter-cli`: ratatui terminal reader (`supersprinter-cli book.txt 300`) plus `supersprinter-cli render book.txt [out.mp4] [wpm]`. |
| `crates/gui` | `supersprinter-gui`: gpui desktop app. Launcher window (file picker, WPM slider, Run) and reader window (canvas-rendered word with ▼/▲ pivot markers, play/pause, slider + numeric WPM input, progress). Client-side window decorations with working close/minimize/fullscreen and edge resize. |

## Theme

`theme.toml` with `background`, `text`, `arrows` as `#rrggbb`, optional
`pivot` (`#rrggbb`, defaults to `text`) and optional `font` (font family
name, default `monospace`; applied by the GUI and MP4 renderer — the CLI
uses the terminal font). Search order: `./theme.toml`,
`~/.config/supersprinter/theme.toml`, `~/.supersprinter/theme.toml`. With
no file found, the default theme embedded at build time
(`themes/default.toml`) is used. An invalid file is a hard error.

## Building

Requires a Rust stable toolchain (edition 2024). The `render`/`gui` crates
need system libraries: ffmpeg development headers (libavcodec etc.) and, for
`gui`, GPUI's Linux deps (libxkbcommon, wayland, fontconfig, etc. — see the
[gpui README](https://github.com/zed-industries/zed)).

```sh
cargo build --workspace

# terminal reader
cargo run -p supersprinter-cli -- book.txt 300
# render an mp4
cargo run -p supersprinter-cli -- render book.txt out.mp4 300
# GUI: launcher window, or open a book directly
cargo run -p supersprinter-gui
cargo run -p supersprinter-gui -- book.txt 300
```

GUI keys: <kbd>space</kbd> play/pause, <kbd>←</kbd>/<kbd>→</kbd> step,
<kbd>+</kbd>/<kbd>-</kbd> WPM, <kbd>q</kbd>/<kbd>Esc</kbd> close.
