//! The typed state API against a generated clip: video, two audio tracks,
//! one subtitle track, and two chapters. Headless, on `ao=null`.

#[macro_use]
mod support;

use std::time::Duration;

use mpv_wgpu_player::{
    ChapterIndex, Direction, Error, Event, Picture, Playback, ScreenshotContent, Seek, Speed,
    TrackArt, TrackChoice, TrackDefault, TrackId, TrackKind, TrackSelection, VideoPresence, Volume,
};
use support::{Harness, Mode, scratch_dir};

fn id(value: u32) -> TrackId {
    TrackId::new(value).expect("non-zero id")
}

fn opened(mode: Mode) -> Option<Harness> {
    let mut harness = Harness::open(mode)?.with_slot();
    harness.load("clip.mkv");
    harness.wait_for("tracks and chapters", |h| {
        !h.player.tracks().is_empty() && !h.player.chapters().is_empty()
    });
    Some(harness)
}

fn clip_lists_its_tracks_and_chapters(mode: Mode) {
    let Some(harness) = opened(mode) else { return };
    let tracks = harness.player.tracks();
    assert!(harness.saw(&Event::TracksChanged));
    assert!(harness.saw(&Event::ChaptersChanged));
    assert_eq!(tracks.len(), 4);
    assert_eq!(tracks.of_kind(TrackKind::Video).count(), 1);
    assert_eq!(tracks.of_kind(TrackKind::Audio).count(), 2);
    assert_eq!(tracks.of_kind(TrackKind::Subtitle).count(), 1);

    let video = tracks.selected(TrackKind::Video).expect("video selected");
    assert_eq!(video.art, TrackArt::Regular);
    assert_eq!(video.selected, TrackSelection::Selected);

    let main = tracks.find(TrackKind::Audio, id(1)).expect("audio 1");
    assert_eq!(main.title.as_deref(), Some("Main"));
    assert_eq!(main.lang.as_deref(), Some("eng"));
    assert_eq!(main.selected, TrackSelection::Selected);
    assert_eq!(main.default, TrackDefault::Marked);

    let second = tracks.find(TrackKind::Audio, id(2)).expect("audio 2");
    assert_eq!(second.lang.as_deref(), Some("deu"));
    assert_eq!(second.selected, TrackSelection::Unselected);

    let chapters = harness.player.chapters();
    let titles: Vec<&str> = chapters.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, ["Opening", "Middle"]);
    assert_eq!(chapters[0].start.get(), 0.0);
    assert_eq!(chapters[1].start.get(), 1.0);
    assert_eq!(harness.player.has_video(), VideoPresence::Present);
}

fn select_track_switches_audio_and_turns_subtitles_off(mode: Mode) {
    let Some(mut harness) = opened(mode) else {
        return;
    };
    harness.clear_events();
    harness
        .player
        .select_track(TrackKind::Audio, TrackChoice::Id(id(2)))
        .expect("select audio 2");
    harness.wait_for("audio 2 selected", |h| {
        h.player
            .tracks()
            .selected(TrackKind::Audio)
            .is_some_and(|track| track.id == id(2))
    });
    assert!(harness.saw(&Event::TracksChanged));

    harness
        .player
        .select_track(TrackKind::Subtitle, TrackChoice::Off)
        .expect("subtitles off");
    harness.wait_for("subtitles off", |h| {
        h.player.tracks().selected(TrackKind::Subtitle).is_none()
    });

    harness
        .player
        .select_track(TrackKind::Subtitle, TrackChoice::Auto)
        .expect("subtitles auto");
    harness
        .player
        .select_track(TrackKind::Subtitle, TrackChoice::Id(id(1)))
        .expect("subtitles 1");
    harness.wait_for("subtitle 1 selected", |h| {
        h.player.tracks().selected(TrackKind::Subtitle).is_some()
    });
}

fn volume_is_absolute_clamped_and_announced(mode: Mode) {
    let Some(mut harness) = opened(mode) else {
        return;
    };
    assert_eq!(harness.player.volume(), Volume::DEFAULT);
    harness.clear_events();
    harness
        .player
        .set_volume(Volume::new(35))
        .expect("set volume");
    assert_eq!(harness.player.volume(), Volume::new(35));
    harness.wait_for("VolumeChanged", |h| h.saw(&Event::VolumeChanged));

    harness
        .player
        .set_volume(Volume::new(400))
        .expect("set volume");
    assert_eq!(harness.player.volume(), Volume::MAX);
    assert_eq!(harness.player.volume().percent(), 150);
}

fn speed_round_trips(mode: Mode) {
    let Some(harness) = opened(mode) else { return };
    assert_eq!(harness.player.speed(), Speed::NORMAL);
    let double = Speed::from_ratio(mpv_wgpu_player::Finite::new(2.0).expect("finite"));
    harness.player.set_speed(double).expect("set speed");
    assert_eq!(harness.player.speed(), double);
    assert_eq!(harness.player.speed().ratio(), 2.0);
}

fn seek_reports_done_and_moves_the_position(mode: Mode) {
    let Some(mut harness) = opened(mode) else {
        return;
    };
    harness
        .player
        .set_playback(Playback::Paused)
        .expect("pause");
    harness.run_for(Duration::from_millis(200));
    harness.clear_events();
    let target = mpv_wgpu_player::Finite::new(1.5).expect("finite");
    harness.player.seek(Seek::Absolute(target)).expect("seek");
    harness.wait_for("SeekDone", |h| h.saw(&Event::SeekDone));
    harness.wait_for("position near 1.5", |h| {
        h.player
            .position()
            .is_some_and(|p| (p.get() - 1.5).abs() < 0.2)
    });
}

fn set_chapter_seeks_and_rejects_a_missing_index(mode: Mode) {
    let Some(mut harness) = opened(mode) else {
        return;
    };
    harness
        .player
        .set_playback(Playback::Paused)
        .expect("pause");
    harness
        .player
        .set_chapter(ChapterIndex::new(1))
        .expect("chapter 1");
    harness.wait_for("chapter 1 current", |h| {
        h.player.chapter() == Some(ChapterIndex::new(1))
    });
    harness.wait_for("position past 1s", |h| {
        h.player.position().is_some_and(|p| p.get() >= 0.9)
    });
    let err = harness
        .player
        .set_chapter(ChapterIndex::new(2))
        .expect_err("only two chapters");
    assert!(matches!(err, Error::NoSuchChapter(2)), "{err:?}");
}

fn frame_step_advances_and_retreats_by_one_frame(mode: Mode) {
    let Some(mut harness) = opened(mode) else {
        return;
    };
    harness
        .player
        .set_playback(Playback::Paused)
        .expect("pause");
    harness.run_for(Duration::from_millis(300));
    harness.clear_events();
    harness
        .player
        .seek(Seek::Absolute(
            mpv_wgpu_player::Finite::new(1.0).expect("finite"),
        ))
        .expect("seek");
    harness.wait_for("SeekDone", |h| h.saw(&Event::SeekDone));
    harness.run_for(Duration::from_millis(200));
    let start = harness.player.position().expect("position").get();

    harness
        .player
        .frame_step(Direction::Forward)
        .expect("step forward");
    harness.wait_for("position after the forward step", |h| {
        h.player.position().is_some_and(|p| p.get() > start + 0.05)
    });
    let after = harness.player.position().expect("position").get();
    assert!(
        (after - start - 0.1).abs() < 0.03,
        "one frame at 10 fps is 0.1 s: {start} -> {after}"
    );

    harness
        .player
        .frame_step(Direction::Backward)
        .expect("step back");
    harness.wait_for("position after the backward step", |h| {
        h.player.position().is_some_and(|p| p.get() < after - 0.05)
    });
}

fn png_size(path: &std::path::Path) -> (u32, u32) {
    let bytes = std::fs::read(path).expect("screenshot file");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "{path:?} is a png");
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("ihdr width"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("ihdr height"));
    (width, height)
}

fn screenshot_video_and_subtitles_are_source_size_and_window_is_slot_size(mode: Mode) {
    let Some(mut harness) = Harness::open(mode).map(|h| h.with_slot_of(160, 120)) else {
        return;
    };
    harness.load("clip.mkv");
    harness
        .player
        .set_playback(Playback::Paused)
        .expect("pause");
    harness.wait_for("a shown frame", |h| {
        matches!(h.player.picture(), Picture::Shown(_))
    });
    let dir = scratch_dir("screenshot");
    const CASES: &[(ScreenshotContent, &str, (u32, u32))] = &[
        (ScreenshotContent::Video, "video.png", (64, 48)),
        (ScreenshotContent::Subtitles, "subtitles.png", (64, 48)),
        (ScreenshotContent::Window, "window.png", (160, 120)),
    ];
    for (content, name, expected) in CASES {
        let path = dir.join(name);
        harness
            .player
            .screenshot_to_file(&path, *content)
            .unwrap_or_else(|err| panic!("{content:?}: {err}"));
        assert_eq!(png_size(&path), *expected, "{content:?}");
    }
    std::fs::remove_dir_all(dir).expect("cleanup");
}

in_each_mode!(
    clip_lists_its_tracks_and_chapters,
    select_track_switches_audio_and_turns_subtitles_off,
    volume_is_absolute_clamped_and_announced,
    speed_round_trips,
    seek_reports_done_and_moves_the_position,
    set_chapter_seeks_and_rejects_a_missing_index,
    frame_step_advances_and_retreats_by_one_frame,
    screenshot_video_and_subtitles_are_source_size_and_window_is_slot_size,
);
