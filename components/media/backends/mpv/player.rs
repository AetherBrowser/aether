/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::{Cell, OnceCell};
use std::ops::Range;
use std::sync::{Arc, Mutex, Weak};

use log::warn;
use servo_media::MediaInstanceError;
use servo_media_player::{Player, PlayerError};
use servo_media_streams::registry::MediaStreamId;
use servo_media_traits::{ClientContextId, MediaInstance};

use crate::Instances;
use crate::handle::{MpvError, MpvHandle, Node};

const DEFAULT_PAUSED: bool = true;
const DEFAULT_MUTED: bool = false;
const DEFAULT_VOLUME: f64 = 1.0;
const DEFAULT_PLAYBACK_RATE: f64 = 1.0;

/// A media player backed by libmpv.
///
/// Its libmpv instance is only created when first needed, so a media element that never
/// plays costs no mpv threads. Operations that are not wired to libmpv yet report a
/// `PlayerError::Backend` instead of silently succeeding.
pub struct MpvPlayer {
    id: usize,
    context_id: ClientContextId,
    /// The backend's instance table. Weak so a player never keeps its backend alive.
    instances: Weak<Mutex<Instances>>,
    /// Options of the libmpv instance, read when it gets created.
    options: Arc<[(&'static str, &'static str)]>,
    handle: OnceCell<MpvHandle>,
    /// Whether this player was playing when its page got suspended, so `resume` restarts
    /// it. A media paused by the page stays paused when the page is resumed.
    resume_after_suspend: Cell<bool>,
    /// The requested mute state, applied to libmpv once its instance exists.
    muted: Cell<bool>,
    /// The requested HTML volume, applied to libmpv once its instance exists.
    volume: Cell<f64>,
}

impl MpvPlayer {
    pub(crate) fn new(
        id: usize,
        context_id: ClientContextId,
        instances: Weak<Mutex<Instances>>,
        options: Arc<[(&'static str, &'static str)]>,
    ) -> MpvPlayer {
        MpvPlayer {
            id,
            context_id,
            instances,
            options,
            handle: OnceCell::new(),
            resume_after_suspend: Cell::new(false),
            muted: Cell::new(DEFAULT_MUTED),
            volume: Cell::new(DEFAULT_VOLUME),
        }
    }

    /// The libmpv instance of this player, created on first use.
    fn handle(&self) -> Result<&MpvHandle, PlayerError> {
        if let Some(handle) = self.handle.get() {
            return Ok(handle);
        }
        let handle = MpvHandle::new(&self.options).map_err(backend_error)?;
        if self.muted.get() != DEFAULT_MUTED {
            handle
                .set_flag("mute", self.muted.get())
                .map_err(backend_error)?;
        }
        if self.volume.get() != DEFAULT_VOLUME {
            handle
                .set_double("volume", html_to_mpv_volume(self.volume.get()))
                .map_err(backend_error)?;
        }
        Ok(self.handle.get_or_init(|| handle))
    }
}

/// mpv applies a cubic curve to its `volume` property (gain = (volume / 100)³), while an
/// HTML volume is a linear gain in [0, 1]. Taking the cube root makes the two match.
fn html_to_mpv_volume(volume: f64) -> f64 {
    100.0 * volume.clamp(0.0, 1.0).cbrt()
}

fn mpv_to_html_volume(volume: f64) -> f64 {
    (volume / 100.0).powi(3)
}

fn backend_error(error: MpvError) -> PlayerError {
    PlayerError::Backend(format!("MPV backend: {error}"))
}

fn not_implemented(operation: &str) -> PlayerError {
    PlayerError::Backend(format!("MPV backend: {operation} is not implemented yet"))
}

impl Player for MpvPlayer {
    fn play(&self) -> Result<(), PlayerError> {
        self.handle()?
            .set_flag("pause", false)
            .map_err(backend_error)?;
        self.resume_after_suspend.set(false);
        Ok(())
    }

    fn pause(&self) -> Result<(), PlayerError> {
        self.handle()?
            .set_flag("pause", true)
            .map_err(backend_error)?;
        self.resume_after_suspend.set(false);
        Ok(())
    }

    fn paused(&self) -> bool {
        // Without a libmpv instance nothing ever played: don't start one just to answer.
        let Some(handle) = self.handle.get() else {
            return DEFAULT_PAUSED;
        };
        handle.get_flag("pause").unwrap_or_else(|error| {
            warn!("MPV backend: cannot read the pause state: {error}");
            DEFAULT_PAUSED
        })
    }

    fn can_resume(&self) -> bool {
        self.resume_after_suspend.get()
    }

    fn stop(&self) -> Result<(), PlayerError> {
        // Without a libmpv instance nothing ever played: there is nothing to stop.
        let Some(handle) = self.handle.get() else {
            return Ok(());
        };
        handle.set_flag("pause", true).map_err(backend_error)?;
        self.resume_after_suspend.set(false);
        // Seeking needs a loaded file. When mpv is idle, pausing is all there is to do.
        if !handle.get_flag("idle-active").map_err(backend_error)? {
            handle
                .command(&["seek", "0", "absolute"])
                .map_err(backend_error)?;
        }
        Ok(())
    }

    fn seek(&self, _time: f64) -> Result<(), PlayerError> {
        Err(not_implemented("seek"))
    }

    fn seekable(&self) -> Vec<Range<f64>> {
        vec![]
    }

    fn set_mute(&self, muted: bool) -> Result<(), PlayerError> {
        if let Some(handle) = self.handle.get() {
            handle.set_flag("mute", muted).map_err(backend_error)?;
        }
        self.muted.set(muted);
        Ok(())
    }

    fn muted(&self) -> bool {
        match self.handle.get().map(|handle| handle.get_node("mute")) {
            Some(Ok(Node::Flag(muted))) => muted,
            _ => self.muted.get(),
        }
    }

    fn set_volume(&self, volume: f64) -> Result<(), PlayerError> {
        if let Some(handle) = self.handle.get() {
            handle
                .set_double("volume", html_to_mpv_volume(volume))
                .map_err(backend_error)?;
        }
        self.volume.set(volume.clamp(0.0, 1.0));
        Ok(())
    }

    fn volume(&self) -> f64 {
        match self.handle.get().map(|handle| handle.get_node("volume")) {
            Some(Ok(Node::Double(volume))) => mpv_to_html_volume(volume),
            _ => self.volume.get(),
        }
    }

    fn set_input_size(&self, _size: u64) -> Result<(), PlayerError> {
        Err(not_implemented("set_input_size"))
    }

    fn set_seekable(&self, _seekable: bool) -> Result<(), PlayerError> {
        Err(not_implemented("set_seekable"))
    }

    fn set_download_buffering_enabled(&self, _enabled: bool) -> Result<(), PlayerError> {
        // mpv manages its own demuxer cache, there is no download buffering to toggle.
        Ok(())
    }

    fn set_playback_rate(&self, _playback_rate: f64) -> Result<(), PlayerError> {
        Err(not_implemented("set_playback_rate"))
    }

    fn playback_rate(&self) -> f64 {
        DEFAULT_PLAYBACK_RATE
    }

    fn push_data(&self, _data: Vec<u8>) -> Result<(), PlayerError> {
        Err(not_implemented("push_data"))
    }

    fn end_of_stream(&self) -> Result<(), PlayerError> {
        Err(not_implemented("end_of_stream"))
    }

    fn buffered(&self) -> Vec<Range<f64>> {
        vec![]
    }

    fn set_stream(&self, _stream: &MediaStreamId, _only_stream: bool) -> Result<(), PlayerError> {
        // libmpv cannot consume servo-media streams (getUserMedia, WebRTC).
        Err(PlayerError::SetStreamFailed)
    }

    fn render_use_gl(&self) -> bool {
        false
    }

    fn set_audio_track(&self, _stream_index: i32, _enabled: bool) -> Result<(), PlayerError> {
        Err(not_implemented("set_audio_track"))
    }

    fn set_video_track(&self, _stream_index: i32, _enabled: bool) -> Result<(), PlayerError> {
        Err(not_implemented("set_video_track"))
    }
}

impl MediaInstance for MpvPlayer {
    fn get_id(&self) -> usize {
        self.id
    }

    fn mute(&self, val: bool) -> Result<(), MediaInstanceError> {
        self.set_mute(val).map_err(|_| MediaInstanceError)
    }

    fn suspend(&self) -> Result<(), MediaInstanceError> {
        if self.paused() {
            return Ok(());
        }
        self.pause().map_err(|_| MediaInstanceError)?;
        self.resume_after_suspend.set(true);
        Ok(())
    }

    fn resume(&self) -> Result<(), MediaInstanceError> {
        if !self.can_resume() {
            return Ok(());
        }
        self.play().map_err(|_| MediaInstanceError)
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        // The backend never holds the table lock while a player can be dropped (see
        // `MpvBackend::live_instances`), so this cannot deadlock.
        let Some(instances) = self.instances.upgrade() else {
            return;
        };
        let Ok(mut instances) = instances.lock() else {
            return;
        };
        if let Some(players) = instances.get_mut(&self.context_id) {
            players.retain(|(id, _)| *id != self.id);
            if players.is_empty() {
                instances.remove(&self.context_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    /// A player whose libmpv instance never opens an audio output.
    fn player_with(options: &[(&'static str, &'static str)]) -> MpvPlayer {
        MpvPlayer::new(
            0,
            ClientContextId::build(1, 1),
            Weak::new(),
            Arc::from(options),
        )
    }

    /// A player with the real player options, without an audio output.
    fn player() -> MpvPlayer {
        let options: Vec<_> = crate::BASE_OPTIONS
            .iter()
            .chain(crate::PLAYER_OPTIONS)
            .chain(&[("ao", "null")])
            .copied()
            .collect();
        player_with(&options)
    }

    #[test]
    fn libmpv_is_only_started_when_needed() {
        let player = player();
        assert!(player.handle.get().is_none());
        player.play().unwrap();
        assert!(player.handle.get().is_some());
    }

    #[test]
    fn new_players_start_paused() {
        let player = player();
        let handle = player.handle().unwrap();
        assert_eq!(handle.get_node("pause"), Ok(Node::Flag(true)));
    }

    #[test]
    fn play_unpauses_libmpv() {
        let player = player();
        player.play().unwrap();
        let handle = player.handle.get().unwrap();
        assert_eq!(handle.get_node("pause"), Ok(Node::Flag(false)));
    }

    #[test]
    fn pause_pauses_libmpv() {
        let player = player();
        player.play().unwrap();
        assert!(!player.paused());
        player.pause().unwrap();
        assert!(player.paused());
        let handle = player.handle.get().unwrap();
        assert_eq!(handle.get_node("pause"), Ok(Node::Flag(true)));
    }

    #[test]
    fn paused_does_not_start_libmpv() {
        let player = player();
        assert!(player.paused());
        assert!(player.handle.get().is_none());
    }

    #[test]
    fn suspend_then_resume_restarts_a_playing_media() {
        let player = player();
        player.play().unwrap();
        player.suspend().unwrap();
        assert!(player.paused());
        assert!(player.can_resume());
        player.resume().unwrap();
        assert!(!player.paused());
        assert!(!player.can_resume());
    }

    #[test]
    fn resume_keeps_a_media_paused_by_the_page_paused() {
        let player = player();
        player.play().unwrap();
        player.pause().unwrap();
        player.suspend().unwrap();
        assert!(!player.can_resume());
        player.resume().unwrap();
        assert!(player.paused());
    }

    #[test]
    fn a_page_action_after_suspend_cancels_the_resume() {
        let player = player();
        player.play().unwrap();
        player.suspend().unwrap();
        player.pause().unwrap();
        player.resume().unwrap();
        assert!(player.paused());
    }

    #[test]
    fn suspend_and_resume_do_not_start_libmpv() {
        let player = player();
        player.suspend().unwrap();
        player.resume().unwrap();
        assert!(player.handle.get().is_none());
    }

    /// Waits until `condition` holds, failing the test after a few seconds.
    fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn time_pos(handle: &MpvHandle) -> Option<f64> {
        match handle.get_node("time-pos") {
            Ok(Node::Double(seconds)) => Some(seconds),
            _ => None,
        }
    }

    /// Loads a file directly, as the stream bridge will once it exists.
    fn load_test_file(player: &MpvPlayer) -> &MpvHandle {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../tests/wpt/tests/media/sine440.mp3");
        let handle = player.handle().unwrap();
        handle
            .command(&["loadfile", path.to_str().unwrap()])
            .unwrap();
        wait_until("the file to load", || handle.get_node("duration").is_ok());
        handle
    }

    #[test]
    fn stop_without_libmpv_does_not_start_it() {
        let player = player();
        player.stop().unwrap();
        assert!(player.handle.get().is_none());
    }

    #[test]
    fn stop_without_a_file_only_pauses() {
        let player = player();
        player.play().unwrap();
        player.stop().unwrap();
        assert!(player.paused());
    }

    #[test]
    fn stop_pauses_and_rewinds_a_loaded_file() {
        let player = player();
        let handle = load_test_file(&player);
        player.play().unwrap();
        wait_until("playback to advance", || {
            time_pos(handle).is_some_and(|seconds| seconds > 0.2)
        });

        player.stop().unwrap();
        assert!(player.paused());
        assert!(!player.can_resume());
        wait_until("the rewind", || {
            time_pos(handle).is_some_and(|seconds| seconds < 0.05)
        });
    }

    #[test]
    fn muting_does_not_start_libmpv() {
        let player = player();
        player.set_mute(true).unwrap();
        assert!(player.handle.get().is_none());
        assert!(player.muted());
    }

    #[test]
    fn mute_requested_before_start_is_applied_to_libmpv() {
        let player = player();
        player.set_mute(true).unwrap();
        player.play().unwrap();
        let handle = player.handle.get().unwrap();
        assert_eq!(handle.get_node("mute"), Ok(Node::Flag(true)));
    }

    #[test]
    fn set_mute_drives_the_libmpv_mute_property() {
        let player = player();
        player.play().unwrap();
        let handle = player.handle.get().unwrap();

        player.set_mute(true).unwrap();
        assert_eq!(handle.get_node("mute"), Ok(Node::Flag(true)));
        assert!(player.muted());

        player.set_mute(false).unwrap();
        assert_eq!(handle.get_node("mute"), Ok(Node::Flag(false)));
        assert!(!player.muted());
    }

    #[test]
    fn html_volume_is_converted_to_mpv_cubic_volume() {
        assert_eq!(html_to_mpv_volume(0.0), 0.0);
        assert_eq!(html_to_mpv_volume(1.0), 100.0);
        assert_eq!(html_to_mpv_volume(0.125), 50.0);
        assert_eq!(mpv_to_html_volume(50.0), 0.125);
        assert_eq!(html_to_mpv_volume(-1.0), 0.0);
        assert_eq!(html_to_mpv_volume(2.0), 100.0);
    }

    #[test]
    fn volume_change_does_not_start_libmpv() {
        let player = player();
        player.set_volume(0.125).unwrap();
        assert!(player.handle.get().is_none());
        assert_eq!(player.volume(), 0.125);
    }

    #[test]
    fn volume_requested_before_start_is_applied_to_libmpv() {
        let player = player();
        player.set_volume(0.125).unwrap();
        player.play().unwrap();
        let handle = player.handle.get().unwrap();
        assert_eq!(handle.get_node("volume"), Ok(Node::Double(50.0)));
    }

    #[test]
    fn set_volume_drives_the_libmpv_volume_property() {
        let player = player();
        player.play().unwrap();
        let handle = player.handle.get().unwrap();

        player.set_volume(0.125).unwrap();
        assert_eq!(handle.get_node("volume"), Ok(Node::Double(50.0)));
        assert_eq!(player.volume(), 0.125);

        player.set_volume(0.0).unwrap();
        assert_eq!(handle.get_node("volume"), Ok(Node::Double(0.0)));
        assert_eq!(player.volume(), 0.0);
    }

    #[test]
    fn libmpv_failures_are_reported_as_backend_errors() {
        let player = player_with(&[("not-an-option", "1")]);
        let Err(PlayerError::Backend(message)) = player.play() else {
            panic!("play() should fail when libmpv cannot be created");
        };
        assert!(message.starts_with("MPV backend: mpv_set_option_string(not-an-option=1)"));
    }
}
