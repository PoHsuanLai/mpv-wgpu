//! Finding, starting and greeting the `mpv` child process.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mpv_wgpu_protocol::{Channel, Hello, Listener, Message, PROTOCOL_VERSION};

use crate::notify::lock;
use crate::options::{PlayerOptions, SubprocessOptions};
use crate::types::Error;

/// Environment variable naming the `mpv` executable.
pub(crate) const MPV_ENV: &str = "MPV_WGPU_MPV";
/// Environment variable naming the plugin library.
pub(crate) const CPLUGIN_ENV: &str = "MPV_WGPU_CPLUGIN";
/// The environment variable the plugin reads the socket name from.
const SOCKET_ENV: &str = "MPV_WGPU_SOCKET";
/// File name of the plugin library `mpv-wgpu-cplugin` builds.
const CPLUGIN_FILE: &str = "libmpv_wgpu_cplugin.so";
/// Client API major version that has the software render API.
const MIN_CLIENT_API_MAJOR: u32 = 2;
/// How long the plugin gets to connect after mpv starts.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// A started child with its stderr being logged.
pub(super) struct Spawned {
    pub(super) child: Child,
    pub(super) channel: Channel,
    pub(super) hello: Hello,
    /// Threads logging the child's output; they end when it exits.
    pub(super) logs: Vec<JoinHandle<()>>,
}

/// The `mpv` to run: the option, then `MPV_WGPU_MPV`, then `mpv` from `PATH`.
pub(crate) fn mpv_program(options: &SubprocessOptions) -> OsString {
    if let Some(path) = &options.mpv {
        return path.clone().into_os_string();
    }
    match std::env::var_os(MPV_ENV) {
        Some(path) if !path.is_empty() => path,
        _ => OsString::from("mpv"),
    }
}

/// The plugin library: the option, then `MPV_WGPU_CPLUGIN`, then next to the
/// running executable or in its parent directory.
pub(crate) fn cplugin_path(options: &SubprocessOptions) -> Result<PathBuf, Error> {
    let mut tried = Vec::new();
    if let Some(path) = &options.cplugin {
        return existing(path.clone()).map_err(|path| not_found(&[path]));
    }
    if let Some(path) = std::env::var_os(CPLUGIN_ENV).filter(|p| !p.is_empty()) {
        return existing(PathBuf::from(path)).map_err(|path| not_found(&[path]));
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent();
        for _ in 0..2 {
            let Some(current) = dir else { break };
            let candidate = current.join(CPLUGIN_FILE);
            match existing(candidate) {
                Ok(found) => return Ok(found),
                Err(missing) => tried.push(missing),
            }
            dir = current.parent();
        }
    }
    Err(not_found(&tried))
}

fn existing(path: PathBuf) -> Result<PathBuf, PathBuf> {
    if path.is_file() { Ok(path) } else { Err(path) }
}

fn not_found(tried: &[PathBuf]) -> Error {
    let list: Vec<String> = tried.iter().map(|p| p.display().to_string()).collect();
    Error::HostStart(format!(
        "{CPLUGIN_FILE} (the mpv-wgpu-cplugin library) not found; looked at [{}]. \
         Set {CPLUGIN_ENV} or SubprocessOptions::cplugin",
        list.join(", ")
    ))
}

/// Start `mpv` with the plugin, wait for it to connect, and read its hello.
pub(super) fn spawn(player: PlayerOptions, options: &SubprocessOptions) -> Result<Spawned, Error> {
    let plugin = cplugin_path(options)?;
    let program = mpv_program(options);
    let listener = Listener::bind_random().map_err(start_error("cannot open the plugin socket"))?;

    let mut command = Command::new(&program);
    command
        .args(mpv_args(player, &plugin, options))
        .env(SOCKET_ENV, listener.name())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        Error::HostStart(format!(
            "cannot run {}: {error}. Install mpv, or set {MPV_ENV} or SubprocessOptions::mpv",
            Path::new(&program).display()
        ))
    })?;

    // mpv prints option errors on stdout and the rest on stderr: log both.
    let tail = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut logs = Vec::new();
    let pipes: [Option<Box<dyn Read + Send>>; 2] = [
        child.stdout.take().map(|p| Box::new(p) as _),
        child.stderr.take().map(|p| Box::new(p) as _),
    ];
    for pipe in pipes.into_iter().flatten() {
        let tail = Arc::clone(&tail);
        match thread::Builder::new()
            .name("mpv-wgpu-log".into())
            .spawn(move || log_output(pipe, &tail))
        {
            Ok(handle) => logs.push(handle),
            Err(error) => {
                kill(&mut child);
                return Err(Error::HostStart(format!("cannot start a thread: {error}")));
            }
        }
    }

    let channel = match accept(&listener, &mut child) {
        Ok(channel) => channel,
        Err(reason) => {
            kill(&mut child);
            for handle in logs {
                let _ = handle.join();
            }
            let tail = lock(&tail).join(" | ");
            let detail = if tail.is_empty() {
                String::new()
            } else {
                format!(" (mpv said: {tail})")
            };
            return Err(Error::HostStart(format!("{reason}{detail}")));
        }
    };
    drop(listener);

    let hello = match read_hello(&channel) {
        Ok(hello) => hello,
        Err(error) => {
            kill(&mut child);
            return Err(error);
        }
    };
    Ok(Spawned {
        child,
        channel,
        hello,
        logs,
    })
}

fn start_error(what: &'static str) -> impl Fn(std::io::Error) -> Error {
    move |error| Error::HostStart(format!("{what}: {error}"))
}

/// The command line: the player's own options, then the caller's extras.
pub(crate) fn mpv_args(
    player: PlayerOptions,
    plugin: &Path,
    options: &SubprocessOptions,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--no-config",
        "--load-scripts=no",
        "--idle=yes",
        "--vo=libmpv",
        "--video-timing-offset=0",
        "--msg-level=all=error",
        "--input-terminal=no",
        "--input-default-bindings=no",
        "--input-vo-keyboard=no",
        "--osc=no",
        "--hwdec=auto-safe",
        "--vid=auto",
        "--keep-open=yes",
        "--video-sync=audio",
        "--sub-visibility=yes",
        "--volume-max=150",
        "--audio-display=embedded-first",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    if let Some(driver) = player.audio_output.as_mpv() {
        args.push(format!("--ao={driver}").into());
    }
    let mut script = OsString::from("--script=");
    script.push(plugin);
    args.push(script);
    args.extend(options.extra_args.iter().map(OsString::from));
    args
}

fn accept(listener: &Listener, child: &mut Child) -> Result<Channel, String> {
    let started = Instant::now();
    loop {
        match listener.accept_timeout(Duration::from_millis(50)) {
            Ok(Some(channel)) => return Ok(channel),
            Ok(None) => {}
            Err(error) => return Err(format!("the plugin socket failed: {error}")),
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!(
                    "mpv exited ({status}) before its plugin connected; is the mpv built with \
                     C plugin support (-Dcplugins=enabled)?"
                ));
            }
            Ok(None) => {}
            Err(error) => return Err(format!("cannot check on mpv: {error}")),
        }
        if started.elapsed() > CONNECT_TIMEOUT {
            return Err("mpv's plugin did not connect in time".to_string());
        }
    }
}

fn read_hello(channel: &Channel) -> Result<Hello, Error> {
    let mut scratch = Vec::new();
    let message = channel
        .recv(&mut scratch)
        .map_err(|error| Error::HostStart(format!("no hello from the plugin: {error}")))?;
    let Some((Message::Hello(hello), _)) = message else {
        return Err(Error::HostStart(
            "the plugin did not start with a hello".to_string(),
        ));
    };
    check_hello(&hello)?;
    Ok(hello)
}

/// Refuse a plugin this player cannot talk to, or an mpv that lacks the render API.
pub(crate) fn check_hello(hello: &Hello) -> Result<(), Error> {
    if hello.protocol != PROTOCOL_VERSION {
        return Err(Error::HostStart(format!(
            "the mpv plugin speaks protocol {} and this player speaks {PROTOCOL_VERSION}; \
             use matching mpv-wgpu-player and mpv-wgpu-cplugin versions (plugin {})",
            hello.protocol, hello.plugin_version
        )));
    }
    if hello.client_api >> 16 < MIN_CLIENT_API_MAJOR {
        return Err(Error::HostStart(format!(
            "{} is too old: its client API is {}.{} and the software render API needs {}.0",
            hello.mpv_version,
            hello.client_api >> 16,
            hello.client_api & 0xffff,
            MIN_CLIENT_API_MAJOR
        )));
    }
    Ok(())
}

fn log_output(pipe: Box<dyn Read + Send>, tail: &Mutex<Vec<String>>) {
    for line in BufReader::new(pipe).lines() {
        let Ok(line) = line else { break };
        let line = line.trim_end().to_string();
        if line.is_empty() {
            continue;
        }
        log::warn!("mpv: {line}");
        let mut tail = lock(tail);
        if tail.len() >= 8 {
            tail.remove(0);
        }
        tail.push(line);
    }
}

pub(super) fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::AudioOutput;

    fn hello(protocol: u16, api: u32) -> Hello {
        Hello {
            protocol,
            client_api: api,
            mpv_version: "mpv v0.35.0".into(),
            plugin_version: "0.1.0".into(),
        }
    }

    #[test]
    fn the_handshake_checks_protocol_and_client_api() {
        assert!(check_hello(&hello(PROTOCOL_VERSION, 0x0002_0000)).is_ok());
        assert!(check_hello(&hello(PROTOCOL_VERSION, 0x0002_0005)).is_ok());
        assert!(matches!(
            check_hello(&hello(PROTOCOL_VERSION + 1, 0x0002_0000)),
            Err(Error::HostStart(text)) if text.contains("protocol")
        ));
        assert!(matches!(
            check_hello(&hello(PROTOCOL_VERSION, 0x0001_0069)),
            Err(Error::HostStart(text)) if text.contains("too old")
        ));
    }

    #[test]
    fn the_command_line_carries_the_player_options_then_the_extras() {
        let options = SubprocessOptions {
            extra_args: vec!["--hwdec=no".into()],
            ..SubprocessOptions::default()
        };
        let args: Vec<String> = mpv_args(
            PlayerOptions {
                audio_output: AudioOutput::Null,
            },
            Path::new("/x/libmpv_wgpu_cplugin.so"),
            &options,
        )
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
        for needed in [
            "--no-config",
            "--vo=libmpv",
            "--idle=yes",
            "--video-timing-offset=0",
            "--keep-open=yes",
            "--hwdec=auto-safe",
            "--ao=null",
            "--script=/x/libmpv_wgpu_cplugin.so",
        ] {
            assert!(args.iter().any(|a| a == needed), "{needed} in {args:?}");
        }
        assert_eq!(args.last().map(String::as_str), Some("--hwdec=no"));
        let auto = mpv_args(
            PlayerOptions::default(),
            Path::new("/p.so"),
            &SubprocessOptions::default(),
        );
        assert!(
            !auto
                .iter()
                .any(|a| a.to_string_lossy().starts_with("--ao="))
        );
    }

    #[test]
    fn an_explicit_missing_plugin_is_named() {
        let options = SubprocessOptions {
            cplugin: Some(PathBuf::from("/nonexistent/libmpv_wgpu_cplugin.so")),
            ..SubprocessOptions::default()
        };
        assert!(matches!(
            cplugin_path(&options),
            Err(Error::HostStart(text)) if text.contains("/nonexistent/")
        ));
    }
}
