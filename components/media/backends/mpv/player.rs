/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::{Cell, OnceCell};
use std::ops::{Deref, Range};
use std::sync::{Arc, Mutex, Weak};

use log::warn;
use servo_base::generic_channel::GenericCallback;
use servo_media::MediaInstanceError;
use servo_media_player::{Player, PlayerError, PlayerEvent};
use servo_media_streams::registry::MediaStreamId;
use servo_media_traits::{ClientContextId, MediaInstance};

use crate::Instances;
use crate::events::EventThread;
use crate::handle::{MpvError, MpvHandle, Node};
use crate::stream::{BridgeObserver, SeekReply, StreamBridge};

const DEFAULT_PAUSED: bool = true;
const DEFAULT_MUTED: bool = false;
const DEFAULT_VOLUME: f64 = 1.0;
const DEFAULT_PLAYBACK_RATE: f64 = 1.0;

/// A media player backed by libmpv.
///
/// Its libmpv instance is only created when first needed, usually when Servo pushes the
/// first bytes of the media, so a media element that never loads costs no mpv threads.
/// Operations that are not wired to libmpv yet report a `PlayerError::Backend` instead of
/// silently succeeding.
pub struct MpvPlayer {
    id: usize,
    context_id: ClientContextId,
    /// The backend's instance table. Weak so a player never keeps its backend alive.
    instances: Weak<Mutex<Instances>>,
    /// Options of the libmpv instance, read when it gets created.
    options: Arc<[(&'static str, &'static str)]>,
    /// Where the events of the libmpv instance go, once it exists.
    sender: GenericCallback<PlayerEvent>,
    /// Where Servo pushes the media, read by libmpv as `aether-media://<id>`.
    bridge: Arc<StreamBridge>,
    /// Whether libmpv was asked to load the media from the bridge.
    loading: Cell<bool>,
    handle: OnceCell<Instance>,
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
        sender: GenericCallback<PlayerEvent>,
    ) -> MpvPlayer {
        MpvPlayer {
            id,
            context_id,
            instances,
            options,
            bridge: Arc::new(StreamBridge::new(Box::new(Notifier {
                sender: sender.clone(),
            }))),
            sender,
            loading: Cell::new(false),
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
        let handle = Arc::new(MpvHandle::new(&self.options).map_err(backend_error)?);
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
        handle
            .add_stream_protocol(self.bridge.clone())
            .map_err(backend_error)?;
        let events = EventThread::spawn(&handle, self.sender.clone()).map_err(backend_error)?;
        Ok(self.handle.get_or_init(|| Instance {
            _events: events,
            handle,
        }))
    }
}

/// Turns the requests of the bridge into `PlayerEvent`s for Servo.
struct Notifier {
    sender: GenericCallback<PlayerEvent>,
}

impl BridgeObserver for Notifier {
    fn need_data(&self) {
        // Fails only once the media element is gone, which drops this player soon.
        let _ = self.sender.send(PlayerEvent::NeedData);
    }

    fn enough_data(&self) {
        let _ = self.sender.send(PlayerEvent::EnoughData);
    }

    fn seek_data(&self, _offset: u64, _bridge: &StreamBridge) -> SeekReply {
        // Not reached: the bridge stays unseekable until seeking is wired (see
        // `set_seekable`), so it answers libmpv's seeks itself.
        SeekReply {
            data_will_come: false,
            ack: Box::new(|| {}),
        }
    }
}

/// A libmpv instance with the thread that forwards its events.
struct Instance {
    /// Only kept for its `Drop`. Declared first so the thread is stopped and joined
    /// before `handle` is released, which destroys the instance.
    _events: EventThread,
    handle: Arc<MpvHandle>,
}

impl Deref for Instance {
    type Target = MpvHandle;

    fn deref(&self) -> &MpvHandle {
        &self.handle
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

    fn set_input_size(&self, size: u64) -> Result<(), PlayerError> {
        self.bridge.set_size(size);
        Ok(())
    }

    fn set_seekable(&self, _seekable: bool) -> Result<(), PlayerError> {
        // Seeking the stream needs Servo to fetch from another offset (`SeekData`), which
        // is not wired yet: until then libmpv sees a stream it cannot seek, which it reads
        // like a pipe, instead of one whose seeks would fail and cancel it.
        Ok(())
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

    fn push_data(&self, data: Vec<u8>) -> Result<(), PlayerError> {
        self.bridge.push(data);
        // libmpv starts with the first bytes: Servo only calls `play` once it has the
        // metadata, which libmpv only reports once it loaded the media.
        if !self.loading.get() {
            self.handle()?
                .command(&["loadfile", &format!("aether-media://{}", self.id)])
                .map_err(backend_error)?;
            self.loading.set(true);
        }
        Ok(())
    }

    fn end_of_stream(&self) -> Result<(), PlayerError> {
        self.bridge.end_of_stream();
        Ok(())
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
        // libmpv's demuxer may be waiting for data that will never come: wake it up
        // before the event thread and the instance are torn down.
        self.bridge.cancel();

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

    use servo_base::generic_channel::GenericReceiver;
    use servo_media_player::PlaybackState;

    use super::*;

    /// A player whose libmpv instance never opens an audio output.
    fn player_with(options: &[(&'static str, &'static str)]) -> MpvPlayer {
        MpvPlayer::new(
            0,
            ClientContextId::build(1, 1),
            Weak::new(),
            Arc::from(options),
            GenericCallback::new(|_| {}).unwrap(),
        )
    }

    /// The real player options, without an audio output.
    fn options() -> Vec<(&'static str, &'static str)> {
        crate::BASE_OPTIONS
            .iter()
            .chain(crate::PLAYER_OPTIONS)
            .chain(&[("ao", "null")])
            .copied()
            .collect()
    }

    fn player() -> MpvPlayer {
        player_with(&options())
    }

    /// A player whose events are received by the test.
    fn player_with_events() -> (MpvPlayer, GenericReceiver<PlayerEvent>) {
        player_with_events_and_options(&options())
    }

    fn player_with_events_and_options(
        options: &[(&'static str, &'static str)],
    ) -> (MpvPlayer, GenericReceiver<PlayerEvent>) {
        let (sender, receiver) = GenericCallback::new_blocking().unwrap();
        let player = MpvPlayer::new(
            0,
            ClientContextId::build(1, 1),
            Weak::new(),
            Arc::from(options),
            sender,
        );
        (player, receiver)
    }

    fn test_file() -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../tests/wpt/tests/media/sine440.mp3"),
        )
        .unwrap()
    }

    /// Pushes a whole file as Servo does: its size, then chunks of it, then its end.
    fn push_file(player: &MpvPlayer, data: &[u8]) {
        player.set_input_size(data.len() as u64).unwrap();
        for chunk in data.chunks(16 * 1024) {
            player.push_data(chunk.to_vec()).unwrap();
        }
        player.end_of_stream().unwrap();
    }

    /// Skips events until one matches, failing the test after a few seconds.
    fn expect_event(
        receiver: &GenericReceiver<PlayerEvent>,
        what: &str,
        matches: impl Fn(&PlayerEvent) -> bool,
    ) -> PlayerEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = receiver
                .try_recv_timeout(remaining)
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
            if matches(&event) {
                return event;
            }
        }
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
    fn loading_a_file_reports_its_metadata_then_its_state() {
        let (player, events) = player_with_events();
        load_test_file(&player);
        let PlayerEvent::MetadataUpdated(metadata) =
            expect_event(&events, "the metadata", |event| {
                matches!(event, PlayerEvent::MetadataUpdated(_))
            })
        else {
            unreachable!()
        };
        assert!(metadata.duration.is_some());
        assert!(!metadata.is_live);
        assert!(metadata.is_seekable);
        assert_eq!(metadata.audio_tracks.len(), 1);
        assert!(metadata.video_tracks.is_empty());
        expect_event(&events, "the paused state", |event| {
            matches!(event, PlayerEvent::StateChanged(PlaybackState::Paused))
        });
    }

    #[test]
    fn playback_reports_its_state_and_position() {
        let (player, events) = player_with_events();
        load_test_file(&player);
        expect_event(&events, "the metadata", |event| {
            matches!(event, PlayerEvent::MetadataUpdated(_))
        });
        player.play().unwrap();
        expect_event(&events, "the playing state", |event| {
            matches!(event, PlayerEvent::StateChanged(PlaybackState::Playing))
        });
        expect_event(
            &events,
            "the position to advance",
            |event| matches!(event, PlayerEvent::PositionChanged(position) if *position > 0.0),
        );
    }

    #[test]
    fn reaching_the_end_reports_end_of_stream_then_paused() {
        let (player, events) = player_with_events();
        let handle = load_test_file(&player);
        let Ok(Node::Double(duration)) = handle.get_node("duration") else {
            panic!("the test file should have a duration");
        };
        handle
            .command(&["seek", &(duration - 0.2).to_string(), "absolute"])
            .unwrap();
        player.play().unwrap();
        expect_event(&events, "the end of stream", |event| {
            matches!(event, PlayerEvent::EndOfStream)
        });
        // `keep-open=yes` pauses at the end instead of unloading the file.
        expect_event(&events, "the paused state", |event| {
            matches!(event, PlayerEvent::StateChanged(PlaybackState::Paused))
        });
    }

    #[test]
    fn a_file_that_cannot_be_played_reports_an_error() {
        let (player, events) = player_with_events();
        player
            .handle()
            .unwrap()
            .command(&["loadfile", "/nonexistent/aether-media-mpv-test.mp3"])
            .unwrap();
        expect_event(&events, "the error", |event| {
            matches!(event, PlayerEvent::Error(_))
        });
    }

    #[test]
    fn libmpv_starts_with_the_first_pushed_bytes() {
        let player = player();
        player.set_input_size(1024).unwrap();
        assert!(player.handle.get().is_none());
        player.push_data(test_file()[..1024].to_vec()).unwrap();
        assert!(player.handle.get().is_some());
    }

    #[test]
    fn pushed_media_is_loaded_and_played() {
        let (player, events) = player_with_events();
        push_file(&player, &test_file());
        let PlayerEvent::MetadataUpdated(metadata) =
            expect_event(&events, "the metadata", |event| {
                matches!(event, PlayerEvent::MetadataUpdated(_))
            })
        else {
            unreachable!()
        };
        assert!(metadata.duration.is_some());
        assert_eq!(metadata.audio_tracks.len(), 1);
        player.play().unwrap();
        expect_event(
            &events,
            "the position to advance",
            |event| matches!(event, PlayerEvent::PositionChanged(position) if *position > 0.0),
        );
    }

    #[test]
    fn pushed_media_plays_to_its_end() {
        // Decode as fast as possible instead of in real time.
        let mut options = options();
        options.push(("ao-null-untimed", "yes"));
        let (player, events) = player_with_events_and_options(&options);
        push_file(&player, &test_file());
        player.play().unwrap();
        expect_event(&events, "the end of stream", |event| {
            matches!(event, PlayerEvent::EndOfStream)
        });
    }

    #[test]
    fn a_player_waiting_for_data_is_dropped_without_hanging() {
        let player = player();
        // Not enough for libmpv to open the media: its demuxer waits for more.
        player.push_data(test_file()[..1024].to_vec()).unwrap();
        thread::sleep(Duration::from_millis(200));

        let (dropped, done) = std::sync::mpsc::channel();
        thread::spawn(move || {
            drop(player);
            dropped.send(()).unwrap();
        });
        done.recv_timeout(Duration::from_secs(5))
            .expect("dropping the player should not wait for more data");
    }

    #[test]
    fn water_marks_are_reported_as_player_events() {
        // Unbounded, unlike `new_blocking`: both events are sent before any is received.
        let (events_sender, events) = std::sync::mpsc::channel();
        let sender = GenericCallback::new(move |event| {
            let _ = events_sender.send(event.unwrap());
        })
        .unwrap();
        let notifier = Notifier { sender };
        notifier.enough_data();
        notifier.need_data();
        let next = || events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(next(), PlayerEvent::EnoughData));
        assert!(matches!(next(), PlayerEvent::NeedData));
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
