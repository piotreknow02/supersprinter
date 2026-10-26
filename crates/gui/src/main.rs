//! gpui RSVP reader: launcher window (file picker + WPM slider + Run) and a
//! reader window with pivot markers, WPM slider and free numeric input.
//!
//! Both windows use client-side decorations (like Zed/zaku): gpui's Wayland
//! backend echoes whatever decoration mode we request, so requesting Server
//! on GNOME yields a window with no titlebar at all. We request Client and
//! paint our own titlebar (close / minimize / fullscreen, drag-to-move,
//! double-click to zoom) plus 6px resize strips wired to
//! `start_window_resize`.
//!
//! The reader area is a single `canvas`: the word is shaped with the pivot
//! glyph centered on the viewport midline, and the ▼/▲ markers are painted as
//! filled triangles at exactly that glyph's x — so markers and letters can
//! never drift apart. Font size scales with the viewport.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use clap::Parser;
use gpui::{
    App, AppContext, Application, Bounds, Context, CursorStyle, Decorations, FocusHandle,
    Focusable, Global, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement, PathBuilder, PathPromptOptions, Pixels, Render, ResizeEdge,
    StatefulInteractiveElement, Styled, Task, TextRun, Tiling, Timer, TitlebarOptions, Window,
    WindowBounds, WindowControlArea, WindowDecorations, WindowOptions, canvas, div, font, point,
    prelude::FluentBuilder, px, rgb, size,
};

use core::{Frame, Theme, build_frames, load_book, load_theme};

const SLIDER_MIN: u32 = 10;
const SLIDER_MAX: u32 = 600;
const FONT: &str = "monospace";
/// Height of the client-side titlebar.
const BAR_H: f32 = 34.;
/// Width of the invisible resize strips along the window edges.
const EDGE: f32 = 6.;

#[derive(Parser)]
#[command(name = "supersprinter-gui", about = "RSVP speed reader")]
struct Cli {
    /// Book file (.txt); omit to open the launcher window
    file: Option<PathBuf>,
    /// Words per minute
    wpm: Option<u32>,
}

fn rgb_bytes(c: core::Rgb) -> gpui::Rgba {
    rgb(((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32)
}

/// True while a titlebar drag is armed (mouse down on the titlebar, waiting
/// for movement to hand the move to the compositor).
struct TitleDrag(bool);
impl Global for TitleDrag {}

/// The monospace family actually available on this system. gpui's Linux font
/// lookup matches fontdb family names exactly, so the fontconfig alias
/// "monospace" resolves to nothing and silently falls back to a PROPORTIONAL
/// font — which is why per-char advance math misaligned the pivot arrows.
/// Resolved once at startup from `all_font_names()`.
struct MonoFamily(String);
impl Global for MonoFamily {}

const MONO_CANDIDATES: &[&str] = &[
    "JetBrains Mono",
    "Fira Mono",
    "Hack",
    "Cascadia Mono",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "Ubuntu Mono",
    "monospace",
];

fn mono_family(cx: &App) -> String {
    cx.try_global::<MonoFamily>()
        .map(|m| m.0.clone())
        .unwrap_or_else(|| FONT.to_string())
}

/// Close `window`, quitting the app if it is the last window.
fn close_window(window: &mut Window, cx: &mut App) {
    if cx.windows().len() <= 1 {
        cx.quit();
    } else {
        window.remove_window();
    }
}

fn win_options(title: String, w: f32, h: f32, cx: &mut App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(w), px(h)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        window_decorations: Some(WindowDecorations::Client),
        is_resizable: true,
        is_movable: true,
        window_min_size: Some(size(px(480.), px(320.))),
        ..Default::default()
    }
}

/// Client-side titlebar: drag area (move, double-click zoom) + minimize,
/// fullscreen and close buttons.
fn title_bar(
    id: &'static str,
    theme: Theme,
    title: String,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let border = rgb_bytes(theme.arrows);
    let text = rgb_bytes(theme.text);
    div()
        .id(id)
        .flex_none()
        .h(px(BAR_H))
        .flex()
        .flex_row()
        .items_center()
        .bg(rgb_bytes(theme.background))
        .border_b_1()
        .border_color(border)
        .window_control_area(WindowControlArea::Drag)
        .child(
            div()
                .id("tb-drag")
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .pl_3()
                .text_color(text)
                .child(title)
                .on_mouse_down(MouseButton::Left, |_e, _w, cx| {
                    cx.set_global(TitleDrag(true));
                })
                .on_mouse_move(|_e, window, cx| {
                    if cx.try_global::<TitleDrag>().is_some_and(|g| g.0) {
                        cx.set_global(TitleDrag(false));
                        window.start_window_move();
                    }
                })
                .on_mouse_up(MouseButton::Left, |_e, _w, cx| {
                    cx.set_global(TitleDrag(false));
                })
                .on_click(|e, window, _cx| {
                    if e.click_count() == 2 {
                        window.zoom_window();
                    }
                }),
        )
        .child(
            div()
                .id("tb-min")
                .cursor_pointer()
                .px_3()
                .h_full()
                .flex()
                .items_center()
                .text_color(text)
                .child("—")
                .on_click(|_e, window, cx| {
                    cx.stop_propagation();
                    window.minimize_window();
                }),
        )
        .child(
            div()
                .id("tb-fs")
                .cursor_pointer()
                .px_3()
                .h_full()
                .flex()
                .items_center()
                .text_color(text)
                .child("☐")
                .on_click(|_e, window, cx| {
                    cx.stop_propagation();
                    window.toggle_fullscreen();
                }),
        )
        .child(
            div()
                .id("tb-close")
                .cursor_pointer()
                .px_3()
                .h_full()
                .flex()
                .items_center()
                .text_color(text)
                .child("✕")
                .on_click(move |_e, window, cx| {
                    cx.stop_propagation();
                    on_close(window, cx);
                }),
        )
}

/// Invisible edge strips that start a compositor resize on mouse-down.
/// Edges the window is tiled against are skipped.
fn resize_strips(tiling: Tiling) -> Vec<impl IntoElement> {
    let e = px(EDGE);
    let corner = px(EDGE * 2.);
    let mk = |id: &'static str, edge: ResizeEdge, cursor: CursorStyle| {
        div()
            .id(id)
            .absolute()
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, move |_e, window, _cx| {
                window.start_window_resize(edge);
            })
    };
    let mut v = Vec::new();
    if !tiling.top {
        v.push(
            mk("rz-top", ResizeEdge::Top, CursorStyle::ResizeUp)
                .top_0()
                .left_0()
                .right_0()
                .h(e),
        );
    }
    if !tiling.bottom {
        v.push(
            mk("rz-bot", ResizeEdge::Bottom, CursorStyle::ResizeDown)
                .bottom_0()
                .left_0()
                .right_0()
                .h(e),
        );
    }
    if !tiling.left {
        v.push(
            mk("rz-left", ResizeEdge::Left, CursorStyle::ResizeLeft)
                .top_0()
                .bottom_0()
                .left_0()
                .w(e),
        );
    }
    if !tiling.right {
        v.push(
            mk("rz-right", ResizeEdge::Right, CursorStyle::ResizeRight)
                .top_0()
                .bottom_0()
                .right_0()
                .w(e),
        );
    }
    if !tiling.top && !tiling.left {
        v.push(
            mk("rz-tl", ResizeEdge::TopLeft, CursorStyle::ResizeUpLeftDownRight)
                .top_0()
                .left_0()
                .size(corner),
        );
    }
    if !tiling.top && !tiling.right {
        v.push(
            mk("rz-tr", ResizeEdge::TopRight, CursorStyle::ResizeUpRightDownLeft)
                .top_0()
                .right_0()
                .size(corner),
        );
    }
    if !tiling.bottom && !tiling.left {
        v.push(
            mk("rz-bl", ResizeEdge::BottomLeft, CursorStyle::ResizeUpRightDownLeft)
                .bottom_0()
                .left_0()
                .size(corner),
        );
    }
    if !tiling.bottom && !tiling.right {
        v.push(
            mk("rz-br", ResizeEdge::BottomRight, CursorStyle::ResizeUpLeftDownRight)
                .bottom_0()
                .right_0()
                .size(corner),
        );
    }
    v
}

/// WPM slider (10–600): click or drag sets the value via `setter`.
fn wpm_slider<T: 'static>(
    cx: &mut Context<T>,
    theme: Theme,
    wpm: u32,
    setter: fn(&mut T, u32, &mut Context<T>),
) -> impl IntoElement {
    let cell: Rc<RefCell<Option<Bounds<Pixels>>>> = Rc::new(RefCell::new(None));
    let for_prepaint = cell.clone();
    let frac = (wpm.min(SLIDER_MAX) as f32 - SLIDER_MIN as f32)
        / (SLIDER_MAX - SLIDER_MIN) as f32;
    let track_color = rgb_bytes(theme.arrows);
    let thumb_color = rgb_bytes(theme.text);

    div()
        .id("wpm-slider")
        .w(px(240.))
        .h(px(20.))
        .child(
            canvas(
                move |bounds, _window, _cx| {
                    *for_prepaint.borrow_mut() = Some(bounds);
                    (bounds, frac, track_color, thumb_color)
                },
                move |_bounds, (b, f, track_c, thumb_c), window, _cx| {
                    let track = Bounds {
                        origin: point(b.origin.x, b.origin.y + px(9.)),
                        size: size(b.size.width, px(2.)),
                    };
                    window.paint_quad(gpui::fill(track, track_c));
                    let thumb_x = b.origin.x + b.size.width * f.clamp(0., 1.);
                    window.paint_quad(gpui::fill(
                        Bounds {
                            origin: point(thumb_x - px(6.), b.origin.y + px(4.)),
                            size: size(px(12.), px(12.)),
                        },
                        thumb_c,
                    ));
                },
            )
            .size_full(),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener({
                let cell = cell.clone();
                move |this, e: &MouseDownEvent, _w, cx| {
                    let Some(b) = *cell.borrow() else { return };
                    let frac = ((e.position.x - b.origin.x) / b.size.width).clamp(0., 1.);
                    let w = (SLIDER_MIN as f32 + frac * (SLIDER_MAX - SLIDER_MIN) as f32).round() as u32;
                    setter(this, w, cx);
                }
            }),
        )
        .on_mouse_move(cx.listener({
            let cell = cell.clone();
            move |this, e: &MouseMoveEvent, _w, cx| {
                if e.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                let Some(b) = *cell.borrow() else { return };
                let frac = ((e.position.x - b.origin.x) / b.size.width).clamp(0., 1.);
                let w = (SLIDER_MIN as f32 + frac * (SLIDER_MAX - SLIDER_MIN) as f32).round() as u32;
                setter(this, w, cx);
            }
        }))
}

// ---------------------------------------------------------------------------
// Launcher window
// ---------------------------------------------------------------------------

struct LauncherView {
    theme: Theme,
    wpm: u32,
    path: Option<PathBuf>,
    error: Option<String>,
    focus: FocusHandle,
    picker: Task<()>,
}

impl LauncherView {
    fn new(theme: Theme, wpm: u32, cx: &mut Context<Self>) -> Self {
        Self {
            theme,
            wpm: wpm.max(1),
            path: None,
            error: None,
            focus: cx.focus_handle(),
            picker: Task::ready(()),
        }
    }

    fn set_wpm(&mut self, wpm: u32, cx: &mut Context<Self>) {
        self.wpm = wpm.clamp(1, 4000);
        cx.notify();
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a book (.txt)".into()),
        });
        self.picker = cx.spawn(async move |this, cx| {
            let paths = rx.await.ok().and_then(|r| r.ok()).flatten();
            let Some(path) = paths.and_then(|mut ps| ps.pop()) else {
                return;
            };
            let _ = this.update(cx, |v, cx| {
                v.path = Some(path);
                v.error = None;
                cx.notify();
            });
        });
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            self.error = Some("choose a file first".into());
            cx.notify();
            return;
        };
        match load_book(&path).map(|t| build_frames(&t)) {
            Ok(frames) if !frames.is_empty() => {
                let theme = self.theme;
                let wpm = self.wpm;
                let stem = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let title = format!("Supersprinter — {stem}");
                let opts = win_options(title.clone(), 900., 500., cx);
                let opened = cx.open_window(opts, move |window, cx| {
                    let view =
                        cx.new(|cx| ReaderView::new(frames, theme, wpm, title, cx));
                    view.focus_handle(cx).focus(window);
                    view
                });
                if opened.is_err() {
                    self.error = Some("failed to open reader window".into());
                }
            }
            Ok(_) => self.error = Some(format!("no words found in {}", path.display())),
            Err(e) => self.error = Some(format!("{e:#}")),
        }
        cx.notify();
    }
}

impl Focusable for LauncherView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for LauncherView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let tiling = match window.window_decorations() {
            Decorations::Client { tiling } => Some(tiling),
            Decorations::Server => None,
        };

        let mut root = div()
            .id("launcher")
            .key_context("launcher")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, w, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => close_window(w, cx),
                    "enter" => this.run(cx),
                    _ => {}
                }
            }))
            .on_click(cx.listener(|this, _e, window, _cx| this.focus.focus(window)))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb_bytes(theme.background))
            .text_color(rgb_bytes(theme.text));

        if let Some(tiling) = tiling {
            root = root.child(title_bar("launcher-titlebar", theme, "Supersprinter".into(), close_window));
            for strip in resize_strips(tiling) {
                root = root.child(strip);
            }
        }

        let path_label = match &self.path {
            Some(p) => p.display().to_string(),
            None => "no file chosen".into(),
        };
        let border = rgb_bytes(theme.arrows);

        let content = div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .border_1()
                            .border_color(border)
                            .font_family(FONT)
                            .child(path_label),
                    )
                    .child(
                        div()
                            .id("browse")
                            .cursor_pointer()
                            .px_3()
                            .py_1()
                            .border_1()
                            .border_color(border)
                            .on_click(cx.listener(|this, _e, _w, cx| this.browse(cx)))
                            .child("Browse…"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(wpm_slider(cx, theme, self.wpm, Self::set_wpm))
                    .child(div().font_family(FONT).child(format!("{} wpm", self.wpm))),
            )
            .child(
                div()
                    .id("run")
                    .cursor_pointer()
                    .px_6()
                    .py_2()
                    .border_1()
                    .border_color(border)
                    .font_family(FONT)
                    .text_color(rgb_bytes(theme.pivot))
                    .on_click(cx.listener(|this, _e, _w, cx| this.run(cx)))
                    .child("▶ Run"),
            )
            .when_some(self.error.clone(), |this, err| {
                this.child(
                    div()
                        .font_family(FONT)
                        .text_color(rgb_bytes(theme.pivot))
                        .child(err),
                )
            });

        root.child(content)
    }
}

// ---------------------------------------------------------------------------
// Reader window
// ---------------------------------------------------------------------------

struct ReaderView {
    frames: Vec<Frame>,
    idx: usize,
    playing: bool,
    wpm: u32,
    theme: Theme,
    title: String,
    task: Task<()>,
    root_focus: FocusHandle,
    input_focus: FocusHandle,
    wpm_buf: String,
}

impl ReaderView {
    fn new(
        frames: Vec<Frame>,
        theme: Theme,
        wpm: u32,
        title: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = ReaderView {
            frames,
            idx: 0,
            playing: true,
            wpm: wpm.max(1),
            theme,
            title,
            task: Task::ready(()),
            root_focus: cx.focus_handle(),
            input_focus: cx.focus_handle(),
            wpm_buf: String::new(),
        };
        this.restart_task(cx);
        this
    }

    /// (Re)start the playback timer task; replacing the field cancels the old one.
    fn restart_task(&mut self, cx: &mut Context<Self>) {
        if !self.playing {
            self.task = Task::ready(());
            return;
        }
        self.task = cx.spawn(async move |this, cx| {
            loop {
                let delay = this
                    .read_with(cx, |v, _cx| {
                        if !v.playing || v.idx + 1 >= v.frames.len() {
                            None
                        } else {
                            Some(60_000u64 / v.wpm.max(1) as u64 * v.frames[v.idx].ticks as u64)
                        }
                    })
                    .unwrap_or(None);
                let Some(ms) = delay else { break };
                Timer::after(Duration::from_millis(ms)).await;
                let keep_going = this
                    .update(cx, |v, cx| {
                        if !v.playing {
                            return false;
                        }
                        v.idx += 1;
                        cx.notify();
                        v.idx + 1 < v.frames.len()
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        });
    }

    fn set_playing(&mut self, playing: bool, cx: &mut Context<Self>) {
        self.playing = playing;
        self.restart_task(cx);
        cx.notify();
    }

    fn step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let len = self.frames.len() as i64;
        self.idx = ((self.idx as i64 + delta).clamp(0, (len - 1).max(0))) as usize;
        self.restart_task(cx);
        cx.notify();
    }

    fn set_wpm(&mut self, wpm: u32, cx: &mut Context<Self>) {
        self.wpm = wpm.clamp(1, 4000);
        self.restart_task(cx);
        cx.notify();
    }

    fn on_reader_key(&mut self, e: &KeyDownEvent, w: &mut Window, cx: &mut Context<Self>) {
        match e.keystroke.key.as_str() {
            "space" => self.set_playing(!self.playing, cx),
            "left" => self.step(-1, cx),
            "right" => self.step(1, cx),
            "q" | "escape" => close_window(w, cx),
            "+" | "=" => self.set_wpm((self.wpm + 25).min(1200), cx),
            "-" | "_" => self.set_wpm(self.wpm.saturating_sub(25).max(20), cx),
            _ => {}
        }
    }

    fn on_input_key(&mut self, e: &KeyDownEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let key = e.keystroke.key.as_str();
        match key {
            "backspace" => {
                self.wpm_buf.pop();
            }
            "enter" => {
                if let Ok(v) = self.wpm_buf.parse::<u32>() {
                    self.set_wpm(v, cx);
                }
                self.wpm_buf.clear();
            }
            _ => {
                if let Some(c) = e.keystroke.key_char.as_deref()
                    && c.len() == 1
                    && c.chars().next().unwrap().is_ascii_digit()
                    && self.wpm_buf.len() < 4
                {
                    self.wpm_buf.push_str(c);
                }
            }
        }
        // Never let keystrokes typed into the WPM box reach the reader
        // context (space would otherwise toggle play while typing a number).
        cx.stop_propagation();
        cx.notify();
    }
}

impl Focusable for ReaderView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.root_focus.clone()
    }
}

impl Render for ReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let tiling = match window.window_decorations() {
            Decorations::Client { tiling } => Some(tiling),
            Decorations::Server => None,
        };
        let frame = &self.frames[self.idx];
        let word: String = frame.word.clone().unwrap_or_default();
        let chars: Vec<char> = word.chars().collect();
        let pivot = frame.pivot.min(chars.len().saturating_sub(1));

        let text_c: gpui::Hsla = rgb_bytes(theme.text).into();
        let pivot_c: gpui::Hsla = rgb_bytes(theme.pivot).into();
        let arrow_c = rgb_bytes(theme.arrows);

        // Byte lengths of the three color runs (pivot glyph highlighted).
        let before: String = chars[..pivot].iter().collect();
        let pivot_s: String = chars.get(pivot).into_iter().collect();
        let after: String = chars[pivot + 1..].iter().collect();

        let fam = mono_family(cx);
        let reader = {
            let word_shape = word.clone();
            let font_family = fam.clone();
            let pivot_byte = before.len();
            let pivot_char = chars.get(pivot).copied();
            canvas(
                move |bounds, window, _cx| {
                    if word_shape.is_empty() {
                        return None;
                    }
                    let ts = window.text_system();
                    let max_w = bounds.size.width * 0.85;
                    let run = |len: usize, color: gpui::Hsla| TextRun {
                        len,
                        font: font(font_family.clone()),
                        color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let shape = |size: Pixels| {
                        ts.shape_line(
                            word_shape.clone().into(),
                            size,
                            &[
                                run(before.len(), text_c),
                                run(pivot_s.len(), pivot_c),
                                run(after.len(), text_c),
                            ],
                            None,
                        )
                    };
                    // Scale the font to the viewport: start at ~16% of the
                    // height, shrink until the shaped word fits 85% of width.
                    let mut font_size = (bounds.size.height * 0.16).clamp(px(16.), px(140.));
                    let mut line = shape(font_size);
                    while line.width > max_w && font_size > px(16.) {
                        font_size *= 0.92;
                        line = shape(font_size);
                    }
                    let line_height = font_size * 1.25;
                    // Exact pivot-glyph placement from the shaped layout:
                    // its left edge and advance (gap to the next glyph). No
                    // assumption about per-char advance widths.
                    let mut pivot_x = None;
                    let mut pivot_adv = None;
                    for run in line.runs.iter() {
                        for (i, g) in run.glyphs.iter().enumerate() {
                            if g.index == pivot_byte {
                                pivot_x = Some(g.position.x);
                                pivot_adv = Some(if i + 1 < run.glyphs.len() {
                                    run.glyphs[i + 1].position.x - g.position.x
                                } else {
                                    line.width - g.position.x
                                });
                                break;
                            }
                        }
                        if pivot_x.is_some() {
                            break;
                        }
                    }
                    let (pivot_x, pivot_adv) = match (pivot_x, pivot_adv) {
                        (Some(x), Some(a)) => (x, a),
                        _ => {
                            // Glyph not found (e.g. ligature): fall back to
                            // uniform advance math.
                            let cw = ts
                                .ch_advance(ts.resolve_font(&font(font_family.clone())), font_size)
                                .unwrap_or(font_size * 0.6);
                            (cw * pivot as f32, cw)
                        }
                    };
                    // Advance-box center is not the visual center: glyphs
                    // like `c` or `!` sit off-center inside their advance.
                    // Use the glyph's ink bounds so the arrows align with
                    // what the eye sees.
                    let fid = ts.resolve_font(&font(font_family.clone()));
                    let pivot_center = pivot_char
                        .and_then(|c| ts.typographic_bounds(fid, font_size, c).ok())
                        .map(|tb| pivot_x + tb.origin.x + tb.size.width / 2.)
                        .unwrap_or(pivot_x + pivot_adv / 2.);
                    Some((line, pivot_center, pivot_adv, line_height))
                },
                move |bounds, calc, window, cx| {
                    let Some((line, pivot_center, pivot_adv, line_height)) = calc else {
                        return;
                    };
                    // The pivot glyph's center must land exactly on the
                    // viewport midline; markers paint at that same x.
                    let mid_x = bounds.origin.x + bounds.size.width / 2.;
                    let origin_x = mid_x - pivot_center;
                    let origin_y = bounds.origin.y + (bounds.size.height - line_height) / 2.;
                    let _ = line.paint(point(origin_x, origin_y), line_height, window, cx);
                    let pivot_x = origin_x + pivot_center;

                    let gap = px(6.);
                    let tw = (pivot_adv * 0.9).max(px(10.));
                    let th = tw * 0.7;
                    // ▼ above the word, apex pointing down at the pivot glyph.
                    let mut pb = PathBuilder::fill();
                    pb.move_to(point(pivot_x - tw / 2., origin_y - gap - th));
                    pb.line_to(point(pivot_x + tw / 2., origin_y - gap - th));
                    pb.line_to(point(pivot_x, origin_y - gap));
                    pb.close();
                    if let Ok(path) = pb.build() {
                        window.paint_path(path, arrow_c);
                    }
                    // ▲ below the word, apex pointing up.
                    let mut pb = PathBuilder::fill();
                    pb.move_to(point(pivot_x, origin_y + line_height + gap));
                    pb.line_to(point(pivot_x - tw / 2., origin_y + line_height + gap + th));
                    pb.line_to(point(pivot_x + tw / 2., origin_y + line_height + gap + th));
                    pb.close();
                    if let Ok(path) = pb.build() {
                        window.paint_path(path, arrow_c);
                    }
                },
            )
            .flex_1()
        };

        let input = div()
            .id("wpm-input")
            .track_focus(&self.input_focus)
            .key_context("wpm-input")
            .on_key_down(cx.listener(Self::on_input_key))
            .on_click(cx.listener(|this, _e, window, cx| {
                // Without this the root div's bubbling on_click refocuses
                // root_focus and the input loses focus immediately.
                cx.stop_propagation();
                this.input_focus.focus(window);
            }))
            .px_2()
            .py_1()
            .border_1()
            .border_color(rgb_bytes(theme.arrows))
            .text_size(px(16.))
            .text_color(rgb_bytes(theme.text))
            .font_family(fam.clone())
            .child(if self.wpm_buf.is_empty() {
                "type wpm…".into()
            } else {
                format!("{}▏", self.wpm_buf)
            });

        let play_label = if self.playing { "⏸ pause" } else { "▶ play" };
        let progress = format!("{}/{}", self.idx + 1, self.frames.len());
        let wpm_label = format!("{} wpm", self.wpm);

        // Controls at the top, directly under the titlebar.
        let controls = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap_4()
            .p_3()
            .text_color(rgb_bytes(theme.text))
            .child(
                div()
                    .id("play-toggle")
                    .cursor_pointer()
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(rgb_bytes(theme.arrows))
                    .on_click(cx.listener(|this, _, _w, cx| this.set_playing(!this.playing, cx)))
                    .child(play_label),
            )
            .child(wpm_slider(cx, theme, self.wpm, Self::set_wpm))
            .child(input)
            .child(div().child(wpm_label))
            .child(div().child(progress));

        let mut root = div()
            .id("root")
            .key_context("reader")
            .track_focus(&self.root_focus)
            .on_key_down(cx.listener(Self::on_reader_key))
            .size_full()
            .relative()
            .bg(rgb_bytes(theme.background))
            .flex()
            .flex_col()
            .on_click(cx.listener(|this, _e, window, _cx| {
                this.root_focus.focus(window);
            }));

        if let Some(tiling) = tiling {
            let title = self.title.clone();
            root = root.child(title_bar("reader-titlebar", theme, title, |w, cx| {
                close_window(w, cx);
            }));
            for strip in resize_strips(tiling) {
                root = root.child(strip);
            }
        }

        root.child(controls).child(reader)
    }
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
        let frames = match load_book(&path) {
            Ok(text) => build_frames(&text),
            Err(e) => {
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        };
        (path, frames)
    });

    Application::new().run(move |cx: &mut App| {
        cx.set_global(TitleDrag(false));
        let names = cx.text_system().all_font_names();
        let fam = MONO_CANDIDATES
            .iter()
            .find(|c| names.iter().any(|n| n == *c))
            .map(|s| s.to_string())
            .unwrap_or_else(|| "monospace".to_string());
        cx.set_global(MonoFamily(fam));
        match &direct {
            Some((path, frames)) => {
                if frames.is_empty() {
                    eprintln!("error: no words found in {}", path.display());
                    std::process::exit(1);
                }
                let stem = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let title = format!("Supersprinter — {stem}");
                let frames = frames.clone();
                let theme_for_view = theme;
                let opts = win_options(title.clone(), 900., 500., cx);
                cx.open_window(opts, move |window, cx| {
                    let view =
                        cx.new(|cx| ReaderView::new(frames, theme_for_view, wpm, title, cx));
                    view.focus_handle(cx).focus(window);
                    view
                })
                .expect("failed to open window");
            }
            None => {
                let opts = win_options("Supersprinter".into(), 560., 360., cx);
                cx.open_window(opts, |window, cx| {
                    let view = cx.new(|cx| LauncherView::new(theme, wpm, cx));
                    view.focus_handle(cx).focus(window);
                    view
                })
                .expect("failed to open window");
            }
        }
    });
}
