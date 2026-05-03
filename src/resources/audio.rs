//! Embedded theme audio with looping playback.

use std::io::Cursor;

use tracing::warn;

/// The theme song WAV data, embedded at compile time.
static THEME_WAV: &[u8] = include_bytes!("../../theme.wav");

/// Handle that keeps audio playback alive. Playback stops when dropped.
pub struct AudioHandle {
    _stream: rodio::OutputStream,
    _sink: rodio::Sink,
}

/// Install a no-op ALSA error handler to suppress PCM underrun spam.
///
/// During heavy disk I/O the audio buffer starves, causing ALSA's default
/// handler to print "underrun occurred" to stderr on every glitch. A no-op
/// handler silences the output without changing ALSA's recovery behaviour.
/// libasound is already in the link path via rodio → cpal → alsa-sys.
#[cfg(target_os = "linux")]
fn suppress_alsa_errors() {
    use std::ffi::{c_char, c_int};

    type AlsaHandler =
        unsafe extern "C" fn(*const c_char, c_int, *const c_char, c_int, *const c_char);

    unsafe extern "C" fn noop(
        _: *const c_char,
        _: c_int,
        _: *const c_char,
        _: c_int,
        _: *const c_char,
    ) {
    }

    extern "C" {
        fn snd_lib_error_set_handler(handler: Option<AlsaHandler>);
    }

    unsafe { snd_lib_error_set_handler(Some(noop)) };
}

#[cfg(not(target_os = "linux"))]
fn suppress_alsa_errors() {}

/// Ensure the audio server is reachable when running as root via sudo/pkexec.
///
/// PipeWire and PulseAudio live in the invoking user's XDG_RUNTIME_DIR.
/// `sudo` strips that variable, so ALSA→PipeWire routing silently fails.
/// We restore it from SUDO_UID / PKEXEC_UID when running as root.
fn ensure_audio_env() {
    use nix::unistd::Uid;

    if !Uid::effective().is_root() {
        return;
    }

    if std::env::var_os("XDG_RUNTIME_DIR").is_some() {
        return;
    }

    let real_uid = std::env::var("SUDO_UID")
        .or_else(|_| std::env::var("PKEXEC_UID"))
        .ok();

    if let Some(uid) = real_uid {
        let runtime_dir = format!("/run/user/{uid}");
        if std::path::Path::new(&runtime_dir).is_dir() {
            std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
            tracing::info!("Set XDG_RUNTIME_DIR={runtime_dir} for audio routing");
        }
    }
}

/// Start looping playback of the embedded theme song.
///
/// Returns a handle that must be kept alive for playback to continue.
/// Returns `None` if audio initialization fails (e.g., no audio device).
pub fn play_theme_loop() -> Option<AudioHandle> {
    suppress_alsa_errors();
    ensure_audio_env();

    let (stream, stream_handle) = match rodio::OutputStream::try_default() {
        Ok(s) => s,
        Err(e) => {
            warn!("Could not open audio device: {e}");
            return None;
        }
    };

    let sink = match rodio::Sink::try_new(&stream_handle) {
        Ok(s) => s,
        Err(e) => {
            warn!("Could not create audio sink: {e}");
            return None;
        }
    };

    let cursor = Cursor::new(THEME_WAV);
    let source = match rodio::Decoder::new_looped(cursor) {
        Ok(s) => s,
        Err(e) => {
            warn!("Could not decode theme audio: {e}");
            return None;
        }
    };

    sink.append(source);

    Some(AudioHandle {
        _stream: stream,
        _sink: sink,
    })
}
