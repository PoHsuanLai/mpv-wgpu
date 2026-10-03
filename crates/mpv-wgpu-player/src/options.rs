//! Construction-time choices for [`crate::Player`].

use std::fmt;
use std::path::PathBuf;

/// The mpv audio output driver (`--ao`).
///
/// `Auto` leaves `ao` unset so mpv probes every driver it was built with, in
/// its own priority order (PipeWire, PulseAudio, ALSA, ... on Linux; CoreAudio
/// on macOS; WASAPI on Windows). The literal string `auto` is not an `ao`
/// driver name: mpv reports `Audio output auto not found!` when playback
/// starts, and an audio-only file then ends with [`crate::EndReason::Error`].
/// The explicit variants name one driver and do not fall back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioOutput {
    /// Let mpv probe the drivers it has.
    #[default]
    Auto,
    /// PulseAudio (`pulse`).
    Pulse,
    /// PipeWire (`pipewire`).
    PipeWire,
    /// ALSA (`alsa`).
    Alsa,
    /// macOS CoreAudio (`coreaudio`).
    CoreAudio,
    /// Windows WASAPI (`wasapi`).
    Wasapi,
    /// Decode and clock the audio, play nothing (`null`).
    Null,
}

impl AudioOutput {
    /// The value for mpv's `ao` option, or `None` to leave mpv's probing in place.
    pub(crate) fn as_mpv(self) -> Option<&'static str> {
        match self {
            AudioOutput::Auto => None,
            AudioOutput::Pulse => Some("pulse"),
            AudioOutput::PipeWire => Some("pipewire"),
            AudioOutput::Alsa => Some("alsa"),
            AudioOutput::CoreAudio => Some("coreaudio"),
            AudioOutput::Wasapi => Some("wasapi"),
            AudioOutput::Null => Some("null"),
        }
    }
}

impl fmt::Display for AudioOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_mpv().unwrap_or("auto"))
    }
}

/// What a [`crate::Player`] is created with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlayerOptions {
    /// Which audio driver mpv opens.
    pub audio_output: AudioOutput,
}

/// Where the mpv core runs.
///
/// [`Host::InProcess`] links libmpv into the application: the `in-process`
/// cargo feature, on by default. [`Host::Subprocess`] runs the user's own `mpv`
/// executable as a child process with a small plugin loaded into it, and links
/// no mpv code: the `subprocess` feature. [`Host::default`] is in-process when
/// that feature is on, and subprocess otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    /// libmpv in this process.
    InProcess,
    /// The `mpv` executable as a child process.
    Subprocess(SubprocessOptions),
}

impl Default for Host {
    fn default() -> Self {
        if cfg!(feature = "in-process") {
            Host::InProcess
        } else {
            Host::Subprocess(SubprocessOptions::default())
        }
    }
}

/// How to find and start `mpv` for [`Host::Subprocess`].
///
/// The `mpv` executable comes from [`SubprocessOptions::mpv`], else the
/// `MPV_WGPU_MPV` environment variable, else `mpv` on `PATH`. The plugin
/// (`libmpv_wgpu_cplugin.so`, from the `mpv-wgpu-cplugin` crate) comes from
/// [`SubprocessOptions::cplugin`], else `MPV_WGPU_CPLUGIN`, else the directory of
/// the running executable or that directory's parent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubprocessOptions {
    /// The `mpv` executable.
    pub mpv: Option<PathBuf>,
    /// The `mpv-wgpu-cplugin` shared library.
    pub cplugin: Option<PathBuf>,
    /// Extra mpv command-line options, such as `--hwdec=no`. They come after the
    /// player's own, so they win.
    pub extra_args: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::AudioOutput;

    #[test]
    fn audio_output_maps_to_the_mpv_driver_name() {
        const CASES: &[(&str, AudioOutput, Option<&str>, &str)] = &[
            ("auto leaves mpv probing", AudioOutput::Auto, None, "auto"),
            ("pulse", AudioOutput::Pulse, Some("pulse"), "pulse"),
            (
                "pipewire",
                AudioOutput::PipeWire,
                Some("pipewire"),
                "pipewire",
            ),
            ("alsa", AudioOutput::Alsa, Some("alsa"), "alsa"),
            (
                "coreaudio",
                AudioOutput::CoreAudio,
                Some("coreaudio"),
                "coreaudio",
            ),
            ("wasapi", AudioOutput::Wasapi, Some("wasapi"), "wasapi"),
            ("null", AudioOutput::Null, Some("null"), "null"),
        ];
        for (name, output, mpv, shown) in CASES {
            assert_eq!(output.as_mpv(), *mpv, "{name}");
            assert_eq!(output.to_string(), *shown, "{name}");
        }
    }

    #[test]
    fn default_audio_output_is_auto() {
        assert_eq!(AudioOutput::default(), AudioOutput::Auto);
    }
}
