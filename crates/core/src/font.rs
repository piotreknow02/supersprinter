use anyhow::{Context as _, Result};

use crate::{DEFAULT_FONT, Theme};

/// Family names tried (in order) when neither the theme font nor the
/// fontconfig `monospace` alias resolves to an installed face.
const MONO_FALLBACKS: &[&str] = &[
    "JetBrains Mono",
    "Fira Mono",
    "Hack",
    "Cascadia Mono",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "Ubuntu Mono",
];

/// A theme font resolved to a concrete installed face.
pub struct ResolvedFont {
    /// Concrete family name of the matched face (gpui matches names exactly).
    pub family: String,
    /// Where the face data lives.
    pub source: fontdb::Source,
    /// Face index within `source` (for TrueType collections).
    pub index: u32,
    /// True when the requested theme font was not installed and a monospace
    /// fallback was used instead.
    pub fell_back: bool,
}

/// Resolve the theme font against the system fonts via fontdb (on Linux this
/// consults fontconfig and also covers `~/.fonts`). The generic `monospace`
/// alias maps to fontdb's `Family::Monospace`; if that or the named theme
/// font is not installed, `MONO_FALLBACKS` are tried in order.
pub fn resolve_font(theme: &Theme) -> Result<ResolvedFont> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    if theme.font != DEFAULT_FONT {
        if let Some(face) = query(&db, &[fontdb::Family::Name(&theme.font)]) {
            return Ok(face);
        }
    }
    let mut families = vec![fontdb::Family::Monospace];
    families.extend(MONO_FALLBACKS.iter().map(|n| fontdb::Family::Name(n)));
    query(&db, &families)
        .map(|mut f| {
            f.fell_back = true;
            f
        })
        .with_context(|| {
            format!("no usable monospace font found for theme font {:?}", theme.font)
        })
}

fn query(db: &fontdb::Database, families: &[fontdb::Family<'_>]) -> Option<ResolvedFont> {
    let id = db.query(&fontdb::Query {
        families,
        ..Default::default()
    })?;
    let face = db.face(id)?;
    let family = face
        .families
        .first()
        .map(|(n, _)| n.clone())
        .unwrap_or_default();
    Some(ResolvedFont {
        family,
        source: face.source.clone(),
        index: face.index,
        fell_back: false,
    })
}
