//! Small closed choices for player commands and queries.

use crate::tracks::{TrackArt, TrackKind, TrackList};

/// Which way [`crate::Player::frame_step`] moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// To the next frame.
    Forward,
    /// To the previous frame.
    Backward,
}

impl Direction {
    pub(crate) fn command(self) -> &'static str {
        match self {
            Direction::Forward => "frame-step",
            Direction::Backward => "frame-back-step",
        }
    }
}

/// What a screenshot contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotContent {
    /// The decoded picture at source resolution, without subtitles or OSD.
    Video,
    /// The source-resolution picture with subtitles burned in.
    Subtitles,
    /// What the host sees: the slot-sized frame with subtitles and OSD.
    Window,
}

impl ScreenshotContent {
    pub(crate) fn as_mpv(self) -> &'static str {
        match self {
            ScreenshotContent::Video => "video",
            ScreenshotContent::Subtitles => "subtitles",
            ScreenshotContent::Window => "window",
        }
    }
}

/// Whether the current file shows a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoPresence {
    /// No video track is playing: audio-only files, or nothing loaded.
    Absent,
    /// The playing video track is a still cover image attached to the file.
    CoverArt,
    /// Ordinary video is playing.
    Present,
}

impl VideoPresence {
    pub(crate) fn of(tracks: &TrackList) -> Self {
        match tracks.selected(TrackKind::Video) {
            None => VideoPresence::Absent,
            Some(track) => match track.art {
                TrackArt::Cover => VideoPresence::CoverArt,
                TrackArt::Regular => VideoPresence::Present,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::value::Node;

    use super::*;
    use crate::tracks::parse_track_list;

    fn video(albumart: bool, selected: bool) -> Node {
        Node::Array(vec![Node::Map(vec![
            ("id".into(), Node::Int64(1)),
            ("type".into(), Node::String("video".into())),
            ("albumart".into(), Node::Flag(albumart)),
            ("selected".into(), Node::Flag(selected)),
        ])])
    }

    #[test]
    fn video_presence_follows_the_selected_video_track() {
        let cases: Vec<(&str, Node, VideoPresence)> = vec![
            ("no tracks", Node::Array(vec![]), VideoPresence::Absent),
            (
                "video track not selected",
                video(false, false),
                VideoPresence::Absent,
            ),
            ("selected video", video(false, true), VideoPresence::Present),
            ("selected cover", video(true, true), VideoPresence::CoverArt),
            (
                "unselected cover",
                video(true, false),
                VideoPresence::Absent,
            ),
        ];
        for (name, node, expected) in cases {
            assert_eq!(
                VideoPresence::of(&parse_track_list(&node)),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn commands_and_flags_map_to_mpv_words() {
        assert_eq!(Direction::Forward.command(), "frame-step");
        assert_eq!(Direction::Backward.command(), "frame-back-step");
        const SHOTS: &[(ScreenshotContent, &str)] = &[
            (ScreenshotContent::Video, "video"),
            (ScreenshotContent::Subtitles, "subtitles"),
            (ScreenshotContent::Window, "window"),
        ];
        for (content, word) in SHOTS {
            assert_eq!(content.as_mpv(), *word);
        }
    }
}
