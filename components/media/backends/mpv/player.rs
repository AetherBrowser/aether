/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::ops::Range;
use std::sync::{Mutex, Weak};

use servo_media::MediaInstanceError;
use servo_media_player::{Player, PlayerError};
use servo_media_streams::registry::MediaStreamId;
use servo_media_traits::{ClientContextId, MediaInstance};

use crate::Instances;

const DEFAULT_PAUSED: bool = true;
const DEFAULT_MUTED: bool = false;
const DEFAULT_VOLUME: f64 = 1.0;
const DEFAULT_PLAYBACK_RATE: f64 = 1.0;

/// A media player backed by libmpv.
///
/// This is the skeleton of the player: it is registered with its backend and implements
/// the `Player` contract, but playback commands, events and the data path are not wired
/// to libmpv yet, so they report a `PlayerError::Backend` instead of silently succeeding.
pub struct MpvPlayer {
    id: usize,
    context_id: ClientContextId,
    /// The backend's instance table. Weak so a player never keeps its backend alive.
    instances: Weak<Mutex<Instances>>,
}

impl MpvPlayer {
    pub(crate) fn new(
        id: usize,
        context_id: ClientContextId,
        instances: Weak<Mutex<Instances>>,
    ) -> MpvPlayer {
        MpvPlayer {
            id,
            context_id,
            instances,
        }
    }
}

fn not_implemented(operation: &str) -> PlayerError {
    PlayerError::Backend(format!("MPV backend: {operation} is not implemented yet"))
}

impl Player for MpvPlayer {
    fn play(&self) -> Result<(), PlayerError> {
        Err(not_implemented("play"))
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

    fn set_mute(&self, _muted: bool) -> Result<(), PlayerError> {
        Err(not_implemented("set_mute"))
    }

    fn muted(&self) -> bool {
        DEFAULT_MUTED
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
