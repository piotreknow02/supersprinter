use std::path::PathBuf;

use serde::Deserialize;

pub type Rgb = [u8; 3];

/// Presentation theme. Loaded from TOML; see search order in [`load_theme`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub background: Rgb,
    pub text: Rgb,
    pub arrows: Rgb,
    pub pivot: Rgb,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            background: [0x10, 0x10, 0x14],
            text: [0xe6, 0xe6, 0xe6],
            arrows: [0xff, 0x5f, 0x56],
            pivot: [0xff, 0x5f, 0x56],
        }
    }
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

fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("theme.toml")];
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".config").join("supersprinter").join("theme.toml"));
        paths.push(home.join(".supersprinter").join("theme.toml"));
    }
    paths
}

/// Load the theme. Search order, first existing file wins:
/// `./theme.toml` → `$HOME/.config/supersprinter/theme.toml` →
/// `$HOME/.supersprinter/theme.toml`. No file → built-in default.
/// A found-but-invalid file is a hard error.
pub fn load_theme() -> anyhow::Result<Theme> {
    for path in candidate_paths() {
        if !path.is_file() {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("failed to read theme {}: {e}", path.display()))?;
        let file: ThemeFile = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("invalid theme {}: {e}", path.display()))?;
        let parse = |s: &str| {
            parse_hex(s).map_err(|e| anyhow::anyhow!("invalid theme {}: {e}", path.display()))
        };
        let pivot = match &file.pivot {
            Some(p) => parse(p)?,
            None => parse_hex(&file.text)?,
        };
        return Ok(Theme {
            background: parse(&file.background)?,
            text: parse(&file.text)?,
            arrows: parse(&file.arrows)?,
            pivot,
        });
    }
    Ok(Theme::default())
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
pivot = "#ff5f56"
"##;

    #[test]
    fn toml_roundtrip_and_missing_key() {
        let f: ThemeFile = toml::from_str(GOOD).unwrap();
        assert_eq!(parse_hex(&f.background).unwrap(), [0x10, 0x10, 0x14]);
        assert!(f.pivot.is_some());

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
}
