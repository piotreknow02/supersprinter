/// One presentation frame: a word (or a blank pause), the 0-based index of the
/// pivot (ORP) character, and a duration in ticks (1 tick = 60_000 / wpm ms).
/// `word` borrows the source text, so a frame is `Copy` and building one
/// allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    pub word: Option<&'a str>,
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

/// Build one frame from a word token.
fn frame(word: &str) -> Frame<'_> {
    Frame {
        pivot: pivot_index(word.chars().count()),
        ticks: pacing(word),
        word: Some(word),
    }
}

/// The presentation stream of `text` as a lazy view: words are yielded one at
/// a time, borrowed from `text`. Nothing is collected or copied.
pub fn frames(text: &str) -> impl Iterator<Item = Frame<'_>> + use<'_> {
    text.split_whitespace().map(frame)
}

/// Position in a text's frame stream: the byte offset of the current word and
/// its frame index. `Copy` so a reader can store it next to the owned `String`
/// it points into and re-derive the current frame with [`frame_at`] on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePos {
    pub off: usize,
    pub idx: usize,
}

/// The first word at or after `from`, as its start offset and slice.
fn word_from(text: &str, from: usize) -> Option<(usize, &str)> {
    let rest = text[from..].trim_start();
    let word = rest.split_whitespace().next()?;
    Some((text.len() - rest.len(), word))
}

/// Position of the first frame. For empty text the stream is empty and
/// [`frame_at`] returns `None` at this position.
pub fn first_pos(text: &str) -> FramePos {
    word_from(text, 0).map_or(FramePos { off: 0, idx: 0 }, |(off, _)| FramePos { off, idx: 0 })
}

/// The frame at `pos`, or `None` if no word remains from that offset.
pub fn frame_at(text: &str, pos: FramePos) -> Option<Frame<'_>> {
    word_from(text, pos.off).map(|(_, word)| frame(word))
}

/// Position of the frame after `pos`; unchanged at the end of the stream.
pub fn next_pos(text: &str, pos: FramePos) -> FramePos {
    let Some((off, word)) = word_from(text, pos.off) else {
        return pos;
    };
    match word_from(text, off + word.len()) {
        Some((next_off, _)) => FramePos { off: next_off, idx: pos.idx + 1 },
        None => pos,
    }
}

/// Position of frame `target`, clamped to the stream. Forward moves scan from
/// `pos`; a backward move rescans from the start of the text, which is fine
/// for interactive stepping.
pub fn seek_pos(text: &str, pos: FramePos, target: usize) -> FramePos {
    let mut p = if target < pos.idx { first_pos(text) } else { pos };
    while p.idx < target {
        let n = next_pos(text, p);
        if n.idx == p.idx {
            break; // end of stream
        }
        p = n;
    }
    p
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
    fn frames_tokens_and_pivots() {
        let frames: Vec<_> = frames("The quick brown fox jumps over the lazy dog.").collect();
        assert_eq!(frames.len(), 9);
        // "quick" -> pivot 'i' (index 2)
        let quick = frames[1];
        assert_eq!(quick.word, Some("quick"));
        assert_eq!(quick.pivot, 2);
        assert_eq!(&quick.word.unwrap()[quick.pivot..quick.pivot + 1], "i");
        // "brown" -> pivot 'o' (index 2)
        let brown = frames[2];
        assert_eq!(brown.pivot, 2);
        assert_eq!(&brown.word.unwrap()[brown.pivot..brown.pivot + 1], "o");
        // "dog." gets sentence-end pacing
        let dog = frames[8];
        assert_eq!(dog.word, Some("dog."));
        assert_eq!(dog.ticks, 4);
        // "The" is a normal word
        assert_eq!(frames[0].ticks, 1);
    }

    #[test]
    fn frames_normalizes_whitespace() {
        let words: Vec<_> = frames("  a\n\n  b\t\tc  ").map(|f| f.word).collect();
        assert_eq!(words, vec![Some("a"), Some("b"), Some("c")]);
    }

    #[test]
    fn cursor_walk_matches_iterator() {
        let text = "  The  quick\n\n brown fox.  ";
        let expected: Vec<&str> = text.split_whitespace().collect();
        let mut pos = first_pos(text);
        let mut seen = vec![];
        loop {
            let Some(f) = frame_at(text, pos) else { break };
            assert_eq!(seen.len(), pos.idx);
            seen.push(f.word.unwrap());
            let next = next_pos(text, pos);
            if next.idx == pos.idx {
                break;
            }
            pos = next;
        }
        assert_eq!(seen, expected);
        assert_eq!(pos.idx, expected.len() - 1);
        assert_eq!(next_pos(text, pos), pos); // stays on the last frame
    }

    #[test]
    fn cursor_seek_forward_backward_and_clamps() {
        let text = "one two three four five";
        let start = first_pos(text);
        assert_eq!(frame_at(text, start).unwrap().word, Some("one"));

        let third = seek_pos(text, start, 2);
        assert_eq!(third.idx, 2);
        assert_eq!(frame_at(text, third).unwrap().word, Some("three"));

        // Backward rescans from the start and lands on the right frame.
        let back = seek_pos(text, third, 0);
        assert_eq!(back, start);
        assert_eq!(frame_at(text, seek_pos(text, third, 1)).unwrap().word, Some("two"));

        // Clamped past the end: lands on the last frame.
        let last = seek_pos(text, start, 99);
        assert_eq!(last.idx, 4);
        assert_eq!(frame_at(text, last).unwrap().word, Some("five"));
    }

    #[test]
    fn cursor_on_empty_text() {
        let pos = first_pos("");
        assert_eq!(pos, FramePos { off: 0, idx: 0 });
        assert!(frame_at("", pos).is_none());
        assert_eq!(next_pos("", pos), pos);
        assert_eq!(seek_pos("", pos, 3), pos);
    }
}
