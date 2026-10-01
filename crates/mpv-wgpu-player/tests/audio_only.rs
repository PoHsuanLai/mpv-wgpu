//! Audio-only files: no errors, no picture, and cover art shown as video.

mod support;

use std::time::Duration;

use mpv_wgpu_player::{EndReason, Event, Picture, Playback, TrackArt, TrackKind, VideoPresence};
use support::Harness;

#[test]
fn plain_audio_loads_without_a_picture_or_an_error() {
    let Some(mut harness) = Harness::open().map(Harness::with_slot) else {
        return;
    };
    harness.load("tone.flac");
    harness.wait_for("an audio track", |h| !h.player.tracks().is_empty());
    harness.run_for(Duration::from_millis(500));

    assert_eq!(harness.player.has_video(), VideoPresence::Absent);
    assert!(matches!(harness.player.picture(), Picture::Waiting));
    let tracks = harness.player.tracks();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks.of_kind(TrackKind::Audio).count(), 1);
    assert!(
        !harness.saw(&Event::Ended(EndReason::Error)),
        "events: {:?}",
        harness.events
    );
    assert!(harness.player.duration().is_some_and(|d| d.get() > 1.5));
}

#[test]
fn plain_audio_plays_to_the_end_without_an_error() {
    let Some(mut harness) = Harness::open().map(Harness::with_slot) else {
        return;
    };
    harness.load("tone.flac");
    // keep-open holds the file at its end: mpv pauses instead of sending EndFile.
    harness.wait_for("the pause at end of file", |h| {
        h.saw(&Event::Playback(Playback::Paused))
    });
    assert!(
        !harness.events.iter().any(|e| matches!(e, Event::Ended(_))),
        "{:?}",
        harness.events
    );
    assert!(matches!(harness.player.picture(), Picture::Waiting));
    let position = harness.player.position().expect("position").get();
    assert!(position > 1.5, "played to {position}");
}

#[test]
fn embedded_cover_art_is_shown_as_the_picture() {
    let Some(mut harness) = Harness::open().map(Harness::with_slot) else {
        return;
    };
    harness.load("cover.mp3");
    harness.wait_for("a shown cover", |h| {
        matches!(h.player.picture(), Picture::Shown(_))
    });
    assert_eq!(harness.player.has_video(), VideoPresence::CoverArt);
    let tracks = harness.player.tracks();
    let video = tracks.selected(TrackKind::Video).expect("cover track");
    assert_eq!(video.art, TrackArt::Cover);
    assert_eq!(video.codec.as_deref(), Some("png"));
    assert!(!harness.saw(&Event::Ended(EndReason::Error)));
}

#[test]
fn cover_art_survives_pause_and_a_new_slot_is_painted() {
    let Some(mut harness) = Harness::open().map(Harness::with_slot) else {
        return;
    };
    harness.load("cover.mp3");
    harness
        .player
        .set_playback(Playback::Paused)
        .expect("pause");
    harness.wait_for("a shown cover", |h| {
        matches!(h.player.picture(), Picture::Shown(_))
    });
    harness.run_for(Duration::from_millis(200));
    assert!(matches!(harness.player.picture(), Picture::Shown(_)));
}
