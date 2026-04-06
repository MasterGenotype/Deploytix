//! Embedded theme audio with looping playback.

use std::io::Cursor;

use rodio::Source;
use tracing::warn;

/// The theme song WAV data, embedded at compile time.
static THEME_WAV: &[u8] = include_bytes!("../../theme.wav");

/// Handle that keeps audio playback alive. Playback stops when dropped.
pub struct AudioHandle {
    _stream: rodio::OutputStream,
    _sink: rodio::Sink,
}

/// Start looping playback of the embedded theme song.
///
/// Returns a handle that must be kept alive for playback to continue.
/// Returns `None` if audio initialization fails (e.g., no audio device).
pub fn play_theme_loop() -> Option<AudioHandle> {
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
    let source = match rodio::Decoder::new(cursor) {
        Ok(s) => s,
        Err(e) => {
            warn!("Could not decode theme audio: {e}");
            return None;
        }
    };

    sink.append(source.repeat_infinite());

    Some(AudioHandle {
        _stream: stream,
        _sink: sink,
    })
}
