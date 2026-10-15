/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A servo-media backend that plays `<audio>` and `<video>` with libmpv.
//!
//! libmpv only plays media: everything else the `Backend` trait covers (Web Audio, WebRTC,
//! capture, media streams) is delegated to a fallback backend.

mod ffi;
mod handle;
mod player;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use handle::{MpvError, MpvHandle};
use log::{error, warn};
pub use player::MpvPlayer;
use servo_base::generic_channel::GenericCallback;
use servo_media::{Backend, BackendDeInit, BackendInit, MediaInstanceError, SupportsMediaType};
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
use servo_media_traits::{ClientContextId, MediaInstance};
use servo_media_webrtc::{WebRtcController, WebRtcSignaller};

/// Options applied to every libmpv instance, on top of the embedding defaults of the
/// `libmpv` profile (no config files, no terminal, no OSC, no input bindings).
/// They turn off command line player features a browser never uses.
const BASE_OPTIONS: &[(&str, &str)] = &[
    ("load-scripts", "no"),
    ("ytdl", "no"),
    ("load-auto-profiles", "no"),
    ("audio-display", "no"),
];

/// Media instances per client context (a pipeline), weakly referenced.
type Instances = HashMap<ClientContextId, Vec<(usize, Weak<Mutex<dyn MediaInstance>>)>>;

pub struct MpvBackend {
    /// Handles everything libmpv cannot do.
    fallback: Box<dyn Backend>,
    instances: Arc<Mutex<Instances>>,
    next_instance_id: AtomicUsize,
}

impl MpvBackend {
    /// Creates the backend, delegating non-playback features to `fallback`.
    ///
    /// This checks that libmpv is usable, so it should run off the main thread, as
    /// `ServoMedia::init` does.
    pub fn with_fallback(fallback: Box<dyn Backend>) -> MpvBackend {
        if let Err(error) = check_libmpv() {
            error!("MPV backend: cannot use libmpv, media playback is disabled: {error}");
        }
        MpvBackend {
            fallback,
            instances: Default::default(),
            next_instance_id: AtomicUsize::new(0),
        }
    }

    /// The live media instances of a client context, pruning dead ones.
    ///
    /// The table lock is released before returning, so callers can act on the instances,
    /// and possibly drop the last reference to one, without deadlocking in its `Drop`.
    fn live_instances(&self, id: &ClientContextId) -> Vec<Arc<Mutex<dyn MediaInstance>>> {
        let mut instances = self.instances.lock().unwrap();
        let Some(entries) = instances.get_mut(id) else {
            return vec![];
        };
        let mut live = Vec::with_capacity(entries.len());
        entries.retain(|(_, weak)| match weak.upgrade() {
            Some(instance) => {
                live.push(instance);
                true
            },
            None => false,
        });
        if entries.is_empty() {
            instances.remove(id);
        }
        live
    }

    fn media_instance_action(
        &self,
        id: &ClientContextId,
        action: impl Fn(&dyn MediaInstance) -> Result<(), MediaInstanceError>,
    ) {
        for instance in self.live_instances(id) {
            if action(&*instance.lock().unwrap()).is_err() {
                warn!("MPV backend: media instance action failed");
            }
        }
    }
}

/// Creates and destroys a libmpv instance with the options players will use, without
/// opening any output.
fn check_libmpv() -> Result<(), MpvError> {
    let options: Vec<_> = BASE_OPTIONS
        .iter()
        .chain(&[("ao", "null"), ("vid", "no")])
        .copied()
        .collect();
    MpvHandle::new(&options).map(drop)
}

impl BackendInit for MpvBackend {
    fn init() -> Box<dyn Backend> {
        Box::new(MpvBackend::with_fallback(DummyBackend::init()))
    }
}

/// Nothing to release: the instance table only holds weak references.
impl BackendDeInit for MpvBackend {}

impl Backend for MpvBackend {
    fn create_player(
        &self,
        id: &ClientContextId,
        _stream_type: StreamType,
        _sender: GenericCallback<PlayerEvent>,
        _video_renderer: Option<Arc<Mutex<dyn VideoFrameRenderer>>>,
        _audio_renderer: Option<Arc<Mutex<dyn AudioRenderer>>>,
        _gl_context: Box<dyn PlayerGLContext>,
    ) -> Arc<Mutex<dyn Player>> {
        let instance_id = self.next_instance_id.fetch_add(1, Ordering::Relaxed);
        let player = Arc::new(Mutex::new(MpvPlayer::new(
            instance_id,
            *id,
            Arc::downgrade(&self.instances),
        )));
        let instance: Arc<Mutex<dyn MediaInstance>> = player.clone();
        self.instances
            .lock()
            .unwrap()
            .entry(*id)
            .or_default()
            .push((instance_id, Arc::downgrade(&instance)));
        player
    }

    fn can_play_type(&self, _media_type: &str) -> SupportsMediaType {
        // libmpv is not used yet, so nothing can be played.
        SupportsMediaType::No
    }

    fn mute(&self, id: &ClientContextId, val: bool) {
        self.media_instance_action(id, |instance| instance.mute(val));
        self.fallback.mute(id, val);
    }

    fn suspend(&self, id: &ClientContextId) {
        self.media_instance_action(id, |instance| instance.suspend());
        self.fallback.suspend(id);
    }

    fn resume(&self, id: &ClientContextId) {
        self.media_instance_action(id, |instance| instance.resume());
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use servo_media_player::PlayerError;
    use servo_media_player::context::{GlApi, GlContext, NativeDisplay};

    use super::*;

    struct NoGlContext;

    impl PlayerGLContext for NoGlContext {
        fn get_gl_context(&self) -> GlContext {
            GlContext::Unknown
        }
        fn get_native_display(&self) -> NativeDisplay {
            NativeDisplay::Unknown
        }
        fn get_gl_api(&self) -> GlApi {
            GlApi::None
        }
    }

    /// A backend built directly, to test the instance bookkeeping alone.
    fn backend() -> MpvBackend {
        MpvBackend {
            fallback: DummyBackend::init(),
            instances: Default::default(),
            next_instance_id: AtomicUsize::new(0),
        }
    }

    fn create_player(backend: &MpvBackend, context: &ClientContextId) -> Arc<Mutex<dyn Player>> {
        let sender = GenericCallback::new(|_| {}).unwrap();
        backend.create_player(
            context,
            StreamType::Seekable,
            sender,
            None,
            None,
            Box::new(NoGlContext),
        )
    }

    fn registered(backend: &MpvBackend, context: &ClientContextId) -> usize {
        backend
            .instances
            .lock()
            .unwrap()
            .get(context)
            .map_or(0, Vec::len)
    }

    #[test]
    fn players_register_and_unregister_themselves() {
        let backend = backend();
        let context = ClientContextId::build(1, 1);
        let first = create_player(&backend, &context);
        let second = create_player(&backend, &context);
        assert_ne!(
            first.lock().unwrap().get_id(),
            second.lock().unwrap().get_id()
        );
        assert_eq!(registered(&backend, &context), 2);

        drop(first);
        assert_eq!(registered(&backend, &context), 1);
        drop(second);
        assert!(!backend.instances.lock().unwrap().contains_key(&context));
    }

    #[test]
    fn dropping_a_player_during_an_action_does_not_deadlock() {
        let backend = backend();
        let context = ClientContextId::build(1, 1);
        let player = Mutex::new(Some(create_player(&backend, &context)));

        // The action releases the only external reference: the player is then dropped by
        // `media_instance_action` itself, which must not hold the table lock at that point.
        backend.media_instance_action(&context, |_| {
            player.lock().unwrap().take();
            Ok(())
        });
        assert_eq!(registered(&backend, &context), 0);
    }

    #[test]
    fn unimplemented_operations_report_errors() {
        let backend = backend();
        let player = create_player(&backend, &ClientContextId::build(1, 1));
        let player = player.lock().unwrap();
        assert!(matches!(player.play(), Err(PlayerError::Backend(_))));
        assert!(matches!(
            player.push_data(vec![0]),
            Err(PlayerError::Backend(_))
        ));
        assert!(player.paused());
        assert_eq!(player.volume(), 1.0);
    }
}
