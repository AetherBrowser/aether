/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::{Cell, OnceCell};
use std::ops::Range;
use std::sync::{Arc, Mutex, Weak};

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
    /// The requested mute state, applied to libmpv once its instance exists.
    muted: Cell<bool>,
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
            muted: Cell::new(DEFAULT_MUTED),
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
        Ok(self.handle.get_or_init(|| handle))
    }
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
            .map_err(backend_error)
    }

    fn pause(&self) -> Result<(), PlayerError> {
        Err(not_implemented("pause"))
    }

    fn paused(&self) -> bool {
        DEFAULT_PAUSED
    }

    fn can_resume(&self) -> bool {
        false
    }

    fn stop(&self) -> Result<(), PlayerError> {
        Err(not_implemented("stop"))
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

    fn set_volume(&self, _volume: f64) -> Result<(), PlayerError> {
        Err(not_implemented("set_volume"))
    }

    fn volume(&self) -> f64 {
        DEFAULT_VOLUME
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
        self.pause().map_err(|_| MediaInstanceError)
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
    fn libmpv_failures_are_reported_as_backend_errors() {
        let player = player_with(&[("not-an-option", "1")]);
        let Err(PlayerError::Backend(message)) = player.play() else {
            panic!("play() should fail when libmpv cannot be created");
        };
        assert!(message.starts_with("MPV backend: mpv_set_option_string(not-an-option=1)"));
    }
}
