//! Shared helpers for the launcher and reader windows.

use gpui::{
    App, Bounds, Global, ParentElement, SharedString, Styled, Window, WindowBounds,
    WindowDecorations, WindowOptions, div, px, size,
};
use gpui_kit::component::TitleBar;

use core::Theme;

pub(crate) const SLIDER_MIN: u32 = 10;
pub(crate) const SLIDER_MAX: u32 = 600;
pub(crate) const FONT: &str = core::DEFAULT_FONT;

pub(crate) fn rgb_bytes(c: core::Rgb) -> gpui::Rgba {
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
pub(crate) struct UiFont(pub(crate) SharedString);
impl Global for UiFont {}

pub(crate) fn ui_font(cx: &App) -> SharedString {
    cx.try_global::<UiFont>()
        .map(|m| m.0.clone())
        .unwrap_or_else(|| SharedString::new_static(FONT))
}

/// Close `window`, quitting the app if it is the last window.
pub(crate) fn close_window(window: &mut Window, cx: &mut App) {
    if cx.windows().len() <= 1 {
        cx.quit();
    } else {
        window.remove_window();
    }
}

pub(crate) fn win_options(title: SharedString, w: f32, h: f32, cx: &mut App) -> WindowOptions {
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
        // X11 taskbar/titlebar icon; app_id gives Wayland (and X11) the
        // WMClass that the desktop entry's StartupWMClass matches.
        icon: crate::assets::window_icon(),
        app_id: Some("supersprinter".to_string()),
        ..Default::default()
    }
}

/// gpui-kit titlebar themed to the app: title text only; minimize /
/// maximize / close come from TitleBar itself.
pub(crate) fn app_title_bar(
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

/// Window title for a reader opened on `path`: "Supersprinter — <file name>".
pub(crate) fn reader_title(path: &std::path::Path) -> SharedString {
    let stem = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    format!("Supersprinter — {stem}").into()
}
