//! Player state mirrored from observed mpv properties.

use crate::value::PropertyData;

use crate::chapters::{Chapter, ChapterIndex, parse_chapter_list};
use crate::quantities::{Percent, Volume};
use crate::tracks::{TrackList, parse_track_list};
use crate::types::Event;

/// Where a seek is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeekState {
    Idle,
    InProgress,
}

/// Tracks, chapters, volume, seeking, and cache level as mpv last reported them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MediaState {
    pub(crate) tracks: TrackList,
    pub(crate) chapters: Vec<Chapter>,
    pub(crate) chapter: Option<ChapterIndex>,
    pub(crate) volume: Volume,
    seek: SeekState,
    buffering: Percent,
}

impl MediaState {
    pub(crate) fn new() -> Self {
        Self {
            tracks: TrackList::default(),
            chapters: Vec::new(),
            chapter: None,
            volume: Volume::DEFAULT,
            seek: SeekState::Idle,
            buffering: Percent::FULL,
        }
    }

    /// Fold one property change in. The event is returned only when the value
    /// differs from what was held, so mpv's initial echo of an observed
    /// property stays silent.
    pub(crate) fn apply(&mut self, name: &str, data: &PropertyData) -> Option<Event> {
        match (name, data) {
            ("track-list", PropertyData::Node(node)) => self.set_tracks(parse_track_list(node)),
            ("track-list", PropertyData::None) => self.set_tracks(TrackList::default()),
            ("chapter-list", PropertyData::Node(node)) => {
                self.set_chapters(parse_chapter_list(node))
            }
            ("chapter-list", PropertyData::None) => self.set_chapters(Vec::new()),
            ("chapter", PropertyData::Int64(value)) => {
                self.chapter = ChapterIndex::from_mpv(*value);
                None
            }
            ("chapter", PropertyData::None) => {
                self.chapter = None;
                None
            }
            ("volume", PropertyData::Double(value)) => {
                let next = Volume::from_mpv(*value)?;
                (next != std::mem::replace(&mut self.volume, next)).then_some(Event::VolumeChanged)
            }
            ("seeking", PropertyData::Flag(active)) => {
                let next = if *active {
                    SeekState::InProgress
                } else {
                    SeekState::Idle
                };
                let before = std::mem::replace(&mut self.seek, next);
                (before == SeekState::InProgress && next == SeekState::Idle)
                    .then_some(Event::SeekDone)
            }
            ("cache-buffering-state", PropertyData::Int64(value)) => {
                let next = Percent::from_mpv(*value);
                (next != std::mem::replace(&mut self.buffering, next))
                    .then_some(Event::Buffering(next))
            }
            _ => None,
        }
    }

    fn set_tracks(&mut self, next: TrackList) -> Option<Event> {
        let before = std::mem::replace(&mut self.tracks, next);
        (before != self.tracks).then_some(Event::TracksChanged)
    }

    fn set_chapters(&mut self, next: Vec<Chapter>) -> Option<Event> {
        let before = std::mem::replace(&mut self.chapters, next);
        (before != self.chapters).then_some(Event::ChaptersChanged)
    }
}

#[cfg(test)]
mod tests {
    use crate::value::Node;

    use super::*;

    fn audio_tracks(selected: bool) -> PropertyData {
        PropertyData::Node(Node::Array(vec![Node::Map(vec![
            ("id".into(), Node::Int64(1)),
            ("type".into(), Node::String("audio".into())),
            ("selected".into(), Node::Flag(selected)),
        ])]))
    }

    fn chapters(count: usize) -> PropertyData {
        PropertyData::Node(Node::Array(
            (0..count)
                .map(|index| {
                    Node::Map(vec![
                        ("title".into(), Node::String(format!("c{index}"))),
                        ("time".into(), Node::Double(index as f64)),
                    ])
                })
                .collect(),
        ))
    }

    #[test]
    fn apply_emits_only_when_the_value_changes() {
        let cases: Vec<(&str, &str, PropertyData, PropertyData, Option<Event>)> = vec![
            (
                "tracks change",
                "track-list",
                audio_tracks(false),
                audio_tracks(true),
                Some(Event::TracksChanged),
            ),
            (
                "tracks repeat",
                "track-list",
                audio_tracks(true),
                audio_tracks(true),
                None,
            ),
            (
                "chapters change",
                "chapter-list",
                chapters(2),
                chapters(3),
                Some(Event::ChaptersChanged),
            ),
            (
                "chapters repeat",
                "chapter-list",
                chapters(2),
                chapters(2),
                None,
            ),
            (
                "volume changes",
                "volume",
                PropertyData::Double(50.0),
                PropertyData::Double(60.0),
                Some(Event::VolumeChanged),
            ),
            (
                "volume repeats",
                "volume",
                PropertyData::Double(50.0),
                PropertyData::Double(50.0),
                None,
            ),
            (
                "volume rounding to the same percent",
                "volume",
                PropertyData::Double(50.0),
                PropertyData::Double(50.2),
                None,
            ),
            (
                "seek finishes",
                "seeking",
                PropertyData::Flag(true),
                PropertyData::Flag(false),
                Some(Event::SeekDone),
            ),
            (
                "seek starts",
                "seeking",
                PropertyData::Flag(false),
                PropertyData::Flag(true),
                None,
            ),
            (
                "buffering drops",
                "cache-buffering-state",
                PropertyData::Int64(100),
                PropertyData::Int64(40),
                Some(Event::Buffering(Percent::new(40))),
            ),
            (
                "buffering repeats",
                "cache-buffering-state",
                PropertyData::Int64(40),
                PropertyData::Int64(40),
                None,
            ),
        ];
        for (name, property, first, second, expected) in cases {
            let mut state = MediaState::new();
            state.apply(property, &first);
            assert_eq!(state.apply(property, &second), expected, "{name}");
        }
    }

    #[test]
    fn apply_ignores_unrelated_and_mistyped_properties() {
        let mut state = MediaState::new();
        let before = state.clone();
        assert_eq!(state.apply("pause", &PropertyData::Flag(true)), None);
        assert_eq!(
            state.apply("volume", &PropertyData::String("loud".into())),
            None
        );
        assert_eq!(state.apply("volume", &PropertyData::Double(f64::NAN)), None);
        assert_eq!(state, before);
    }

    #[test]
    fn apply_tracks_the_current_chapter_and_clears_on_unavailable() {
        let mut state = MediaState::new();
        state.apply("chapter", &PropertyData::Int64(2));
        assert_eq!(state.chapter.map(ChapterIndex::get), Some(2));
        state.apply("chapter", &PropertyData::Int64(-1));
        assert_eq!(state.chapter, None);
        state.apply("chapter", &PropertyData::Int64(1));
        state.apply("chapter", &PropertyData::None);
        assert_eq!(state.chapter, None);
    }

    #[test]
    fn unavailable_lists_clear_the_state_and_announce_it_once() {
        let mut state = MediaState::new();
        state.apply("track-list", &audio_tracks(true));
        assert_eq!(
            state.apply("track-list", &PropertyData::None),
            Some(Event::TracksChanged)
        );
        assert!(state.tracks.is_empty());
        assert_eq!(state.apply("track-list", &PropertyData::None), None);
    }

    #[test]
    fn volume_above_the_maximum_is_held_at_the_maximum() {
        let mut state = MediaState::new();
        state.apply("volume", &PropertyData::Double(200.0));
        assert_eq!(state.volume, Volume::MAX);
    }
}
