//! Subprocess mode: the mpv child's lifecycle and failures.
#![cfg(feature = "subprocess")]

#[macro_use]
mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use mpv_wgpu_player::{
    AudioOutput, Error, Event, Host, Picture, Player, PlayerOptions, SubprocessOptions,
};
use support::{Harness, Mode, find_mpv, open_device};

/// Pids of processes whose command line contains `marker`.
fn pids_with(marker: &str) -> Vec<u32> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if String::from_utf8_lossy(&cmdline).contains(marker) {
            found.push(pid);
        }
    }
    found
}

fn alive(pid: u32) -> bool {
    // A zombie has no cmdline; a running process does.
    std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|bytes| !bytes.is_empty())
}

fn marked_host(marker: &str) -> Option<Host> {
    let mpv = find_mpv().or_else(|| {
        eprintln!("skipped: no mpv (set MPV_WGPU_MPV or put mpv on PATH)");
        None
    })?;
    Some(Host::Subprocess(SubprocessOptions {
        mpv: Some(mpv),
        extra_args: vec![format!("--force-media-title={marker}")],
        ..SubprocessOptions::default()
    }))
}

fn player_on(host: Host) -> Result<Player, Error> {
    let (device, queue) = open_device().expect("a wgpu device");
    Player::with_host(
        &device,
        &queue,
        PlayerOptions {
            audio_output: AudioOutput::Null,
        },
        host,
    )
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn killing_mpv_is_host_gone_and_stays_so() {
    let marker = format!("mpv-wgpu-crash-{}", std::process::id());
    let Some(host) = marked_host(&marker) else {
        return;
    };
    if open_device().is_err() {
        return;
    }
    let mut player = player_on(host).expect("player");
    let woken = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&woken);
    player.set_notify(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    player
        .load(
            support::fixture("clip.mkv")
                .to_str()
                .expect("utf-8 fixture path"),
        )
        .expect("loadfile");
    wait_until("a loaded event", || {
        let _ = player.poll().expect("poll while mpv runs");
        player.events().contains(&Event::Loaded)
    });
    assert!(woken.load(Ordering::SeqCst) > 0, "the reader woke the host");

    let pids = pids_with(&marker);
    assert_eq!(pids.len(), 1, "one mpv child: {pids:?}");
    let status = std::process::Command::new("kill")
        .args(["-9", &pids[0].to_string()])
        .status()
        .expect("run kill");
    assert!(status.success());

    let before = woken.load(Ordering::SeqCst);
    let mut seen = None;
    wait_until("HostGone", || match player.poll() {
        Ok(_) => false,
        Err(error) => {
            seen = Some(error);
            true
        }
    });
    assert!(matches!(seen, Some(Error::HostGone)), "{seen:?}");
    assert!(
        woken.load(Ordering::SeqCst) > before,
        "the crash woke the host"
    );
    assert!(matches!(player.load("x"), Err(Error::HostGone)));
    assert!(matches!(player.command(&["stop"]), Err(Error::HostGone)));
    assert!(matches!(player.poll(), Err(Error::HostGone)));
    drop(player);
    wait_until("the child to be reaped", || !alive(pids[0]));
}

#[test]
fn dropping_the_player_ends_mpv() {
    let marker = format!("mpv-wgpu-drop-{}", std::process::id());
    let Some(host) = marked_host(&marker) else {
        return;
    };
    if open_device().is_err() {
        return;
    }
    let player = player_on(host).expect("player");
    let pids = pids_with(&marker);
    assert_eq!(pids.len(), 1, "one mpv child: {pids:?}");
    let started = Instant::now();
    drop(player);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "drop took {:?}",
        started.elapsed()
    );
    assert!(!alive(pids[0]), "mpv {} outlived the player", pids[0]);
    assert!(pids_with(&marker).is_empty());
}

#[test]
fn quitting_mpv_by_command_is_host_gone() {
    let Some(host) = marked_host("mpv-wgpu-quit") else {
        return;
    };
    if open_device().is_err() {
        return;
    }
    let mut player = player_on(host).expect("player");
    // mpv may answer the quit or close the socket first; either way it is over.
    let _ = player.command(&["quit"]);
    wait_until("HostGone after quit", || {
        matches!(player.poll(), Err(Error::HostGone))
    });
}

#[test]
fn a_missing_mpv_is_a_start_error_that_says_how_to_fix_it() {
    if open_device().is_err() {
        return;
    }
    let host = Host::Subprocess(SubprocessOptions {
        mpv: Some(PathBuf::from("/nonexistent/mpv")),
        ..SubprocessOptions::default()
    });
    let Err(Error::HostStart(text)) = player_on(host) else {
        panic!("expected HostStart");
    };
    assert!(text.contains("/nonexistent/mpv"), "{text}");
    assert!(text.contains("MPV_WGPU_MPV"), "{text}");
}

#[test]
fn a_missing_plugin_is_a_start_error_that_names_where_it_looked() {
    let Some(mpv) = find_mpv() else {
        eprintln!("skipped: no mpv");
        return;
    };
    if open_device().is_err() {
        return;
    }
    let host = Host::Subprocess(SubprocessOptions {
        mpv: Some(mpv),
        cplugin: Some(PathBuf::from("/nonexistent/libmpv_wgpu_cplugin.so")),
        ..SubprocessOptions::default()
    });
    let Err(Error::HostStart(text)) = player_on(host) else {
        panic!("expected HostStart");
    };
    assert!(
        text.contains("/nonexistent/libmpv_wgpu_cplugin.so"),
        "{text}"
    );
}

#[test]
fn a_program_that_is_not_mpv_is_a_start_error_not_a_hang() {
    if open_device().is_err() {
        return;
    }
    let host = Host::Subprocess(SubprocessOptions {
        mpv: Some(PathBuf::from("/bin/true")),
        ..SubprocessOptions::default()
    });
    let started = Instant::now();
    let Err(Error::HostStart(text)) = player_on(host) else {
        panic!("expected HostStart");
    };
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(text.contains("before its plugin connected"), "{text}");
}

#[test]
fn a_bad_mpv_option_is_reported_with_mpvs_own_words() {
    let Some(mpv) = find_mpv() else {
        eprintln!("skipped: no mpv");
        return;
    };
    if open_device().is_err() {
        return;
    }
    let host = Host::Subprocess(SubprocessOptions {
        mpv: Some(mpv),
        extra_args: vec!["--no-such-option-at-all".into()],
        ..SubprocessOptions::default()
    });
    let Err(Error::HostStart(text)) = player_on(host) else {
        panic!("expected HostStart");
    };
    assert!(text.contains("no-such-option"), "{text}");
}

#[test]
fn extra_arguments_reach_mpv_and_can_override_the_defaults() {
    let marker = format!("mpv-wgpu-extra-{}", std::process::id());
    let Some(host) = marked_host(&marker) else {
        return;
    };
    if open_device().is_err() {
        return;
    }
    let player = player_on(host).expect("player");
    let pid = pids_with(&marker)[0];
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).expect("cmdline");
    let text = String::from_utf8_lossy(&cmdline);
    assert!(text.contains("--no-config"), "{text}");
    assert!(text.contains("--vo=libmpv"), "{text}");
    assert!(text.contains("--ao=null"), "{text}");
    drop(player);
}

fn a_missing_file_ends_with_an_error(mode: Mode) {
    let Some(mut harness) = Harness::open(mode).map(Harness::with_slot) else {
        return;
    };
    harness
        .player
        .load("/nonexistent/definitely-missing.mkv")
        .expect("loadfile is accepted");
    harness.wait_for("an end event", |h| {
        h.events.iter().any(|e| matches!(e, Event::Ended(_)))
    });
    assert!(matches!(harness.player.picture(), Picture::Waiting));
}

fn the_raw_command_reports_mpv_errors(mode: Mode) {
    let Some(mut harness) = Harness::open(mode).map(Harness::with_slot) else {
        return;
    };
    harness.step();
    let error = harness
        .player
        .command(&["no-such-command"])
        .expect_err("unknown command");
    assert!(matches!(error, Error::Mpv(_)), "{error:?}");
    harness
        .player
        .command(&["show-text", "hi"])
        .expect("valid command");
}

in_each_mode!(
    a_missing_file_ends_with_an_error,
    the_raw_command_reports_mpv_errors
);
