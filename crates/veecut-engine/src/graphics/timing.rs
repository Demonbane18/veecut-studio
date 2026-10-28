use std::ops::Range;

use crate::project::{Anim, Item, ItemBody, Word};

/// Herk Edit Kit's two-frame lead and hard page swaps, with a 300 ms tail.
pub(super) fn active_caption(items: &[Item], t: f64, fps: f64) -> Option<(&Item, &[Word])> {
    let lead = 2.0 / fps.max(1.0);
    items
        .iter()
        .filter_map(|item| {
            let ItemBody::Caption(caption) = &item.body else {
                return None;
            };
            let first = caption.words.first()?;
            let start = item.start + first.start - lead;
            (t >= start).then_some((item, caption.words.as_slice(), start))
        })
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .and_then(|(item, words, _)| {
            (t < item.start + words.last()?.end + 0.3).then_some((item, words))
        })
}

pub(super) fn active_word(words: &[Word], t: f64) -> usize {
    words.iter().rposition(|word| word.start <= t).unwrap_or(0)
}

/// Keep words intact, wrap at the character limit, and paginate after two lines.
/// A single word longer than the limit stays whole and is fitted by the renderer.
pub(super) fn caption_pages(
    words: &[Word],
    max_chars: usize,
    uppercase: bool,
) -> Vec<(Range<usize>, usize)> {
    let limit = max_chars.max(1);
    let mut pages = Vec::new();
    let (mut start, mut line_start, mut length, mut second) = (0, 0, 0, None);
    for (i, word) in words.iter().enumerate() {
        let n = if uppercase {
            word.text.to_uppercase().chars().count()
        } else {
            word.text.chars().count()
        };
        if i > line_start && length + 1 + n > limit {
            if second.is_some() {
                pages.push((start..i, second.unwrap_or(i)));
                start = i;
                second = None;
            } else {
                second = Some(i);
            }
            line_start = i;
            length = n;
        } else {
            length += n + usize::from(i > line_start);
        }
    }
    if start < words.len() {
        pages.push((start..words.len(), second.unwrap_or(words.len())));
    }
    pages
}

/// Paired asterisks mark accent text. An unmatched asterisk remains literal.
pub(super) fn accents(text: &str) -> Vec<(&str, bool)> {
    let mut spans = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('*') {
        let Some(close) = rest[open + 1..].find('*').map(|i| i + open + 1) else {
            break;
        };
        if open > 0 {
            spans.push((&rest[..open], false));
        }
        if close > open + 1 {
            spans.push((&rest[open + 1..close], true));
        }
        rest = &rest[close + 1..];
    }
    if !rest.is_empty() {
        spans.push((rest, false));
    }
    spans
}

#[derive(Clone, Copy)]
pub(super) struct Motion {
    pub alpha: f32,
    pub scale: f32,
    pub x: f32,
    pub y: f32,
    pub reveal: f32,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            scale: 1.0,
            x: 0.0,
            y: 0.0,
            reveal: 1.0,
        }
    }
}

/// Seekable 300 ms motion; cubic ease-out entrances and ease-in exits.
pub(super) fn motion(anim: Anim, progress: f32, exit: bool) -> Motion {
    let p = progress.clamp(0.0, 1.0);
    let visible = if exit {
        1.0 - p.powi(3)
    } else {
        1.0 - (1.0 - p).powi(3)
    };
    let mut m = Motion::default();
    match anim {
        Anim::None => {}
        Anim::Fade => m.alpha = visible,
        Anim::Pop => {
            let q = if exit { 1.0 - p } else { p };
            let back = 1.0 + 2.2 * (q - 1.0).powi(3) + 1.2 * (q - 1.0).powi(2);
            m.scale = 0.76 + 0.24 * back;
            m.alpha = visible;
        }
        Anim::SlideUp => {
            m.y = 1.0 - visible;
            m.alpha = visible;
        }
        Anim::SlideDown => {
            m.y = visible - 1.0;
            m.alpha = visible;
        }
        Anim::SlideLeft => {
            m.x = 1.0 - visible;
            m.alpha = visible;
        }
        Anim::SlideRight => {
            m.x = visible - 1.0;
            m.alpha = visible;
        }
        Anim::Typewriter | Anim::Wipe => m.reveal = visible,
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Caption;

    fn word(text: &str, start: f64) -> Word {
        Word {
            text: text.into(),
            start,
            end: start + 0.3,
            emphasis: false,
        }
    }

    #[test]
    fn paired_accent_markup_preserves_unicode_and_unmatched_marks() {
        assert_eq!(
            accents("Try *café* now *"),
            vec![("Try ", false), ("café", true), (" now *", false)]
        );
        assert_eq!(
            accents("*one* *two*"),
            vec![("one", true), (" ", false), ("two", true)]
        );
        assert!(accents("").is_empty());
    }

    #[test]
    fn pages_lead_by_two_frames_and_swap_without_overlap() {
        let mut items = vec![
            Item {
                id: "a".into(),
                start: 1.0,
                duration: 0.4,
                body: ItemBody::Caption(Caption {
                    words: vec![word("one", 0.0), word("two", 0.4)],
                }),
            },
            Item {
                id: "b".into(),
                start: 1.8,
                duration: 0.3,
                body: ItemBody::Caption(Caption {
                    words: vec![word("next", 0.0)],
                }),
            },
        ];
        assert!(active_caption(&items, 1.0 - 2.0 / 30.0 - 0.0001, 30.0).is_none());
        assert_eq!(
            active_caption(&items, 1.0 - 2.0 / 30.0, 30.0).unwrap().0.id,
            "a"
        );
        assert_eq!(
            active_caption(&items, 1.8 - 2.0 / 30.0, 30.0).unwrap().0.id,
            "b"
        );
        assert!(active_caption(&items, 2.401, 30.0).is_none());
        let words = vec![word("one", 0.0), word("two", 0.4)];
        assert_eq!(active_word(&words, -0.05), 0);
        assert_eq!(active_word(&words, 0.4), 1);
        if let ItemBody::Caption(c) = &mut items[0].body {
            c.words.last_mut().unwrap().end = 10.0;
        }
        assert!(
            active_caption(&items, 2.401, 30.0).is_none(),
            "an old page must never reappear"
        );
    }

    #[test]
    fn wraps_at_max_chars_and_paginates_at_two_lines() {
        let words: Vec<_> = ["one", "two", "three", "four", "five", "six"]
            .into_iter()
            .map(|s| word(s, 0.0))
            .collect();
        assert_eq!(
            caption_pages(&words, 7, false),
            vec![(0..3, 2), (3..5, 4), (5..6, 6)]
        );
        assert_eq!(
            caption_pages(&[word("ééé", 0.0), word("yes", 0.0)], 7, false),
            vec![(0..2, 2)]
        );
    }

    #[test]
    fn motion_is_seekable_and_ends_at_rest() {
        for a in [
            Anim::Fade,
            Anim::Pop,
            Anim::SlideUp,
            Anim::SlideDown,
            Anim::SlideLeft,
            Anim::SlideRight,
            Anim::Typewriter,
            Anim::Wipe,
        ] {
            let end = motion(a, 1.0, false);
            assert_eq!(
                (end.alpha, end.scale, end.x, end.y, end.reveal),
                (1.0, 1.0, 0.0, 0.0, 1.0)
            );
        }
        assert!(motion(Anim::Pop, 0.7, false).scale > 1.0);
    }
}
