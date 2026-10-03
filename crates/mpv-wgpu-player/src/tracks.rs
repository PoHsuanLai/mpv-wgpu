//! The file's video, audio, and subtitle tracks, read from mpv's `track-list`.

use std::fmt;
use std::num::NonZeroU32;

use crate::value::Node;

use crate::nodes::{field, flag, int, text};

/// Which stream family a track belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackKind {
    /// Moving pictures, or a cover image shown for an audio file.
    Video,
    /// Sound.
    Audio,
    /// Text or bitmap subtitles.
    Subtitle,
}

impl TrackKind {
    /// The name mpv uses for the kind in `track-list`.
    fn parse(text: &str) -> Option<Self> {
        match text {
            "video" => Some(TrackKind::Video),
            "audio" => Some(TrackKind::Audio),
            "sub" => Some(TrackKind::Subtitle),
            _ => None,
        }
    }

    /// The mpv option that selects a track of this kind.
    pub(crate) fn option(self) -> &'static str {
        match self {
            TrackKind::Video => "vid",
            TrackKind::Audio => "aid",
            TrackKind::Subtitle => "sid",
        }
    }
}

impl fmt::Display for TrackKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TrackKind::Video => "video",
            TrackKind::Audio => "audio",
            TrackKind::Subtitle => "subtitle",
        })
    }
}

/// mpv's track id. It counts from 1 within each [`TrackKind`], so audio track 1
/// and video track 1 are different tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackId(NonZeroU32);

impl TrackId {
    /// `None` for 0, which mpv never assigns.
    pub fn new(id: u32) -> Option<Self> {
        NonZeroU32::new(id).map(Self)
    }

    /// The id as mpv numbers it, at least 1.
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What [`crate::Player::select_track`] asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackChoice {
    /// Play no track of the kind.
    Off,
    /// Let mpv pick by its language and default rules.
    Auto,
    /// Play exactly this track.
    Id(TrackId),
}

impl TrackChoice {
    pub(crate) fn as_mpv(self) -> String {
        match self {
            TrackChoice::Off => "no".to_string(),
            TrackChoice::Auto => "auto".to_string(),
            TrackChoice::Id(id) => id.get().to_string(),
        }
    }
}

/// Whether the container marks the track as its default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackDefault {
    /// The file flags this track as the default of its kind.
    Marked,
    /// No default flag.
    Unmarked,
}

/// Whether mpv is playing the track now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackSelection {
    /// Decoded and presented.
    Selected,
    /// Available, not playing.
    Unselected,
}

/// Where the track came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackOrigin {
    /// Inside the opened file.
    Embedded,
    /// A separate file mpv loaded next to it, such as an `.srt`.
    External,
}

/// What a video track holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackArt {
    /// A single image attached as album art or cover.
    Cover,
    /// Ordinary video, and every track that is not video.
    Regular,
}

/// One entry of mpv's `track-list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    /// Id within [`Track::kind`].
    pub id: TrackId,
    /// Video, audio, or subtitle.
    pub kind: TrackKind,
    /// The title tag, when the file has one.
    pub title: Option<String>,
    /// The language tag, usually an ISO 639 code.
    pub lang: Option<String>,
    /// The codec name, such as `h264` or `aac`.
    pub codec: Option<String>,
    /// Whether the file marks it as default.
    pub default: TrackDefault,
    /// Whether mpv is playing it.
    pub selected: TrackSelection,
    /// Embedded or loaded from another file.
    pub origin: TrackOrigin,
    /// Cover image or regular video.
    pub art: TrackArt,
}

/// Every track of the current file, in mpv's order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrackList(Vec<Track>);

impl TrackList {
    /// All tracks.
    pub fn as_slice(&self) -> &[Track] {
        &self.0
    }

    /// All tracks.
    pub fn iter(&self) -> std::slice::Iter<'_, Track> {
        self.0.iter()
    }

    /// The tracks of one kind.
    pub fn of_kind(&self, kind: TrackKind) -> impl Iterator<Item = &Track> {
        self.0.iter().filter(move |track| track.kind == kind)
    }

    /// The track of `kind` mpv is playing.
    pub fn selected(&self, kind: TrackKind) -> Option<&Track> {
        self.of_kind(kind)
            .find(|track| track.selected == TrackSelection::Selected)
    }

    /// The track of `kind` with `id`.
    pub fn find(&self, kind: TrackKind, id: TrackId) -> Option<&Track> {
        self.of_kind(kind).find(|track| track.id == id)
    }

    /// Number of tracks of every kind.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the file has no tracks, or none have been read yet.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'a> IntoIterator for &'a TrackList {
    type Item = &'a Track;
    type IntoIter = std::slice::Iter<'a, Track>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Parse mpv's `track-list` node. Anything that is not an array yields an empty
/// list, and entries with an unknown type or an id below 1 are dropped.
pub(crate) fn parse_track_list(node: &Node) -> TrackList {
    let Node::Array(items) = node else {
        return TrackList::default();
    };
    TrackList(items.iter().filter_map(parse_track).collect())
}

fn parse_track(node: &Node) -> Option<Track> {
    let Node::Map(entries) = node else {
        return None;
    };
    let kind = match field(entries, "type") {
        Some(Node::String(name)) => TrackKind::parse(name)?,
        _ => return None,
    };
    let id = TrackId::new(u32::try_from(int(entries, "id")?).ok()?)?;
    Some(Track {
        id,
        kind,
        title: text(entries, "title"),
        lang: text(entries, "lang"),
        codec: text(entries, "codec"),
        default: if flag(entries, "default") {
            TrackDefault::Marked
        } else {
            TrackDefault::Unmarked
        },
        selected: if flag(entries, "selected") {
            TrackSelection::Selected
        } else {
            TrackSelection::Unselected
        },
        origin: if flag(entries, "external") {
            TrackOrigin::External
        } else {
            TrackOrigin::Embedded
        },
        art: if kind == TrackKind::Video && flag(entries, "albumart") {
            TrackArt::Cover
        } else {
            TrackArt::Regular
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, Node)]) -> Node {
        Node::Map(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect(),
        )
    }

    fn s(value: &str) -> Node {
        Node::String(value.to_string())
    }

    fn id(value: u32) -> TrackId {
        TrackId::new(value).expect("non-zero id")
    }

    #[test]
    fn parse_track_list_reads_every_field() {
        let node = Node::Array(vec![
            map(&[
                ("id", Node::Int64(1)),
                ("type", s("video")),
                ("codec", s("h264")),
                ("default", Node::Flag(true)),
                ("selected", Node::Flag(true)),
                ("albumart", Node::Flag(false)),
            ]),
            map(&[
                ("id", Node::Int64(2)),
                ("type", s("audio")),
                ("title", s("Commentary")),
                ("lang", s("en")),
                ("codec", s("aac")),
                ("default", Node::Flag(false)),
                ("selected", Node::Flag(false)),
            ]),
            map(&[
                ("id", Node::Int64(1)),
                ("type", s("sub")),
                ("lang", s("de")),
                ("external", Node::Flag(true)),
            ]),
        ]);
        let list = parse_track_list(&node);
        let expected = vec![
            Track {
                id: id(1),
                kind: TrackKind::Video,
                title: None,
                lang: None,
                codec: Some("h264".into()),
                default: TrackDefault::Marked,
                selected: TrackSelection::Selected,
                origin: TrackOrigin::Embedded,
                art: TrackArt::Regular,
            },
            Track {
                id: id(2),
                kind: TrackKind::Audio,
                title: Some("Commentary".into()),
                lang: Some("en".into()),
                codec: Some("aac".into()),
                default: TrackDefault::Unmarked,
                selected: TrackSelection::Unselected,
                origin: TrackOrigin::Embedded,
                art: TrackArt::Regular,
            },
            Track {
                id: id(1),
                kind: TrackKind::Subtitle,
                title: None,
                lang: Some("de".into()),
                codec: None,
                default: TrackDefault::Unmarked,
                selected: TrackSelection::Unselected,
                origin: TrackOrigin::External,
                art: TrackArt::Regular,
            },
        ];
        assert_eq!(list.as_slice(), expected.as_slice());
    }

    #[test]
    fn parse_track_list_drops_unusable_entries() {
        let cases: Vec<(&str, Node, usize)> = vec![
            ("not an array", Node::None, 0),
            ("empty array", Node::Array(vec![]), 0),
            (
                "unknown type",
                Node::Array(vec![map(&[("id", Node::Int64(1)), ("type", s("data"))])]),
                0,
            ),
            (
                "zero id",
                Node::Array(vec![map(&[("id", Node::Int64(0)), ("type", s("audio"))])]),
                0,
            ),
            (
                "negative id",
                Node::Array(vec![map(&[("id", Node::Int64(-1)), ("type", s("audio"))])]),
                0,
            ),
            (
                "missing id",
                Node::Array(vec![map(&[("type", s("audio"))])]),
                0,
            ),
            ("entry is not a map", Node::Array(vec![s("audio")]), 0),
            (
                "valid entry beside a bad one",
                Node::Array(vec![
                    map(&[("type", s("audio"))]),
                    map(&[("id", Node::Int64(3)), ("type", s("audio"))]),
                ]),
                1,
            ),
        ];
        for (name, node, kept) in &cases {
            assert_eq!(parse_track_list(node).len(), *kept, "{name}");
        }
    }

    #[test]
    fn empty_strings_are_absent_tags() {
        let node = Node::Array(vec![map(&[
            ("id", Node::Int64(1)),
            ("type", s("audio")),
            ("title", s("")),
            ("lang", s("")),
        ])]);
        let list = parse_track_list(&node);
        let track = list.find(TrackKind::Audio, id(1)).expect("track");
        assert_eq!(
            (track.title.as_deref(), track.lang.as_deref()),
            (None, None)
        );
    }

    #[test]
    fn album_art_only_marks_video_tracks() {
        let node = Node::Array(vec![
            map(&[
                ("id", Node::Int64(1)),
                ("type", s("video")),
                ("albumart", Node::Flag(true)),
                ("selected", Node::Flag(true)),
            ]),
            map(&[
                ("id", Node::Int64(1)),
                ("type", s("audio")),
                ("albumart", Node::Flag(true)),
            ]),
        ]);
        let list = parse_track_list(&node);
        let video = list.selected(TrackKind::Video).expect("video");
        assert_eq!(video.art, TrackArt::Cover);
        let audio = list.of_kind(TrackKind::Audio).next().expect("audio");
        assert_eq!(audio.art, TrackArt::Regular);
    }

    #[test]
    fn lookups_respect_the_kind() {
        let node = Node::Array(vec![
            map(&[("id", Node::Int64(1)), ("type", s("video"))]),
            map(&[
                ("id", Node::Int64(1)),
                ("type", s("audio")),
                ("selected", Node::Flag(true)),
            ]),
        ]);
        let list = parse_track_list(&node);
        assert_eq!(list.len(), 2);
        assert!(list.selected(TrackKind::Video).is_none());
        assert!(list.selected(TrackKind::Subtitle).is_none());
        assert_eq!(
            list.selected(TrackKind::Audio).map(|track| track.kind),
            Some(TrackKind::Audio)
        );
        assert!(list.find(TrackKind::Subtitle, id(1)).is_none());
        assert_eq!(list.of_kind(TrackKind::Video).count(), 1);
    }

    #[test]
    fn track_choice_maps_to_the_mpv_value() {
        let cases: Vec<(&str, TrackChoice, &str)> = vec![
            ("off", TrackChoice::Off, "no"),
            ("auto", TrackChoice::Auto, "auto"),
            ("id", TrackChoice::Id(id(7)), "7"),
        ];
        for (name, choice, expected) in cases {
            assert_eq!(choice.as_mpv(), expected, "{name}");
        }
    }

    #[test]
    fn track_kind_maps_to_the_selection_option() {
        const CASES: &[(&str, TrackKind, &str)] = &[
            ("video", TrackKind::Video, "vid"),
            ("audio", TrackKind::Audio, "aid"),
            ("subtitle", TrackKind::Subtitle, "sid"),
        ];
        for (name, kind, option) in CASES {
            assert_eq!(kind.option(), *option, "{name}");
        }
        assert!(TrackId::new(0).is_none());
    }
}
