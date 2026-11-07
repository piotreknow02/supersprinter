//! Embedded binary assets. Decoded once and cached; a decode failure (never
//! expected — the file is committed and `include_bytes!`-verified at compile
//! time) degrades to no icon instead of panicking.

use std::sync::{Arc, LazyLock};

static WINDOW_ICON: LazyLock<Option<Arc<image::RgbaImage>>> = LazyLock::new(|| {
    image::load_from_memory(include_bytes!(
        "../../../assets/supersprinter-window-icon.png"
    ))
    .map(|i| Arc::new(i.to_rgba8()))
    .ok()
});

pub(crate) fn window_icon() -> Option<Arc<image::RgbaImage>> {
    WINDOW_ICON.clone()
}

#[cfg(test)]
mod tests {
    #[test]
    fn window_icon_decodes() {
        let icon = super::window_icon();
        assert!(icon.is_some(), "embedded PNG failed to decode");
        let icon = icon.unwrap();
        assert_eq!((icon.width(), icon.height()), (256, 256));
    }
}
