use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Deserialize;

/// TOML source of the built-in default theme, embedded at compile time from
/// `themes/default.toml` at the workspace root.
const DEFAULT_TOML: &str = include_str!("../../../themes/default.toml");

/// Value of `font` used when the theme omits it. Treated as a generic alias:
/// [`resolve_font`](crate::resolve_font) maps it to the system monospace font.
pub const DEFAULT_FONT: &str = "monospace";

pub type Rgb = [u8; 3];

/// Presentation theme. Loaded from TOML; see search order in [`load_theme`].
#[derive(Debug, PartialEq, Eq)]
pub struct Theme {
    pub background: Rgb,
    pub text: Rgb,
    pub arrows: Rgb,
    pub pivot: Rgb,
    /// Font family name used for the word display (and all GUI text).
    pub font: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile<'a> {
    #[serde(borrow)]
    background: &'a str,
    #[serde(borrow)]
    text: &'a str,
    #[serde(borrow)]
    arrows: &'a str,
    #[serde(default)]
    pivot: Option<&'a str>,
    #[serde(default)]
    font: Option<&'a str>,
}

/// Parse `#rrggbb` (case-insensitive) into RGB bytes.
pub fn parse_hex(s: &str) -> anyhow::Result<Rgb> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    anyhow::ensure!(
        hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()),
        "invalid hex color {s:?}: expected #rrggbb"
    );
    let byte = |i| Ok::<_, anyhow::Error>(u8::from_str_radix(&hex[i..i + 2], 16)?);
    Ok([byte(0)?, byte(2)?, byte(4)?])
}

/// Theme file candidates in priority order. The arity is fixed at compile
/// time: `./theme.toml` plus two `$HOME` locations. The `$HOME` candidates are
/// `None` when the home directory cannot be resolved.
fn candidate_paths() -> [Option<PathBuf>; 3] {
    let home = dirs::home_dir();
    [
        Some(PathBuf::from("theme.toml")),
        home.as_ref().map(|home| {
            home.join(".config")
                .join("supersprinter")
                .join("theme.toml")
        }),
        home.as_ref()
            .map(|home| home.join(".supersprinter").join("theme.toml")),
    ]
}

/// Parse theme TOML. `source` names the origin in error messages.
fn parse_theme(raw: &str, source: &str) -> anyhow::Result<Theme> {
    let file: ThemeFile =
        toml::from_str(raw).map_err(|e| anyhow::anyhow!("invalid theme {source}: {e}"))?;
    let parse = |s: &str| parse_hex(s).map_err(|e| anyhow::anyhow!("invalid theme {source}: {e}"));
    let pivot = match &file.pivot {
        Some(p) => parse(p)?,
        None => parse_hex(file.text).map_err(|e| anyhow::anyhow!("invalid theme {source}: {e}"))?,
    };
    let font = match file.font {
        Some(f) => {
            anyhow::ensure!(
                !f.trim().is_empty(),
                "invalid theme {source}: font must not be empty"
            );
            f.to_owned()
        }
        None => DEFAULT_FONT.to_owned(),
    };
    Ok(Theme {
        background: parse(file.background)?,
        text: parse(file.text)?,
        arrows: parse(file.arrows)?,
        pivot,
        font,
    })
}

/// Process-wide theme instance, stored by [`load_theme`].
static THEME: OnceLock<Theme> = OnceLock::new();

/// Load the theme. Search order, first existing file wins:
/// `./theme.toml` → `$HOME/.config/supersprinter/theme.toml` →
/// `$HOME/.supersprinter/theme.toml`. No file → the default theme embedded
/// at build time from `themes/default.toml`.
/// A found-but-invalid file is a hard error.
///
/// The theme is immutable once loaded, so it is stored for the lifetime of
/// the process and every caller shares one `&'static` reference. Repeated
/// calls return the same reference without re-reading files.
pub fn load_theme() -> anyhow::Result<&'static Theme> {
    if let Some(theme) = THEME.get() {
        return Ok(theme);
    }
    let theme = discover_theme()?;
    let _ = THEME.set(theme); // concurrent first call: keep the winner
    Ok(THEME.get().unwrap())
}

fn discover_theme() -> anyhow::Result<Theme> {
    for path in candidate_paths().into_iter().flatten() {
        if !path.is_file() {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("failed to read theme {}: {e}", path.display()))?;
        return parse_theme(&raw, &path.display().to_string());
    }
    parse_theme(DEFAULT_TOML, "built-in default theme")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parse() {
        assert_eq!(parse_hex("#1A2b3C").unwrap(), [0x1a, 0x2b, 0x3c]);
        assert_eq!(parse_hex("00ff00").unwrap(), [0x00, 0xff, 0x00]);
        assert!(parse_hex("#12345").is_err());
        assert!(parse_hex("#gggggg").is_err());
        assert!(parse_hex("").is_err());
    }

    const GOOD: &str = r##"
background = "#101014"
text = "#e6e6e6"
arrows = "#ff5f56"
font = "JetBrains Mono"
"##;

    #[test]
    fn toml_roundtrip_and_missing_key() {
        let f: ThemeFile = toml::from_str(GOOD).unwrap();
        assert_eq!(parse_hex(f.background).unwrap(), [0x10, 0x10, 0x14]);
        assert_eq!(f.font, Some("JetBrains Mono"));

        let missing = r##"
background = "#101014"
text = "#e6e6e6"
"##;
        assert!(toml::from_str::<ThemeFile>(missing).is_err());

        let unknown = r##"
background = "#101014"
text = "#e6e6e6"
arrows = "#ff5f56"
bogus = 1
"##;
        assert!(toml::from_str::<ThemeFile>(unknown).is_err());
    }

    #[test]
    fn pivot_defaults_to_text() {
        let no_pivot = r##"
background = "#000000"
text = "#abcdef"
arrows = "#ffffff"
"##;
        let f: ThemeFile = toml::from_str(no_pivot).unwrap();
        assert!(f.pivot.is_none());
    }

    #[test]
    fn embedded_default_parses() {
        let t = parse_theme(DEFAULT_TOML, "test").unwrap();
        assert_eq!(t.background, [0x10, 0x10, 0x14]);
        assert_eq!(t.text, [0xe6, 0xe6, 0xe6]);
        assert_eq!(t.arrows, [0xff, 0x5f, 0x56]);
        assert_eq!(t.pivot, [0xff, 0x5f, 0x56]);
        assert_eq!(t.font, DEFAULT_FONT);
    }

    #[test]
    fn font_default_and_custom() {
        let no_font = r##"
background = "#000000"
text = "#ffffff"
arrows = "#ffffff"
"##;
        assert_eq!(parse_theme(no_font, "test").unwrap().font, DEFAULT_FONT);

        let custom = r##"
background = "#000000"
text = "#ffffff"
arrows = "#ffffff"
font = "Fira Mono"
"##;
        assert_eq!(parse_theme(custom, "test").unwrap().font, "Fira Mono");

        let empty = r##"
background = "#000000"
text = "#ffffff"
arrows = "#ffffff"
font = "  "
"##;
        assert!(parse_theme(empty, "test").is_err());
    }
}
