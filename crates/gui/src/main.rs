//! gpui RSVP reader: launcher window (file picker + WPM slider + Run) and a
//! reader window with pivot markers, WPM slider and free numeric input.
//!
//! Both windows are client-side decorated and use the gpui-kit `TitleBar`
//! (drag-to-move, double-click zoom, minimize / maximize / close); a ghost
//! button in the bar toggles fullscreen.
//!
//! The reader area is a single `canvas`: the word is shaped with the pivot
//! glyph centered on the viewport midline, and the ▼/▲ markers are painted as
//! filled triangles at exactly that glyph's x — so markers and letters can
//! never drift apart. Font size scales with the viewport.

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, Focusable, Global, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, PathBuilder, PathPromptOptions, Pixels, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Task, TextAlign, TextRun,
    Window, WindowBounds, WindowDecorations, WindowOptions, canvas, div, font, point,
    prelude::FluentBuilder, px, size,
};
use gpui_kit::assets::{AllAssets, IconName};
use gpui_kit::component::{
    Disableable, Theme as KitTheme, ThemeMode, TitleBar,
    button::{Button, ButtonVariants},
    input::{InputEvent, InputState, NumberInput},
    slider::{Slider, SliderEvent, SliderState},
};

use core::{
    Frame, FramePos, Theme, first_pos, frame_at, frames, load_book, load_theme, next_pos, seek_pos,
};

const SLIDER_MIN: u32 = 10;
const SLIDER_MAX: u32 = 600;
const FONT: &str = core::DEFAULT_FONT;

#[derive(Parser)]
#[command(name = "supersprinter-gui", about = "RSVP speed reader")]
struct Cli {
    /// Book file (.txt); omit to open the launcher window
    file: Option<PathBuf>,
    /// Words per minute
    wpm: Option<u32>,
}

fn rgb_bytes(c: core::Rgb) -> gpui::Rgba {
    let [r, g, b] = c;
    gpui::Rgba {
        r: (r as f32) / 255.0,
        g: (g as f32) / 255.0,
        b: (b as f32) / 255.0,
        a: 1.0,
    }
}

/// The font family actually used, resolved once at startup by
/// [`core::resolve_font`] (fontdb): the theme's `font` when it is installed,
/// otherwise a monospace fallback. gpui's Linux font lookup matches fontdb
/// family names exactly, so the fontconfig alias "monospace" resolves to
/// nothing and silently falls back to a PROPORTIONAL font — which is why
/// per-char advance math misaligned the pivot arrows.
struct UiFont(SharedString);
impl Global for UiFont {}

fn ui_font(cx: &App) -> SharedString {
    cx.try_global::<UiFont>()
        .map(|m| m.0.clone())
        .unwrap_or_else(|| SharedString::new_static(FONT))
}

/// Close `window`, quitting the app if it is the last window.
fn close_window(window: &mut Window, cx: &mut App) {
    if cx.windows().len() <= 1 {
        cx.quit();
    } else {
        window.remove_window();
    }
}

fn win_options(title: SharedString, w: f32, h: f32, cx: &mut App) -> WindowOptions {
    let mut titlebar = TitleBar::title_bar_options();
    titlebar.title = Some(title);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(w), px(h)),
            cx,
        ))),
        titlebar: Some(titlebar),
        // TitleBar only paints its min/max/close under client-side
        // decorations; gpui's Wayland backend echoes whatever mode we ask for.
        window_decorations: Some(WindowDecorations::Client),
        is_resizable: true,
        is_movable: true,
        window_min_size: Some(size(px(480.), px(320.))),
        ..Default::default()
    }
}

/// gpui-kit titlebar themed to the app: title text only; minimize /
/// maximize / close come from TitleBar itself.
fn app_title_bar(
    theme: &Theme,
    title: SharedString,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) -> TitleBar {
    TitleBar::new()
        .on_close_window(move |_e, w, cx| on_close(w, cx))
        .bg(rgb_bytes(theme.background))
        .border_color(rgb_bytes(theme.arrows))
        .child(
            div()
                .flex_1()
                .text_color(rgb_bytes(theme.text))
                .child(title),
        )
}

// ---------------------------------------------------------------------------
// Launcher window
// ---------------------------------------------------------------------------

struct LauncherView {
    theme: &'static Theme,
    wpm: u32,
    path: Option<PathBuf>,
    error: Option<SharedString>,
    focus: FocusHandle,
    picker: Task<()>,
    slider: Entity<SliderState>,
    _slider_sub: Subscription,
}

impl LauncherView {
    fn new(theme: &'static Theme, wpm: u32, cx: &mut Context<Self>) -> Self {
        let slider = cx.new(|_| {
            SliderState::new()
                .min(SLIDER_MIN as f32)
                .max(SLIDER_MAX as f32)
                .step(1.0)
                .default_value(wpm.max(1).clamp(SLIDER_MIN, SLIDER_MAX) as f32)
        });
        let wpm = wpm.max(1);
        let _slider_sub = cx.subscribe(&slider, |this, _, ev: &SliderEvent, cx| {
            if let SliderEvent::Change(v) = ev {
                this.set_wpm(v.start() as u32, cx);
            }
        });
        Self {
            theme,
            wpm,
            path: None,
            error: None,
            focus: cx.focus_handle(),
            picker: Task::ready(()),
            slider,
            _slider_sub,
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
        match load_book(&path) {
            Ok(text) if frames(&text).next().is_some() => {
                let theme = self.theme;
                let wpm = self.wpm;
                let stem = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let title: SharedString = format!("Supersprinter — {stem}").into();
                let opts = win_options(title.clone(), 900., 500., cx);
                let opened = gpui_kit::open_window(opts, cx, move |window, cx| {
                    let view = cx.new(|cx| {
                        ReaderView::new(window, text, path.clone(), theme, wpm, title, cx)
                    });
                    view.focus_handle(cx).focus(window, cx);
                    view
                });
                if opened.is_err() {
                    self.error = Some("failed to open reader window".into());
                }
            }
            Ok(_) => {
                self.error = Some(format!("no words found in {}", path.display()).into());
            }
            Err(e) => self.error = Some(format!("{e:#}").into()),
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;

        let mut root = div()
            .id("launcher")
            .key_context("launcher")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(
                |this, e: &KeyDownEvent, w, cx| match e.keystroke.key.as_str() {
                    "escape" => close_window(w, cx),
                    "enter" => this.run(cx),
                    _ => {}
                },
            ))
            .on_click(cx.listener(|this, _e, window, cx| this.focus.focus(window, cx)))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb_bytes(theme.background))
            .text_color(rgb_bytes(theme.text));

        root = root.child(app_title_bar(theme, "Supersprinter".into(), close_window));

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
            .children([
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .children([
                        div()
                            .px_3()
                            .py_1()
                            .border_1()
                            .border_color(border)
                            .font_family(ui_font(cx))
                            .child(path_label)
                            .into_any_element(),
                        Button::new("browse")
                            .label("Browse…")
                            .on_click(cx.listener(|this, _e, _w, cx| this.browse(cx)))
                            .into_any_element(),
                    ])
                    .into_any_element(),
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .children([
                        div()
                            .w(px(240.))
                            .child(Slider::new(&self.slider).w_full())
                            .into_any_element(),
                        div()
                            .font_family(ui_font(cx))
                            .child(format!("{} wpm", self.wpm))
                            .into_any_element(),
                    ])
                    .into_any_element(),
                Button::new("run")
                    .primary()
                    .label("▶ Run")
                    .on_click(cx.listener(|this, _e, _w, cx| this.run(cx)))
                    .into_any_element(),
            ])
            .when_some(self.error.clone(), |this, err| {
                this.child(
                    div()
                        .font_family(ui_font(cx))
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
    text: String,
    pos: FramePos,
    total: usize,
    source: PathBuf,
    playing: bool,
    wpm: u32,
    theme: &'static Theme,
    title: SharedString,
    task: Task<()>,
    export: Task<()>,
    exporting: bool,
    export_msg: Option<SharedString>,
    root_focus: FocusHandle,
    slider: Entity<SliderState>,
    wpm_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl ReaderView {
    fn new(
        window: &mut Window,
        text: String,
        source: PathBuf,
        theme: &'static Theme,
        wpm: u32,
        title: SharedString,
        cx: &mut Context<Self>,
    ) -> Self {
        let pos = first_pos(&text);
        let total = frames(&text).count();
        let wpm = wpm.max(1);
        let slider = cx.new(|_| {
            SliderState::new()
                .min(SLIDER_MIN as f32)
                .max(SLIDER_MAX as f32)
                .step(1.0)
                .default_value(wpm.clamp(SLIDER_MIN, SLIDER_MAX) as f32)
        });
        let wpm_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("wpm")
                .default_value(wpm.to_string())
                .min(1.)
                .max(4000.)
                .step(25.)
        });
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(&slider, window, |this, _, ev, window, cx| {
            if let SliderEvent::Change(v) = ev {
                this.set_wpm(v.start() as u32, window, cx);
            }
        }));
        subscriptions.push(
            cx.subscribe_in(&wpm_input, window, |this, state, ev, window, cx| {
                if matches!(ev, InputEvent::Change) {
                    if let Ok(v) = state.read(cx).value().parse::<u32>() {
                        if (1..=4000).contains(&v) {
                            // NOT set_wpm: never echo into the box while typing.
                            this.apply_wpm(v, window, cx);
                        }
                    }
                }
            }),
        );
        let mut this = ReaderView {
            text,
            pos,
            total,
            source,
            playing: true,
            wpm,
            theme,
            title,
            task: Task::ready(()),
            export: Task::ready(()),
            exporting: false,
            export_msg: None,
            root_focus: cx.focus_handle(),
            slider,
            wpm_input,
            _subscriptions: subscriptions,
        };
        this.restart_task(cx);
        this
    }

    /// The frame at the current position.
    fn frame(&self) -> Option<Frame<'_>> {
        frame_at(&self.text, self.pos)
    }

    /// (Re)start the playback timer task; replacing the field cancels the old one.
    fn restart_task(&mut self, cx: &mut Context<Self>) {
        if !self.playing {
            self.task = Task::ready(());
            return;
        }
        let executor = cx.background_executor().clone();
        self.task = cx.spawn(async move |this, cx| {
            loop {
                let delay = this
                    .read_with(cx, |v, _cx| {
                        if !v.playing || v.pos.idx + 1 >= v.total {
                            None
                        } else {
                            Some(
                                60_000u64 / v.wpm.max(1) as u64
                                    * v.frame().map_or(1, |f| f.ticks) as u64,
                            )
                        }
                    })
                    .unwrap_or(None);
                let Some(ms) = delay else { break };
                executor.timer(Duration::from_millis(ms)).await;
                let keep_going = this
                    .update(cx, |v, cx| {
                        if !v.playing {
                            return false;
                        }
                        v.pos = next_pos(&v.text, v.pos);
                        cx.notify();
                        v.pos.idx + 1 < v.total
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
        let target =
            (self.pos.idx as i64 + delta).clamp(0, self.total.saturating_sub(1) as i64) as usize;
        self.pos = seek_pos(&self.text, self.pos, target);
        self.restart_task(cx);
        cx.notify();
    }

    /// Commit a new wpm: state + timer + slider sync. Never touches the
    /// number input (that would echo typing back into the box).
    fn apply_wpm(&mut self, wpm: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.wpm = wpm.clamp(1, 4000);
        self.restart_task(cx);
        let target = self.wpm.clamp(SLIDER_MIN, SLIDER_MAX) as f32;
        if self.slider.read(cx).value().start() != target {
            self.slider
                .update(cx, |s, cx| s.set_value(target, window, cx));
        }
        cx.notify();
    }

    /// Commit + mirror the value into the number box (slider + +/- keys).
    fn set_wpm(&mut self, wpm: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_wpm(wpm, window, cx);
        let text = self.wpm.to_string();
        if self.wpm_input.read(cx).value().to_string() != text {
            self.wpm_input
                .update(cx, |s, cx| s.set_value(text, window, cx));
        }
    }

    /// Opens a save dialog and renders the current book to mp4 at the
    /// chosen path, encoding on a background thread so the UI stays live.
    fn export_mp4(&mut self, cx: &mut Context<Self>) {
        if self.exporting {
            return;
        }
        self.export_msg = None;
        let dir = self
            .source
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let stem = self
            .source
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "book".into());
        let rx = cx.prompt_for_new_path(&dir, Some(&format!("{stem}.mp4")));
        let source = self.source.clone();
        let wpm = self.wpm;
        let theme = self.theme;
        self.export = cx.spawn(async move |this, cx| {
            let dest = match rx.await.ok().and_then(|r| r.ok()).flatten() {
                Some(p) => p,
                None => return,
            };
            let _ = this.update(cx, |v, cx| {
                v.exporting = true;
                v.export_msg = Some("exporting…".into());
                cx.notify();
            });
            let result = cx
                .background_spawn(async move {
                    let result = std::fs::read_to_string(&source)
                        .map_err(Into::into)
                        .and_then(|text| render::render_video(&text, &dest, wpm, theme));
                    (dest, result)
                })
                .await;
            let (dest, result) = result;
            let msg: SharedString = match result {
                Ok(()) => format!("saved {}", dest.display()).into(),
                Err(e) => format!("export failed: {e:#}").into(),
            };
            let _ = this.update(cx, |v, cx| {
                v.exporting = false;
                v.export_msg = Some(msg);
                cx.notify();
            });
        });
    }

    fn on_reader_key(&mut self, e: &KeyDownEvent, w: &mut Window, cx: &mut Context<Self>) {
        match e.keystroke.key.as_str() {
            "space" => self.set_playing(!self.playing, cx),
            "left" => self.step(-1, cx),
            "right" => self.step(1, cx),
            "q" | "escape" => close_window(w, cx),
            "+" | "=" => self.set_wpm((self.wpm + 25).min(1200), w, cx),
            "-" | "_" => self.set_wpm(self.wpm.saturating_sub(25).max(20), w, cx),
            _ => {}
        }
    }
}

impl Focusable for ReaderView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.root_focus.clone()
    }
}

impl Render for ReaderView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let frame = self.frame();
        let word: &str = frame.and_then(|f| f.word).unwrap_or("");
        let pivot = frame
            .map_or(0, |f| f.pivot)
            .min(word.chars().count().saturating_sub(1));

        let text_c: gpui::Hsla = rgb_bytes(theme.text).into();
        let pivot_c: gpui::Hsla = rgb_bytes(theme.pivot).into();
        let arrow_c = rgb_bytes(theme.arrows);

        // Byte offsets of the pivot char: the three color runs are pure byte
        // lengths, so no per-frame String is built.
        let (pivot_byte, pivot_len, pivot_char) = word
            .char_indices()
            .nth(pivot)
            .map(|(i, c)| (i, c.len_utf8(), Some(c)))
            .unwrap_or((word.len(), 0, None));
        let before_len = pivot_byte;
        let after_len = word.len() - pivot_byte - pivot_len;

        let fam = ui_font(cx);
        let reader = {
            // The word is a slice of the book text; shaping needs an owned
            // SharedString, so this copies one word's bytes per frame.
            let word_shape: SharedString = SharedString::new(word);
            let font_family = fam.clone();
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
                            word_shape.clone(),
                            size,
                            &[
                                run(before_len, text_c),
                                run(pivot_len, pivot_c),
                                run(after_len, text_c),
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
                    let _ = line.paint(
                        point(origin_x, origin_y),
                        line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
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

        // The guard div is REQUIRED: keys typed in the box (space, q, +/-)
        // would otherwise bubble to the reader root's key context and toggle
        // play, close the window, or step wpm. The focused NumberInput sits
        // deeper and receives keys first; the guard only stops the bubble.
        let input = div()
            .id("wpm-input-guard")
            .key_context("wpm-input")
            .on_key_down(|_e, _w, cx| cx.stop_propagation())
            .w(px(120.))
            .child(NumberInput::new(&self.wpm_input).accessibility_label("Words per minute"));

        let play_label = if self.playing { "pause" } else { "play" };
        let progress = format!("{}/{}", self.pos.idx + 1, self.total);
        let wpm_label = format!("{} wpm", self.wpm);

        // Controls at the top, directly under the titlebar: playback on the
        // left, speed controls on the right.
        let controls = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .p_3()
            .text_color(rgb_bytes(theme.text))
            .children([
                div().flex().flex_row().items_center().gap_4().children([
                    Button::new("play-toggle")
                        .icon(if self.playing {
                            IconName::Pause
                        } else {
                            IconName::Play
                        })
                        // Fixed-width label sized for "pause" (the longer
                        // word) so the button never resizes on flip.
                        .child(div().w(px(48.)).child(play_label))
                        .on_click(
                            cx.listener(|this, _, _w, cx| this.set_playing(!this.playing, cx)),
                        )
                        .into_any_element(),
                    Button::new("export-mp4")
                        .icon(if self.exporting {
                            IconName::LoaderCircle
                        } else {
                            IconName::Download
                        })
                        .label(if self.exporting {
                            "exporting…"
                        } else {
                            "export mp4"
                        })
                        .disabled(self.exporting)
                        .on_click(cx.listener(|this, _, _w, cx| this.export_mp4(cx)))
                        .into_any_element(),
                    self.export_msg
                        .as_ref()
                        .map(|m| div().text_color(rgb_bytes(theme.arrows)).child(m.clone()))
                        .unwrap_or_else(|| div())
                        .into_any_element(),
                ]),
                div().flex().flex_row().items_center().gap_4().children([
                    div()
                        .w(px(240.))
                        .child(Slider::new(&self.slider).w_full())
                        .into_any_element(),
                    input.into_any_element(),
                    div().child(wpm_label).into_any_element(),
                    div().child(progress).into_any_element(),
                ]),
            ]);

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
            .on_click(cx.listener(|this, _e, window, cx| {
                this.root_focus.focus(window, cx);
            }));

        let title = self.title.clone();
        root = root.child(app_title_bar(theme, title, |w, cx| close_window(w, cx)));

        root.children([controls.into_any_element(), reader.into_any_element()])
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
            let fam: SharedString = match core::resolve_font(&theme) {
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
                    let stem = path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    let title: SharedString = format!("Supersprinter — {stem}").into();
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
