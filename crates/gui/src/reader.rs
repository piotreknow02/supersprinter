//! Reader window. The reader area is a single `canvas`: the word is shaped
//! with the pivot glyph centered on the viewport midline, and the ▼/▲ markers
//! are painted as filled triangles at exactly that glyph's x — so markers and
//! letters can never drift apart. Font size scales with the viewport.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, PathBuilder, Pixels, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, TextAlign, TextRun, Window, canvas,
    div, font, point, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    Disableable,
    button::Button,
    collapsible::Collapsible,
    input::{Input, InputEvent, InputState, NumberInput},
    slider::{Slider, SliderEvent, SliderState},
    status_bar::StatusBar,
};

use core::{Frame, FramePos, Theme, first_pos, frame_at, frames, next_pos, seek_pos};

use crate::utils::{SLIDER_MAX, SLIDER_MIN, app_title_bar, close_window, rgb_bytes, ui_font};

pub(crate) struct ReaderView {
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
    ffmpeg_input: Entity<InputState>,
    settings_open: bool,
    _subscriptions: Vec<Subscription>,
}

impl ReaderView {
    pub(crate) fn new(
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
        let ffmpeg_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("-preset slow -crf 18"));
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(&slider, window, |this, _, ev, window, cx| {
            if let SliderEvent::Change(v) = ev {
                this.set_wpm(v.start() as u32, window, cx);
            }
        }));
        subscriptions.push(
            cx.subscribe_in(&wpm_input, window, |this, state, ev, window, cx| {
                if matches!(ev, InputEvent::Change)
                    && let Ok(v) = state.read(cx).value().parse::<u32>()
                    && (1..=4000).contains(&v)
                {
                    // NOT set_wpm: never echo into the box while typing.
                    this.apply_wpm(v, window, cx);
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
            ffmpeg_input,
            settings_open: false,
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
        if self.wpm_input.read(cx).value() != text {
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
        let ffmpeg_args = self.ffmpeg_input.read(cx).value().to_string();
        if let Err(e) = render::parse_ffmpeg_args(&ffmpeg_args) {
            self.export_msg = Some(format!("ffmpeg args: {e:#}").into());
            cx.notify();
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
                        .and_then(|text| {
                            render::render_video(&text, &dest, wpm, theme, &ffmpeg_args)
                        });
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
        // Keys typed inside an input (wpm / ffmpeg args) must not drive the
        // reader: act only when the reader root itself holds (or lacks) focus.
        // Stopping the bubble in the input guards instead would swallow the
        // keystrokes before gpui's text-input path reaches the editor.
        if w.focused(cx).is_some_and(|f| f != self.root_focus) {
            return;
        }
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

        // The guard div is REQUIRED: the root's on_click refocuses the
        // reader, which would steal focus from the box on every click; the
        // click stop below prevents that bubble. Key isolation lives in
        // on_reader_key's focus gate instead — stopping keys here would
        // block typing in the box.
        let input = div()
            .id("wpm-input-guard")
            .debug_selector(|| "wpm-input-guard".into())
            .key_context("wpm-input")
            .on_click(|_e, _w, cx| cx.stop_propagation())
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

        // Collapsible ffmpeg-args row, right-aligned under the controls bar.
        // The guard div is REQUIRED (same reason as wpm-input-guard): typing
        // '-', space or 'q' in the box must not bubble to the reader root's
        // key context (step / pause / close).
        let settings = div()
            .flex_none()
            .flex()
            .flex_row()
            .justify_end()
            .px_3()
            .child(
                Collapsible::new()
                    .open(self.settings_open)
                    .child(
                        div()
                            .id("ffmpeg-toggle")
                            .debug_selector(|| "ffmpeg-toggle".into())
                            .child(
                                Button::new("ffmpeg-settings")
                                    .icon(if self.settings_open {
                                        IconName::ChevronUp
                                    } else {
                                        IconName::ChevronDown
                                    })
                                    .label("ffmpeg args")
                                    .on_click(cx.listener(|this, _, _w, cx| {
                                        this.settings_open = !this.settings_open;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .content(
                        div().w(px(420.)).pt_2().child(
                            div()
                                .id("ffmpeg-input-guard")
                                .debug_selector(|| "ffmpeg-input-guard".into())
                                .key_context("ffmpeg-input")
                                .on_click(|_e, _w, cx| cx.stop_propagation())
                                .child(Input::new(&self.ffmpeg_input)),
                        ),
                    ),
            );

        // Status line pinned to the bottom: export progress and results
        // render here instead of crowding the controls row. The bar is
        // always present so the layout never jumps when a message appears.
        let status = StatusBar::new()
            .flex_none()
            .bg(rgb_bytes(theme.background))
            .border_color(rgb_bytes(theme.arrows))
            .text_color(rgb_bytes(theme.arrows))
            .left(self.export_msg.clone().unwrap_or_else(|| "Ready".into()));

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
        root = root.child(app_title_bar(theme, title, close_window));

        root.children([
            controls.into_any_element(),
            settings.into_any_element(),
            reader.into_any_element(),
            status.into_any_element(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use std::sync::LazyLock;

    static TEST_THEME: LazyLock<Theme> = LazyLock::new(|| Theme {
        background: [10, 10, 10],
        text: [240, 240, 240],
        arrows: [120, 120, 120],
        pivot: [220, 50, 50],
        font: "monospace".into(),
    });

    fn open_reader(cx: &mut TestAppContext) -> (Entity<ReaderView>, &mut VisualTestContext) {
        cx.update(gpui_kit::init);
        cx.add_window_view(|window, cx| {
            ReaderView::new(
                window,
                "the quick brown fox".into(),
                PathBuf::from("/tmp/b.txt"),
                &TEST_THEME,
                300,
                "test".into(),
                cx,
            )
        })
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("no bounds for {selector:?}"));
        cx.simulate_click(bounds.center(), Modifiers::default());
    }

    fn assert_focused(view: &Entity<ReaderView>, cx: &mut VisualTestContext, which: &str) {
        let (focused, want) = cx.update(|window, app| {
            let want = match which {
                "ffmpeg" => view.read(app).ffmpeg_input.read(app).focus_handle(app),
                "wpm" => view.read(app).wpm_input.read(app).focus_handle(app),
                _ => unreachable!(),
            };
            (window.focused(app), want)
        });
        assert!(
            focused.as_ref().is_some_and(|f| f == &want),
            "{which} input lost focus after click: {focused:?}"
        );
    }

    #[gpui::test]
    fn ffmpeg_input_keeps_focus(cx: &mut TestAppContext) {
        let (view, cx) = open_reader(cx);
        cx.update(|_, _| {});
        click(cx, "ffmpeg-toggle");
        click(cx, "ffmpeg-input-guard");
        assert_focused(&view, cx, "ffmpeg");
    }

    #[gpui::test]
    fn wpm_input_keeps_focus(cx: &mut TestAppContext) {
        let (view, cx) = open_reader(cx);
        cx.update(|_, _| {});
        click(cx, "wpm-input-guard");
        assert_focused(&view, cx, "wpm");
    }
}
