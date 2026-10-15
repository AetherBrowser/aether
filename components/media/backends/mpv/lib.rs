/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A servo-media backend that plays `<audio>` and `<video>` with libmpv.
//!
//! libmpv only plays media: everything else the `Backend` trait covers (Web Audio, WebRTC,
//! capture, media streams) is delegated to a fallback backend.

use std::sync::{Arc, Mutex};

use servo_base::generic_channel::GenericCallback;
use servo_media::{Backend, BackendDeInit, BackendInit, SupportsMediaType};
use servo_media_audio::context::{AudioContext, AudioContextOptions};
use servo_media_audio::sink::AudioSinkError;
use servo_media_dummy::DummyBackend;
use servo_media_player::audio::AudioRenderer;
use servo_media_player::context::PlayerGLContext;
use servo_media_player::video::VideoFrameRenderer;
use servo_media_player::{Player, PlayerEvent, StreamType};
use servo_media_streams::capture::MediaTrackConstraintSet;
use servo_media_streams::device_monitor::MediaDeviceMonitor;
use servo_media_streams::registry::MediaStreamId;
use servo_media_streams::{MediaOutput, MediaSocket, MediaStreamType};
use servo_media_traits::ClientContextId;
use servo_media_webrtc::{WebRtcController, WebRtcSignaller};

pub struct MpvBackend {
    /// Handles everything libmpv cannot do.
    fallback: Box<dyn Backend>,
}

impl MpvBackend {
    /// Creates the backend, delegating non-playback features to `fallback`.
    pub fn with_fallback(fallback: Box<dyn Backend>) -> MpvBackend {
        MpvBackend { fallback }
    }
}

impl BackendInit for MpvBackend {
    fn init() -> Box<dyn Backend> {
        Box::new(MpvBackend::with_fallback(DummyBackend::init()))
    }
}

/// Nothing to release yet.
impl BackendDeInit for MpvBackend {}

impl Backend for MpvBackend {
    fn create_player(
        &self,
        id: &ClientContextId,
        stream_type: StreamType,
        sender: GenericCallback<PlayerEvent>,
        video_renderer: Option<Arc<Mutex<dyn VideoFrameRenderer>>>,
        audio_renderer: Option<Arc<Mutex<dyn AudioRenderer>>>,
        gl_context: Box<dyn PlayerGLContext>,
    ) -> Arc<Mutex<dyn Player>> {
        // Replaced by an mpv player once it exists.
        self.fallback.create_player(
            id,
            stream_type,
            sender,
            video_renderer,
            audio_renderer,
            gl_context,
        )
    }

    fn can_play_type(&self, _media_type: &str) -> SupportsMediaType {
        // libmpv is not used yet, so nothing can be played.
        SupportsMediaType::No
    }

    fn mute(&self, id: &ClientContextId, val: bool) {
        self.fallback.mute(id, val);
    }

    fn suspend(&self, id: &ClientContextId) {
        self.fallback.suspend(id);
    }

    fn resume(&self, id: &ClientContextId) {
        self.fallback.resume(id);
    }

    fn create_audiostream(&self) -> MediaStreamId {
        self.fallback.create_audiostream()
    }

    fn create_videostream(&self) -> MediaStreamId {
        self.fallback.create_videostream()
    }

    fn create_stream_output(&self) -> Box<dyn MediaOutput> {
        self.fallback.create_stream_output()
    }

    fn create_stream_and_socket(
        &self,
        ty: MediaStreamType,
    ) -> (Box<dyn MediaSocket>, MediaStreamId) {
        self.fallback.create_stream_and_socket(ty)
    }

    fn create_audioinput_stream(&self, set: MediaTrackConstraintSet) -> Option<MediaStreamId> {
        self.fallback.create_audioinput_stream(set)
    }

    fn create_videoinput_stream(&self, set: MediaTrackConstraintSet) -> Option<MediaStreamId> {
        self.fallback.create_videoinput_stream(set)
    }

    fn create_audio_context(
        &self,
        id: &ClientContextId,
        options: AudioContextOptions,
    ) -> Result<Arc<Mutex<AudioContext>>, AudioSinkError> {
        self.fallback.create_audio_context(id, options)
    }

    fn create_webrtc(&self, signaller: Box<dyn WebRtcSignaller>) -> WebRtcController {
        self.fallback.create_webrtc(signaller)
    }

    fn set_capture_mocking(&self, mock: bool) {
        self.fallback.set_capture_mocking(mock)
    }

    fn get_device_monitor(&self) -> Box<dyn MediaDeviceMonitor> {
        self.fallback.get_device_monitor()
    }
}
