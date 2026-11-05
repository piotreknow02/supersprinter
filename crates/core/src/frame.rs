use std::sync::Arc;

/// One presentation frame: a word (or a blank pause), the 0-based index of the
/// pivot (ORP) character, and a duration in ticks (1 tick = 60_000 / wpm ms).
/// `word` is an `Arc<str>` so cloning a frame is a refcount bump, not a copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub word: Option<Arc<str>>,
    pub pivot: usize,
    pub ticks: u32,
}

/// Pivot index (0-based) for a word of `len` characters: the letter at ~30% of
/// the word (ceil), clamped inside the word. This is the common RSVP ORP rule
/// (Swift/Reedy): `quick` -> 'i', `brown` -> 'o'. Chosen over Glance's table
/// (which marks the 2nd letter for all 2-5 char words, i.e. most words) after
/// user feedback that the marker was "always the second letter".
pub fn pivot_index(len: usize) -> usize {
    ((len as f32 * 0.3).ceil() as usize).min(len.saturating_sub(1))
}

/// Dwell time in ticks, ported from Glance's `glanceify()` pacing rules
/// (glance.js:42-95): sentence end gets 4 ticks (Glance inserts 3 blank
/// frames), commas/colons/dashes/long words get 2 (Glance duplicates).
fn pacing(word: &str) -> u32 {
    if word.contains(['.', '!', '?']) {
        4
    } else if word.contains([',', ';', ':', '-', '(']) || word.chars().count() > 8 {
        2
    } else {
        1
    }
}

/// Build the full presentation stream from raw text.
pub fn build_frames(text: &str) -> Vec<Frame> {
    text.split_whitespace()
        .map(|word| Frame {
            pivot: pivot_index(word.chars().count()),
            ticks: pacing(word),
            word: Some(Arc::from(word)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pivot_table_boundaries() {
        assert_eq!(pivot_index(1), 0);
        assert_eq!(pivot_index(2), 1);
        assert_eq!(pivot_index(3), 1);
        assert_eq!(pivot_index(4), 2);
        assert_eq!(pivot_index(5), 2);
        assert_eq!(pivot_index(10), 3);
        assert_eq!(pivot_index(11), 4);
        assert_eq!(pivot_index(20), 6);
    }

    #[test]
    fn pivot_never_exceeds_word() {
        assert_eq!(pivot_index(0), 0);
        assert_eq!(pivot_index(1), 0);
        assert_eq!(pivot_index(2), 1);
    }

    #[test]
    fn pacing_precedence() {
        // Sentence end wins over comma.
        assert_eq!(pacing("hi,."), 4);
        assert_eq!(pacing("dog."), 4);
        assert_eq!(pacing("wow!"), 4);
        assert_eq!(pacing("huh?"), 4);
        // Comma / colon / dash / paren / long word double up.
        assert_eq!(pacing("hello,"), 2);
        assert_eq!(pacing("well;"), 2);
        assert_eq!(pacing("note:"), 2);
        assert_eq!(pacing("well-known"), 2);
        assert_eq!(pacing("(wait"), 2);
        assert_eq!(pacing("extraordinary"), 2); // 13 chars > 8
        assert_eq!(pacing("exactly8"), 1); // exactly 8 chars
        assert_eq!(pacing("word"), 1);
    }

    #[test]
    fn build_frames_tokens_and_pivots() {
        let frames = build_frames("The quick brown fox jumps over the lazy dog.");
        assert_eq!(frames.len(), 9);
        // "quick" -> pivot 'i' (index 2)
        let quick = &frames[1];
        assert_eq!(quick.word.as_deref(), Some("quick"));
        assert_eq!(quick.pivot, 2);
        assert_eq!(&quick.word.as_ref().unwrap()[quick.pivot..quick.pivot + 1], "i");
        // "brown" -> pivot 'o' (index 2)
        let brown = &frames[2];
        assert_eq!(brown.pivot, 2);
        assert_eq!(&brown.word.as_ref().unwrap()[brown.pivot..brown.pivot + 1], "o");
        // "dog." gets sentence-end pacing
        let dog = &frames[8];
        assert_eq!(dog.word.as_deref(), Some("dog."));
        assert_eq!(dog.ticks, 4);
        // "The" is a normal word
        assert_eq!(frames[0].ticks, 1);
    }

    #[test]
    fn build_frames_normalizes_whitespace() {
        let frames = build_frames("  a\n\n  b\t\tc  ");
        let words: Vec<_> = frames.iter().map(|f| f.word.clone()).collect();
        assert_eq!(words, vec![Some("a".into()), Some("b".into()), Some("c".into())]);
    }
}
