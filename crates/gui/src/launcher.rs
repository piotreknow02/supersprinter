//! Launcher window: file picker + WPM slider + Run.

use std::path::PathBuf;

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, PathPromptOptions, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, Window, div, prelude::FluentBuilder,
    px,
};
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    slider::{Slider, SliderEvent, SliderState},
};

use core::{Theme, frames, load_book};

use crate::reader::ReaderView;
use crate::utils::{
    SLIDER_MAX, SLIDER_MIN, app_title_bar, close_window, reader_title, rgb_bytes, ui_font,
    win_options,
};

pub(crate) struct LauncherView {
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
    pub(crate) fn new(theme: &'static Theme, wpm: u32, cx: &mut Context<Self>) -> Self {
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
                let title = reader_title(&path);
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
