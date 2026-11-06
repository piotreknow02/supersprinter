//! Terminal RSVP reader + mp4 render subcommand.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame as TuiFrame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style, Styled},
    text::{Line, Span},
    widgets::Paragraph,
};

use core::{Frame, Theme, first_pos, frame_at, frames, load_book, load_theme, next_pos, seek_pos};

#[derive(Parser)]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(name = "supersprinter-cli", about = "RSVP speed reader")]
struct Cli {
    /// Book file (.txt)
    file: Option<PathBuf>,
    /// Words per minute
    #[arg(default_value = "300")]
    wpm: u32,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Render the book to an mp4 file
    Render {
        /// Book file (.txt)
        file: PathBuf,

        /// Output mp4 (default: <file-stem>.mp4 next to the input)
        output: Option<PathBuf>,

        /// Words per minute
        #[arg(default_value = "300")]
        wpm: u32,
    },
}

/// Restores the terminal on drop so every exit path (including panic unwind)
/// leaves the user's shell usable.
struct TermGuard;

impl Drop for TermGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

fn to_color(rgb: core::Rgb) -> Color {
    Color::Rgb(rgb[0], rgb[1], rgb[2])
}

fn draw(
    tui: &mut TuiFrame,
    frame: &Frame<'_>,
    theme: &Theme,
    wpm: u32,
    idx: usize,
    total: usize,
    paused: bool,
) {
    let area = tui.area();
    let center_col = area.width / 2;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(1), // marker above
            Constraint::Length(1), // word
            Constraint::Length(1), // marker below
            Constraint::Fill(1),
            Constraint::Length(1), // status
        ])
        .split(area);

    let word: &str = frame.word.unwrap_or("");
    let pivot = frame.pivot;

    // Pad so the pivot glyph lands on center_col; words too wide start at 0.
    let pad = center_col.saturating_sub(pivot as u16);

    let marker = |ch: char| {
        let col = if word.is_empty() { center_col } else { pad + pivot as u16 };
        Line::from(vec![
            " ".repeat(col as usize).into(),
            ch.set_style(Style::default().fg(to_color(theme.arrows))),
        ])
    };

    let word_line = if word.is_empty() {
        Line::from("")
    } else {
        // Byte offsets of the pivot char so the spans borrow the word (zero alloc).
        let (pb, pl) = word
            .char_indices()
            .nth(pivot)
            .map(|(i, c)| (i, c.len_utf8()))
            .unwrap_or((word.len(), 0));
        let text = Style::default().fg(to_color(theme.text));
        let pivot_style = Style::default().fg(to_color(theme.pivot)).add_modifier(Modifier::BOLD);
        Line::from(vec![
            " ".repeat(pad as usize).into(),
            Span::from(&word[..pb]).set_style(text),
            Span::from(&word[pb..pb + pl]).set_style(pivot_style),
            Span::from(&word[pb + pl..]).set_style(text),
        ])
    };

    tui.render_widget(Paragraph::new(marker('▼')), chunks[1]);
    tui.render_widget(Paragraph::new(word_line), chunks[2]);
    tui.render_widget(Paragraph::new(marker('▲')), chunks[3]);

    let status = format!(
        " {}{} / {} · {} wpm · space: {} · ←/→: step · +/-: wpm · q: quit",
        if paused { "⏸ " } else { "" },
        idx + 1,
        total,
        wpm,
        if paused { "play" } else { "pause" },
    );
    tui.render_widget(
        Paragraph::new(Line::from(status)).style(Style::default().fg(to_color(theme.text))),
        chunks[5],
    );
}

fn read_terminal(path: &Path, mut wpm: u32, theme: &Theme) -> Result<()> {
    let text = load_book(path)?;
    let total = frames(&text).count();
    if total == 0 {
        anyhow::bail!("no words found in {}", path.display());
    }

    let mut terminal = ratatui::init();
    let _guard = TermGuard;

    let mut pos = first_pos(&text);
    let mut playing = true;
    let mut quit = false;

    while !quit {
        let Some(frame) = frame_at(&text, pos) else { break };
        terminal.draw(|tui| draw(tui, &frame, theme, wpm, pos.idx, total, !playing))?;

        let tick_ms = 60_000 / wpm.max(1) * frame.ticks;
        let timeout = if playing { Some(Duration::from_millis(tick_ms as u64)) } else { None };

        if event::poll(timeout.unwrap_or(Duration::MAX))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => quit = true,
                    KeyCode::Char(' ') => playing = !playing,
                    KeyCode::Left => pos = seek_pos(&text, pos, pos.idx.saturating_sub(1)),
                    KeyCode::Right => {
                        if pos.idx + 1 < total {
                            pos = next_pos(&text, pos);
                        } else {
                            quit = true;
                        }
                    }
                    KeyCode::Char('+') | KeyCode::Char('=') => wpm = (wpm + 25).min(1200),
                    KeyCode::Char('-') => wpm = wpm.saturating_sub(25).max(20),
                    _ => {}
                },
                Event::Resize(_, _) => {}
                _ => {}
            }
        } else if playing {
            // Timeout: advance to the next frame; end of stream exits 0.
            if pos.idx + 1 < total {
                pos = next_pos(&text, pos);
            } else {
                quit = true;
            }
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = cli_main() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn cli_main() -> Result<()> {
    let cli = Cli::parse();
    let theme = load_theme()?;

    match cli.command {
        Some(Command::Render { file, output, wpm }) => {
            let out = output.unwrap_or_else(|| file.with_extension("mp4"));
            let text = load_book(&file)?;
            eprintln!("rendering {} wpm → {}", wpm, out.display());
            render::render_video(&text, &out, wpm, &theme)?;
            eprintln!("done: {}", out.display());
            Ok(())
        }
        None => {
            let file = cli.file.context("missing <FILE> argument")?;
            let wpm = cli.wpm;
            read_terminal(&file, wpm, &theme)
        }
    }
}
