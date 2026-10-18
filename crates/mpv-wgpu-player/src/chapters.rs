//! Chapter marks, read from mpv's `chapter-list`.

use crate::value::Value;

use crate::nodes::{seconds, text};
use crate::types::Finite;

/// One chapter mark.
#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
    /// The chapter title. Empty when the file does not name it.
    pub title: String,
    /// Seconds from the start of the file.
    pub start: Finite,
}

/// A chapter's zero-based position in [`crate::Player::chapters`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChapterIndex(u32);

impl ChapterIndex {
    /// The chapter at `index`, counting from 0.
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// The zero-based position.
    pub const fn get(self) -> u32 {
        self.0
    }

    pub(crate) fn from_mpv(value: i64) -> Option<Self> {
        u32::try_from(value).ok().map(Self)
    }
}

/// Parse mpv's `chapter-list` node. Anything that is not an array yields no
/// chapters, and entries without a finite `time` are dropped.
pub(crate) fn parse_chapter_list(node: &Value) -> Vec<Chapter> {
    let Value::Array(items) = node else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let Value::Map(entries) = item else {
                return None;
            };
            Some(Chapter {
                title: text(entries, "title").unwrap_or_default(),
                start: seconds(entries, "time")?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: Option<&str>, time: Value) -> Value {
        let mut entries = vec![("time".to_string(), time)];
        if let Some(title) = title {
            entries.push(("title".to_string(), Value::String(title.to_string())));
        }
        Value::Map(entries)
    }

    fn finite(value: f64) -> Finite {
        Finite::new(value).expect("finite")
    }

    #[test]
    fn parse_chapter_list_reads_titles_and_starts() {
        let node = Value::Array(vec![
            entry(Some("Intro"), Value::Double(0.0)),
            entry(Some("Main"), Value::Double(12.5)),
            entry(None, Value::Int64(30)),
        ]);
        assert_eq!(
            parse_chapter_list(&node),
            vec![
                Chapter {
                    title: "Intro".into(),
                    start: finite(0.0)
                },
                Chapter {
                    title: "Main".into(),
                    start: finite(12.5)
                },
                Chapter {
                    title: String::new(),
                    start: finite(30.0)
                },
            ]
        );
    }

    #[test]
    fn parse_chapter_list_drops_unusable_entries() {
        let cases: Vec<(&str, Value, usize)> = vec![
            ("not an array", Value::String("x".into()), 0),
            ("empty", Value::Array(vec![]), 0),
            (
                "nan time",
                Value::Array(vec![entry(Some("a"), Value::Double(f64::NAN))]),
                0,
            ),
            (
                "missing time",
                Value::Array(vec![Value::Map(vec![(
                    "title".to_string(),
                    Value::String("a".into()),
                )])]),
                0,
            ),
            ("entry is not a map", Value::Array(vec![Value::Int64(1)]), 0),
            (
                "good entry kept",
                Value::Array(vec![
                    entry(Some("a"), Value::String("later".into())),
                    entry(Some("b"), Value::Double(1.0)),
                ]),
                1,
            ),
        ];
        for (name, node, kept) in &cases {
            assert_eq!(parse_chapter_list(node).len(), *kept, "{name}");
        }
    }

    #[test]
    fn chapter_index_from_mpv_rejects_negative() {
        const CASES: &[(&str, i64, Option<u32>)] = &[
            ("before the first chapter", -1, None),
            ("first", 0, Some(0)),
            ("third", 2, Some(2)),
        ];
        for (name, input, expected) in CASES {
            assert_eq!(
                ChapterIndex::from_mpv(*input).map(ChapterIndex::get),
                *expected,
                "{name}"
            );
        }
    }
}
