//! The live numbers a host asked for, by the score text they belong to.
//!
//! A host moves a plain number of the running score the way it moves a
//! slider: it names the number's range before the text is evaluated, and
//! the evaluation turns that number into a live cell. The ranges belong to
//! one text. A text can wait for its launch line or its sounds while a newer
//! text is sent, so each text keeps its own ranges, found by its revision.

use std::ops::Range;
use std::sync::Arc;

use crate::ui_events::source_revision;

use super::Session;

/// The texts the store keeps ranges for. A text waits behind at most a few
/// newer ones, so the oldest past this are let go.
const KEPT_TEXTS: usize = 8;

#[derive(Debug, Default)]
pub(super) struct LiveNumbers {
    /// Newest last: the revision of a text and its ranges.
    texts: Vec<(String, Arc<[Range<usize>]>)>,
}

impl LiveNumbers {
    fn set(&mut self, revision: String, ranges: Arc<[Range<usize>]>) {
        self.texts.retain(|(kept, _)| *kept != revision);
        if ranges.is_empty() {
            return;
        }
        if self.texts.len() == KEPT_TEXTS {
            self.texts.remove(0);
        }
        self.texts.push((revision, ranges));
    }

    fn of(&self, revision: &str) -> Arc<[Range<usize>]> {
        self.texts
            .iter()
            .rev()
            .find(|(kept, _)| kept == revision)
            .map_or_else(|| Arc::from([]), |(_, ranges)| Arc::clone(ranges))
    }
}

impl Session {
    /// The numbers of `source` that its evaluations make live, as byte
    /// ranges of it. They replace the ranges given before for the same
    /// text. No ranges evaluates every number as written.
    pub fn set_live_numbers(&mut self, source: &str, ranges: Vec<Range<usize>>) {
        self.live_numbers
            .set(source_revision(source), Arc::from(ranges));
    }

    /// The live numbers of `source`: the ranges last given for this text.
    pub fn live_numbers(&self, source: &str) -> Arc<[Range<usize>]> {
        self.live_numbers.of(&source_revision(source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One range, as the store keeps it.
    fn one(start: usize) -> Arc<[Range<usize>]> {
        std::iter::once(start..start + 1).collect()
    }

    #[test]
    fn each_text_keeps_its_own_numbers_and_the_newest_wins() {
        let mut store = LiveNumbers::default();
        store.set("a".into(), one(1));
        store.set("b".into(), one(3));
        store.set("a".into(), one(5));
        assert_eq!(store.of("a"), one(5));
        assert_eq!(store.of("b"), one(3));
        store.set("a".into(), Arc::from([]));
        assert!(store.of("a").is_empty());
        for text in 0..=KEPT_TEXTS {
            store.set(text.to_string(), one(text));
        }
        assert!(store.of("b").is_empty(), "the oldest text is let go");
        assert_eq!(store.of(&KEPT_TEXTS.to_string()), one(KEPT_TEXTS));
    }
}
