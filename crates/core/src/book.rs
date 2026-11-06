use std::path::Path;

/// Read a book file as UTF-8 text. Whitespace normalization happens later in
/// [`crate::frame::frames`]; nothing else is stripped.
pub fn load_book(path: &Path) -> anyhow::Result<String> {
    std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read book {}: {e}", path.display()))
}
