//! gpui RSVP reader: launcher window (file picker + WPM slider + Run) and a
//! reader window with pivot markers, WPM slider and free numeric input.
//!
//! Both windows are client-side decorated and use the gpui-kit `TitleBar`
//! (drag-to-move, double-click zoom, minimize / maximize / close); a ghost
//! button in the bar toggles fullscreen.

mod assets;
mod launcher;
mod reader;
mod utils;

use std::path::PathBuf;

use clap::Parser;
use gpui::{App, AppContext, Focusable, SharedString};
use gpui_kit::assets::AllAssets;
use gpui_kit::component::{Theme as KitTheme, ThemeMode};

use core::{frames, load_book, load_theme};

use crate::launcher::LauncherView;
use crate::reader::ReaderView;
use crate::utils::{FONT, UiFont, reader_title, rgb_bytes, win_options};

#[derive(Parser)]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(name = "supersprinter-gui", about = "RSVP speed reader")]
struct Cli {
    /// Book file (.txt); omit to open the launcher window
    file: Option<PathBuf>,
    /// Words per minute
    wpm: Option<u32>,
}

fn main() {
    let cli = Cli::parse();
    let theme = match load_theme() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    };
    let wpm = cli.wpm.unwrap_or(300);
    // With a file argument, go straight to the reader; otherwise open the
    // launcher window.
    let direct = cli.file.map(|path| {
        let text = match load_book(&path) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        };
        (path, text)
    });

    gpui_kit::application()
        .with_assets(AllAssets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            let fam: SharedString = match core::resolve_font(theme) {
                Ok(r) => {
                    if r.fell_back && theme.font != FONT {
                        eprintln!(
                            "warning: theme font {:?} not installed, using {:?}",
                            theme.font, r.family
                        );
                    }
                    r.family.into()
                }
                Err(e) => {
                    eprintln!("error: {e:#}");
                    std::process::exit(1);
                }
            };
            cx.set_global(UiFont(fam.clone()));
            // Map the app's TOML theme onto the gpui-kit theme so the
            // components (titlebar, slider, buttons, input) match the reader.
            let lum = (0.2126 * theme.background[0] as f32
                + 0.7152 * theme.background[1] as f32
                + 0.0722 * theme.background[2] as f32)
                / 255.0;
            KitTheme::update(cx, |t| {
                t.mode = if lum < 0.5 {
                    ThemeMode::Dark
                } else {
                    ThemeMode::Light
                };
                t.colors.background = rgb_bytes(theme.background).into();
                t.colors.foreground = rgb_bytes(theme.text).into();
                t.colors.primary = rgb_bytes(theme.pivot).into();
                t.colors.border = rgb_bytes(theme.arrows).into();
                t.font_family = fam.clone();
            });
            match direct {
                Some((path, text)) => {
                    if frames(&text).next().is_none() {
                        eprintln!("error: no words found in {}", path.display());
                        std::process::exit(1);
                    }
                    let title = reader_title(&path);
                    let opts = win_options(title.clone(), 900., 500., cx);
                    gpui_kit::open_window(opts, cx, move |window, cx| {
                        let view =
                            cx.new(|cx| ReaderView::new(window, text, path, theme, wpm, title, cx));
                        view.focus_handle(cx).focus(window, cx);
                        view
                    })
                    .expect("failed to open window");
                }
                None => {
                    let opts = win_options("Supersprinter".into(), 560., 360., cx);
                    gpui_kit::open_window(opts, cx, move |window, cx| {
                        let view = cx.new(|cx| LauncherView::new(theme, wpm, cx));
                        view.focus_handle(cx).focus(window, cx);
                        view
                    })
                    .expect("failed to open window");
                }
            }
        });
}
